//! Original overlay 2 tunnel and floating map-object geometry.
//! Packets contain GP0 payloads only; guest pointers and DMA links are absent.

use crate::mission_background::{PaletteGradient, RingPaletteDepth};
use ac3_graphics::projection::{ProjectionControls, project, project_three};
use ac3_graphics::scalar::ScalarMath;
use ac3_graphics::transform::{SinCosTable, rotate_translate};
use ac3_graphics::{Error, Result, Vram};

pub const MAP_PROJECTION: ProjectionControls = ProjectionControls {
    offset: [160 << 16, 240 << 16],
    distance: 250,
    depth_scale: -11184,
    depth_offset: 0x1555000,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Matrix {
    pub rotation: [[i16; 3]; 3],
    pub translation: [i32; 3],
}

/// Resident CompMatrix: its translation input is explicitly narrowed to SVECTOR.
pub fn compose(left: Matrix, right: Matrix) -> Matrix {
    let columns: [[i16; 3]; 3] = std::array::from_fn(|column| {
        let vector = std::array::from_fn(|row| right.rotation[row][column]);
        rotate_translate(left.rotation, [0; 3], vector).saturated
    });
    let rotation = std::array::from_fn(|row| std::array::from_fn(|column| columns[column][row]));
    let vector =
        rotate_translate(left.rotation, [0; 3], right.translation.map(|v| v as i16)).vector;
    Matrix {
        rotation,
        translation: std::array::from_fn(|i| vector[i].wrapping_add(left.translation[i])),
    }
}

/// Resident RotMatrix_gte performs products in a different order to RotMatrix.
pub fn rotation_gte(trig: &SinCosTable, angles: [i16; 3]) -> [[i16; 3]; 3] {
    let [sx, cx] = trig.sin_cos(angles[0]);
    let [sy, cy] = trig.sin_cos(angles[1]);
    let [sz, cz] = trig.sin_cos(angles[2]);
    let mul = |a: i32, b: i32| a.wrapping_mul(b) >> 12;
    let sycx = mul(sy, cx);
    let sysx = mul(sy, sx);
    [
        [mul(cz, cy), -mul(sz, cy), sy],
        [
            mul(cz, sysx) + mul(sz, cx),
            mul(cz, cx) - mul(sz, sysx),
            -mul(cy, sx),
        ],
        [
            mul(sz, sx) - mul(cz, sycx),
            mul(cz, sx) + mul(sz, sycx),
            mul(cy, cx),
        ],
    ]
    .map(|row| row.map(|v| v as i16))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texture {
    pub origin: [i16; 2],
    pub clut: [u16; 2],
}
impl Texture {
    /// The original static RECT-pair descriptor (pixel RECT, then palette RECT).
    pub fn from_descriptor(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 16 {
            return Err(Error::Truncated("map texture descriptor"));
        }
        let half = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        Ok(Self {
            origin: [half(0) as i16, half(2) as i16],
            clut: [half(8), half(10)],
        })
    }
    fn tpage(self) -> u32 {
        let [x, y] = self.origin.map(|v| v as u16 as u32);
        ((y & 256) >> 4) | ((x & 1023) >> 6) | ((y & 512) << 2)
    }
    fn clut(self) -> u32 {
        (u32::from(self.clut[1]) << 6) | ((u32::from(self.clut[0]) >> 4) & 63)
    }
    fn u(self) -> i32 {
        i32::from(self.origin[0] % 64) * 4
    }
    fn v(self) -> i32 {
        i32::from(self.origin[1] % 256)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapState {
    pub frame: u32,
    pub tube_phase: i32,
    pub object_phase: i32,
    pub tube_progress: i32,
    pub object_progress: i32,
    pub world: Matrix,
    pub camera_angles: [i16; 3],
    pub angle_counter: i16,
    pub speed: i16,
    pub fraction: i16,
    pub step: i16,
    pub ring_phase: u32,
    pub angles: [[i16; 3]; 64],
    pub translations: [[i32; 3]; 64],
    pub vertices: [[i16; 3]; 16],
    pub object_indices: [i16; 48],
    pub objects: [[i32; 3]; 48],
}

/// Original resident rand/srand state; share this with other translated callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Random {
    pub state: u32,
}
impl Random {
    pub fn next_value(&mut self) -> i32 {
        self.state = self.state.wrapping_mul(0x41c64e6d).wrapping_add(0x3039);
        ((self.state >> 16) & 32767) as i32
    }
}

/// ApplyMatrixLV splits signed long coordinates before the two original products.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketKind {
    Near,
    Far,
    Object,
    Palette,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub bucket: u8,
    pub kind: PacketKind,
    pub words: Vec<u32>,
}
/// Emission order is preserved. Original addPrim prepends within each depth bucket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DepthPackets {
    pub emitted: Vec<Packet>,
    /// Six near-ring samples in emission order, before OT sorting.
    pub gradient_depths: Vec<RingPaletteDepth>,
}
impl DepthPackets {
    pub fn execution_order(&self) -> Vec<&Packet> {
        let mut result = Vec::with_capacity(self.emitted.len());
        for bucket in (0..=255).rev() {
            result.extend(
                self.emitted
                    .iter()
                    .rev()
                    .filter(|p| usize::from(p.bucket) == bucket),
            );
        }
        result
    }
    fn add(&mut self, bucket: u16, kind: PacketKind, words: Vec<u32>) {
        self.emitted.push(Packet {
            bucket: bucket.min(255) as u8,
            kind,
            words,
        });
    }
    fn append(&mut self, other: Self) {
        self.emitted.extend(other.emitted);
        self.gradient_depths.extend(other.gradient_depths);
    }
}

/// Native asset-backed map lifecycle. No runtime dependency on capture fixtures.
pub struct MapRenderer {
    pub state: MapState,
    pub tube_texture: Texture,
    pub object_texture: Texture,
    pub palette: PaletteGradient,
    /// EA598/EAE74 is a distinct alternate backdrop, not silently approximated.
    pub fallback_phase: i32,
    trig: SinCosTable,
    math: ScalarMath,
}
impl MapRenderer {
    /// Inputs are verified PAL resident/overlay-2 payloads and mission-loaded VRAM.
    /// Only the tunnel/object initializer is owned here (288 PRNG draws).
    pub fn new(resident: &[u8], overlay: &[u8], vram: &Vram, random: &mut Random) -> Result<Self> {
        let trig_bytes = resident
            .get(0xa4f84..0xa8f84)
            .ok_or(Error::Truncated("resident trig table"))?;
        let descriptors = overlay
            .get(0x2ca40..0x2ca60)
            .ok_or(Error::Truncated("overlay map textures"))?;
        let trig = SinCosTable::from_le_bytes(trig_bytes)?;
        let math = ScalarMath::from_resident(resident, 0x80010000)?;
        let tube_texture = Texture::from_descriptor(&descriptors[..16])?;
        let object_texture = Texture::from_descriptor(&descriptors[16..])?;
        let palette = PaletteGradient::from_vram(vram, tube_texture.clut)?;
        Ok(Self {
            state: MapState::new(&math, random),
            tube_texture,
            object_texture,
            palette,
            fallback_phase: 0,
            trig,
            math,
        })
    }
    /// State-only part of E7B50/E7CE8; audio effects belong to the scene owner.
    pub fn set_tube_phase(&mut self, phase: i32) -> Result<()> {
        if !(0..=3).contains(&phase) {
            return Err(Error::Invalid("map tube phase"));
        }
        self.state.tube_phase = phase;
        self.state.tube_progress = 0;
        Ok(())
    }
    /// State-only part of E97D4/E9824.
    pub fn set_object_phase(&mut self, phase: i32) -> Result<()> {
        if !(0..=3).contains(&phase) {
            return Err(Error::Invalid("map object phase"));
        }
        self.state.object_phase = phase;
        self.state.object_progress = 0;
        Ok(())
    }
    /// EB014: update and render once per original callback, including inactive ticks.
    pub fn tick(&mut self, far_rgb: [u8; 3]) -> Result<DepthPackets> {
        if self.state.tube_phase == 0 && self.state.object_phase == 0 && self.fallback_phase != 0 {
            return Err(Error::Unsupported("alternate map triangle backdrop"));
        }
        let mut packets = DepthPackets::default();
        if self.state.tube_phase != 0 {
            self.state.update(&self.trig, &self.math)?;
            packets.append(self.state.draw_near(
                &self.trig,
                self.tube_texture,
                &self.palette,
                far_rgb,
            )?);
            packets.append(self.state.draw_far(&self.trig, self.tube_texture));
            self.state.finish_tube();
        }
        if self.state.object_phase != 0 {
            packets.append(
                self.state
                    .draw_objects(&self.trig, self.object_texture, far_rgb)?,
            );
        }
        self.state.frame = self.state.frame.wrapping_add(1);
        Ok(packets)
    }
}

#[derive(Clone, Copy)]
struct Ring {
    screen: [[i16; 2]; 16],
    depth: [u16; 16],
    first_ir0: u16,
}
fn ring(trig: &SinCosTable, state: &MapState, index: usize) -> Ring {
    let matrix = compose(
        state.world,
        Matrix {
            rotation: rotation_gte(trig, state.angles[index]),
            translation: state.translations[index],
        },
    );
    let mut output = Ring {
        screen: [[0; 2]; 16],
        depth: [0; 16],
        first_ir0: 0,
    };
    for start in (0..15).step_by(3) {
        let result = project_three(
            matrix.rotation,
            matrix.translation,
            MAP_PROJECTION,
            std::array::from_fn(|i| state.vertices[start + i]),
        );
        output.screen[start..start + 3].copy_from_slice(&result.screen);
        output.depth[start..start + 3].copy_from_slice(&result.depth);
        if start == 0 {
            output.first_ir0 = result.ir0;
        }
    }
    let result = project(
        matrix.rotation,
        matrix.translation,
        MAP_PROJECTION,
        state.vertices[15],
    );
    output.screen[15] = result.screen[0];
    output.depth[15] = result.depth[0];
    output
}
fn visible(screen: [[i16; 2]; 4]) -> bool {
    screen.iter().any(|p| p[0] >= 0)
        && screen.iter().any(|p| p[1] >= 0)
        && screen.iter().any(|p| p[0] < 321)
        && screen.iter().any(|p| p[1] < 513)
}
fn xy(p: [i16; 2]) -> u32 {
    u32::from(p[0] as u16) | (u32::from(p[1] as u16) << 16)
}
fn area(a: [i16; 2], b: [i16; 2], c: [i16; 2]) -> i32 {
    let [ax, ay] = a.map(i32::from);
    let [bx, by] = b.map(i32::from);
    let [cx, cy] = c.map(i32::from);
    ax * (by - cy) + bx * (cy - ay) + cx * (ay - by)
}
fn quad(screen: [[i16; 2]; 4], texture: Texture, v: i32, height: i32) -> Vec<u32> {
    let u = texture.u() as u32;
    let top = (v as u32) << 8;
    let bottom = ((v + height) as u32) << 8;
    vec![
        0x2c838383,
        xy(screen[0]),
        u | top | (texture.clut() << 16),
        xy(screen[1]),
        (u + 31) | top | (texture.tpage() << 16),
        xy(screen[2]),
        u | bottom,
        xy(screen[3]),
        (u + 31) | bottom,
    ]
}

impl MapState {
    /// EAF0C/E7E58/E986C geometry, using the caller's live original PRNG state.
    /// The alternate triangle backdrop allocated by EA598 is a separate renderer.
    pub fn new(math: &ScalarMath, random: &mut Random) -> Self {
        let mut state = Self {
            frame: 0,
            tube_phase: 0,
            object_phase: 0,
            tube_progress: 0,
            object_progress: 0,
            world: Matrix::default(),
            camera_angles: [0; 3],
            angle_counter: 0,
            speed: 256,
            fraction: 0,
            step: 0,
            ring_phase: 0,
            angles: [[0; 3]; 64],
            translations: std::array::from_fn(|i| [0, 0, 0x5da8 - (i as i32 * 3125 / 8)]),
            vertices: std::array::from_fn(|i| {
                [
                    (math.cos((i as i32) << 8) / 2) as i16,
                    (math.sin((i as i32) << 8) / 2) as i16,
                    0,
                ]
            }),
            object_indices: [0; 48],
            objects: [[0; 3]; 48],
        };
        state.initialize_objects(math, random);
        state
    }
    pub fn initialize_objects(&mut self, math: &ScalarMath, random: &mut Random) {
        for i in 0..48 {
            self.object_indices[i] = (random.next_value() % 64) as i16;
            let angle = random.next_value() & 4095;
            let x = math.cos(angle) * (random.next_value() % 1536) / 4096;
            let angle = random.next_value() & 4095;
            let y = math.sin(angle) * (random.next_value() % 1536) / 4096;
            let z = random.next_value() % 1562;
            self.objects[i] = [
                x + if x < 0 { -384 } else { 384 },
                y + if y < 0 { -384 } else { 384 },
                z,
            ];
        }
    }
    /// E80D8, including the original one-ring shift when `step` equals two.
    pub fn update(&mut self, trig: &SinCosTable, math: &ScalarMath) -> Result<()> {
        self.speed = if self.tube_phase == 1 || self.tube_phase == 3 {
            512
        } else {
            (((self.frame & 511) as i32 - 256).abs() + 128) as i16
        };
        let total = self.fraction.wrapping_add(self.speed);
        self.step = total / 390;
        self.fraction = total % 390;
        if !(0..64).contains(&self.step) {
            return Err(Error::Unsupported("map update step outside natural range"));
        }
        if self.step != 0 {
            self.ring_phase = self.ring_phase.wrapping_add(self.step as u32) & 7;
            if self.tube_phase == 1 || self.tube_phase == 3 {
                self.tube_progress = self.tube_progress.wrapping_add(i32::from(self.step));
            }
            if self.object_phase == 1 || self.object_phase == 3 {
                self.object_progress = self.object_progress.wrapping_add(i32::from(self.step));
            }
            let origin = self.translations[62];
            for i in (self.step as usize..64).rev() {
                self.angles[i] = self.angles[i - 1];
                self.translations[i] =
                    std::array::from_fn(|j| self.translations[i - 1][j].wrapping_sub(origin[j]));
            }
            let mut counter = i32::from(self.angle_counter);
            for i in (0..self.step as usize).rev() {
                counter += 1;
                let sine = math.sin(counter << 4);
                self.angles[i] = [(sine >> 6) as i16, (sine >> 5) as i16, 0];
                self.translations[i] = rotate_translate(
                    rotation_gte(trig, self.angles[i + 1]),
                    self.translations[i + 1],
                    [0, 0, 390],
                )
                .vector;
            }
            self.angle_counter = counter as i16;
        }
        let interpolation = i32::from(self.fraction) * 4096 / 390;
        let position: [i32; 3] = std::array::from_fn(|i| {
            let difference = self.translations[62][i].wrapping_sub(self.translations[63][i]) as i16;
            (interpolation * i32::from(difference)) >> 12
        });
        let target: [i32; 3] = std::array::from_fn(|i| {
            let start = i32::from(self.translations[42][i] as i16);
            let difference = self.translations[43][i]
                .wrapping_sub(start)
                .clamp(-32768, 32767);
            ((i64::from(start) << 12) + i64::from(interpolation) * i64::from(difference)) as i32
                >> 12
        });
        // Original MFC2/subu reads target.z - position.z; decompiled C loses this.
        let forward = target[2].wrapping_sub(position[2]);
        self.camera_angles = [
            math.atan2(target[1].wrapping_sub(position[1]), forward)? as i16,
            math.atan2(target[0].wrapping_sub(position[0]), forward)? as i16,
            0,
        ];
        let camera = trig.rotation(self.camera_angles);
        self.world.rotation = std::array::from_fn(|r| std::array::from_fn(|c| camera[c][r]));
        self.world.translation = apply_long(self.world.rotation, position.map(i32::wrapping_neg));
        Ok(())
    }
    /// Completion after both tube drawing calls, as in E9748.
    pub fn finish_tube(&mut self) {
        if self.tube_progress / 8 > 11 && (self.tube_phase == 1 || self.tube_phase == 3) {
            self.tube_progress = 0;
            self.tube_phase = if self.tube_phase == 1 { 2 } else { 0 };
        }
    }
    /// 800E8574. The shared vertex Z values are stateful during reveal/exit.
    pub fn draw_near(
        &mut self,
        trig: &SinCosTable,
        texture: Texture,
        palette: &PaletteGradient,
        far_rgb: [u8; 3],
    ) -> Result<DepthPackets> {
        let mut packets = DepthPackets::default();
        let mut previous = None;
        let v = texture.v() - ((self.ring_phase & 7) as i32 * 4 - 32);
        for segment in 0..7 {
            let progress = self.tube_progress / 8;
            if progress == segment - 1 {
                let z = if self.tube_phase == 1 {
                    (7 - self.tube_progress % 8) * 25000 / 64
                } else {
                    0
                };
                for vertex in &mut self.vertices {
                    vertex[2] = z as i16;
                }
            } else if progress == segment {
                let z = if self.tube_phase == 3 {
                    (-self.tube_progress % 8) * 25000 / 64
                } else {
                    0
                };
                for vertex in &mut self.vertices {
                    vertex[2] = z as i16;
                }
            }
            let current = ring(trig, self, segment as usize * 8);
            if let Some(prev) = previous {
                let prev: Ring = prev;
                if !((self.tube_phase == 1 && progress < segment - 1)
                    || (self.tube_phase == 3 && segment - 1 < progress))
                {
                    for i in 0..16 {
                        let next = (i + 1) & 15;
                        let screen = [
                            prev.screen[i],
                            prev.screen[next],
                            current.screen[i],
                            current.screen[next],
                        ];
                        if visible(screen) && area(screen[0], screen[2], screen[3]) >= 0 {
                            packets.add(
                                prev.depth[i] >> 8,
                                PacketKind::Near,
                                quad(screen, texture, v, 31),
                            );
                        }
                    }
                }
                let depth = RingPaletteDepth {
                    depth_cue: current.first_ir0,
                    previous_ring_first_depth: prev.depth[0],
                };
                let upload = palette.upload(far_rgb, depth)?;
                packets.gradient_depths.push(depth);
                packets.add(
                    u16::from(upload.ot_bucket),
                    PacketKind::Palette,
                    upload.words.to_vec(),
                );
            }
            previous = Some(current);
        }
        Ok(packets)
    }
    /// 800E9060 connects the remaining sixteen rings without backface culling.
    pub fn draw_far(&self, trig: &SinCosTable, texture: Texture) -> DepthPackets {
        let mut packets = DepthPackets::default();
        let mut previous = None;
        for segment in 0..16 {
            let current = ring(trig, self, 48 + segment);
            if let Some(prev) = previous {
                let prev: Ring = prev;
                if !((self.tube_phase == 1 && self.tube_progress - 48 < segment as i32)
                    || (self.tube_phase == 3 && (segment as i32) < self.tube_progress - 48))
                {
                    let v = texture.v()
                        + ((((segment - 1) & 7) as i32 * 4 - (self.ring_phase & 7) as i32 * 4)
                            & 31);
                    for i in 0..16 {
                        let next = (i + 1) & 15;
                        let screen = [
                            prev.screen[i],
                            prev.screen[next],
                            current.screen[i],
                            current.screen[next],
                        ];
                        if visible(screen) {
                            packets.add(
                                prev.depth[i] >> 8,
                                PacketKind::Far,
                                quad(screen, texture, v, 7),
                            );
                        }
                    }
                }
            }
            previous = Some(current);
        }
        packets
    }
    /// 800E9A54: CPU RotMatrix for objects, projection, DPCS and billboard quad.
    /// The two unused UV high halves are normalized to zero, unlike reused guest RAM.
    pub fn draw_objects(
        &mut self,
        trig: &SinCosTable,
        texture: Texture,
        far_rgb: [u8; 3],
    ) -> Result<DepthPackets> {
        if self.step != 0 {
            for index in &mut self.object_indices {
                let value = i32::from(*index) + i32::from(self.step);
                *index = (if value > 61 { value % 62 } else { value }) as i16;
            }
        }
        let mut packets = DepthPackets::default();
        for i in 0..48 {
            let index = self.object_indices[i];
            if (self.object_phase == 1 && i32::from(index) > self.object_progress)
                || (self.object_phase == 3 && self.object_progress > i32::from(index))
            {
                continue;
            }
            let index = usize::try_from(index)
                .ok()
                .filter(|&v| v < 64)
                .ok_or(Error::Invalid("map object ring index"))?;
            let object = Matrix {
                rotation: trig.rotation(self.camera_angles),
                translation: self.objects[i],
            };
            let ring = Matrix {
                rotation: trig.rotation(self.angles[index]),
                translation: self.translations[index],
            };
            let view = compose(self.world, compose(ring, object));
            let output = project(view.rotation, view.translation, MAP_PROJECTION, [0; 3]);
            let depth = output.depth[0];
            if depth == 0 {
                continue;
            }
            let size = (64000 / i32::from(depth)).max(2);
            let left = i32::from(output.screen[0][0]) - (size >> 2);
            let right = left + (size >> 1);
            let top = i32::from(output.screen[0][1]) - (size >> 1);
            let bottom = top + size;
            if right < 0 || left >= 321 || bottom < 0 || top >= 513 {
                continue;
            }
            let color = far_rgb
                .map(|far| ((180 << 12) + i32::from(output.ir0) * (i32::from(far) - 180)) >> 12);
            let command =
                0x2c000000 | color[0] as u32 | ((color[1] as u32) << 8) | ((color[2] as u32) << 16);
            let u = texture.u() as u32;
            let v = texture.v() as u32;
            // Original SW combines the signed X directly; negative X sets high bits too.
            let coord = |x: i32, y: i32| (x as u32) | ((y as u32) << 16);
            let words = vec![
                command,
                coord(left, top),
                u | (v << 8) | (texture.clut() << 16),
                coord(right, top),
                (u + 15) | (v << 8) | (texture.tpage() << 16),
                coord(left, bottom),
                (u | ((v + 15) << 8)) & 65535,
                coord(right, bottom),
                ((u + 15) | ((v + 15) << 8)) & 65535,
            ];
            packets.add(depth >> 8, PacketKind::Object, words);
        }
        if self.object_progress > 63 && (self.object_phase == 1 || self.object_phase == 3) {
            self.object_progress = 0;
            self.object_phase = if self.object_phase == 1 { 2 } else { 0 };
        }
        Ok(packets)
    }
}
