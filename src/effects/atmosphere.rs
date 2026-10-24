//! Atmosphere (Phase 4 item 5): the Japanese release's terrain fog removed
//! by a palette (CLUT) redirect, and a height haze in its place. The study
//! behind every rule and number is `docs/EFFECTS_ATMOSPHERE.md`.
//!
//! Everything here is a pure function of values the GPU renderer already
//! has or reads at a frame's DrawOTag: a primitive's palette position and
//! ordering-table slot, the sky phase's polygons, a few RAM words. Nothing
//! here touches the machine, the renderer or the settings, so it is
//! display-only by construction. `haze.wgsl` (next to this file) is
//! [`Haze::shade`] for the post pass.
//!
//! ## The game's fog in brief (JP 800943A8, 8005EBB8)
//! - At mission load (8005EBB8) the game reads the terrain palettes, the
//!   16x64 block at VRAM (320,256) ([`FOG_PALETTES`]), and stores 16 copies
//!   interpolated toward the fog colour ([`fog_palette_color`]) by
//!   [`FOG_LEVELS`]: even steps as 16x64 blocks at (320..383, 384..511),
//!   odd steps as 64x16 rows at (0..255, 240..255) and (0..255, 496..511)
//!   ([`strip_source`]). Step 0 is the unfogged palette, unchanged:
//!   [`UNFOGGED_PALETTES`].
//! - Every flight frame (800943A8) links 17 "strips" into the world
//!   ordering table, one per depth bucket ([`bucket_slots`]). Each copies
//!   its step into (320,256) just before the terrain of its bucket draws;
//!   strip 16, in the sky slot, fills the block with the fog colour. Terrain
//!   in slot `s` therefore reads the palette of strip [`strip_for_slot`].
//! - The renderer decodes palettes from the 1x software VRAM, so the fog is
//!   removed by reading terrain palettes from step 0 instead
//!   ([`clut_redirect`]); the game's VRAM is never touched.
use std::f32::consts::PI;

// ---- addresses (Japanese release) ----------------------------------------------------

/// RAM words of the Japanese release this module's inputs come from (all
/// verified against `crates/ac3-flight-jp` and a GPU trace of mission 1).
pub mod jp {
    /// The small-data base.
    pub const GP: u32 = 0x800b_cb74;
    /// `[gp+0x2E0]`: the world (gameplay) block.
    pub const WORLD: u32 = GP + 0x2e0;
    /// `[gp+0x2EC]`: the state context; its word +0x2C is the state (2 is
    /// flight).
    pub const CONTEXT: u32 = GP + 0x2ec;
    /// World +0: flags. Bit 0x200 set: no fog strips this frame.
    pub const WORLD_FLAGS: u32 = 0;
    pub const NO_FOG: u32 = 0x200;
    /// World +0x28: the camera record (+4..+0xC position 20.12, +0x24 H,
    /// +0x28/+0x2A the depth-cue near and far).
    pub const CAMERA: u32 = 0x28;
    /// World +0x4C: the view rotation (world to view, 4.12, row-major).
    pub const VIEW: u32 = 0x4c;
    /// World +0x6C: the light direction, toward the sun (4.12, world).
    pub const SUN: u32 = 0x6c;
    /// World +0x8C: the light colour matrix (4.12; column 0 is the sun).
    pub const LIGHT_COLOR: u32 = 0x8c;
    /// World +0x244: mission section 0 (+8 the fog colour, RGB bytes;
    /// +0xC/+0xE the fog's near and far depth as halfwords).
    pub const MISSION: u32 = 0x244;
    pub const FOG_COLOR: u32 = 8;
    pub const FOG_NEAR: u32 = 0xc;
    pub const FOG_FAR: u32 = 0xe;
    /// World +0x288: the 17 depth-bucket halfwords of the strips.
    pub const FOG_BUCKETS: u32 = 0x288;
    /// The RECT the palette builder 8005EBB8 reads: (320, 256, 16, 64).
    pub const PALETTE_SOURCE: u32 = 0x800b_cdcc;
    /// The rsin table (halfwords, angles 0..=0x400) behind [`super::FOG_LEVELS`].
    pub const RSIN_TABLE: u32 = 0x800b_65c4;
    /// The fog strips, the palette builder, and the depth-cue set-up.
    pub const FOG_STRIPS_ROUTINE: u32 = 0x8009_43a8;
    pub const PALETTE_BUILDER: u32 = 0x8005_ebb8;
    pub const DEPTH_CUE_ROUTINE: u32 = 0x8005_e7ec;
}

// ---- the CLUT fog -------------------------------------------------------------------

/// A rectangle of VRAM, in 1x pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VramRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl VramRect {
    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// The terrain palettes the strips rewrite: 64 palettes of 16 colours.
pub const FOG_PALETTES: VramRect = VramRect {
    x: 320,
    y: 256,
    w: 16,
    h: 64,
};

/// Where the unfogged terrain palettes stay for the whole mission: step 0
/// of the palette builder, the same 16x64 layout as [`FOG_PALETTES`].
pub const UNFOGGED_PALETTES: [u16; 2] = [320, 384];

/// Strips per frame: 16 palette steps and the fog-colour fill.
pub const STRIPS: usize = 17;

/// The fog level of palette step `i` (4.12): `rsin(64 i)`, so
/// `4096 sin(pi i / 32)`. Strip 16 (the fill) is 4096.
pub const FOG_LEVELS: [u16; 16] = [
    0, 401, 799, 1189, 1567, 1931, 2276, 2598, 2896, 3166, 3406, 3612, 3784, 3920, 4017, 4076,
];

/// The fog level of strip `strip` (0..=16), 4.12.
pub fn strip_level(strip: usize) -> u16 {
    FOG_LEVELS.get(strip).copied().unwrap_or(4096)
}

/// What a strip puts into [`FOG_PALETTES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripSource {
    /// One GP0 80 copy of the 16x64 block at (x, y).
    Block { x: u16, y: u16 },
    /// Four GP0 80 copies of 16x16 from the 64x16 row at (x, y): source
    /// (x + 16k, y) to (320, 256 + 16k).
    Rows { x: u16, y: u16 },
    /// A TILE of the fog colour (drawn with the mask bit set).
    Fill,
}

/// The source of strip `strip` (800943A8): even strips copy a block from
/// (320 + 16 (i/2 % 4), 384 + 64 (i / 8)), odd strips four 16x16 pieces of
/// the row at (64 ((i-1)/2 % 4), 240 + 256 (i / 8)), strip 16 fills.
pub fn strip_source(strip: usize) -> StripSource {
    let i = strip as u16;
    if strip >= 16 {
        StripSource::Fill
    } else if i & 1 == 0 {
        StripSource::Block {
            x: ((i & 6) << 3) + 0x140,
            y: ((i & 8) << 3) + 0x180,
        }
    } else {
        StripSource::Rows {
            x: (i & 6) << 5,
            y: ((i & 8) << 5) + 0xf0,
        }
    }
}

/// The ordering-table slots of the 17 strips (8005EAB8, then the `+4` OT
/// base of 800943A8): `((i + 1)(far - near) / 16 + near) >> 3` (limited to
/// 0..=0xFFD) plus one, and 4095 (the sky slot) for strip 16. `near` and
/// `far` are mission section 0's halfwords +0xC/+0xE.
pub fn bucket_slots(near: i16, far: i16) -> [u16; STRIPS] {
    let (near, far) = (i32::from(near), i32::from(far));
    let mut slots = [0u16; STRIPS];
    for (index, slot) in slots.iter_mut().enumerate().take(16) {
        let product = (index as i32 + 1).wrapping_mul(far.wrapping_sub(near));
        // `sra 4` after adding 15 to a negative product: division toward 0.
        let quotient = if product < 0 {
            (product + 15) >> 4
        } else {
            product >> 4
        };
        let bucket = ((quotient + near) >> 3).clamp(0, 0xffd);
        *slot = bucket as u16 + 1;
    }
    slots[16] = 0xffe + 1;
    slots
}

/// The strip whose palette a primitive in ordering-table slot `slot` reads:
/// the walk runs from slot 4095 down and each strip comes first in its
/// slot, so it is the lowest strip whose slot is at or above `slot`.
/// `slots` are the strips' slots ([`bucket_slots`] or the RAM table + 1).
pub fn strip_for_slot(slot: u16, slots: &[u16; STRIPS]) -> usize {
    (0..STRIPS)
        .filter(|&strip| slots[strip] >= slot)
        .min_by_key(|&strip| (slots[strip], strip))
        .unwrap_or(STRIPS - 1)
}

/// One palette colour of fog level `level` (4.12): the palette builder's
/// INTPL (8005EB20) of the RGB555 `source` toward `fog` (RGB bytes), sf=1
/// lm=0, keeping the STP bit; colour 0 (transparent) stays 0.
pub fn fog_palette_color(source: u16, fog: [u8; 3], level: u16) -> u16 {
    if source == 0 {
        return 0;
    }
    let level = i64::from(level);
    let mut out = 0u16;
    for (channel, &far) in fog.iter().enumerate() {
        let shift = 5 * channel as u16;
        let ir = i64::from((source >> shift) & 0x1f) << 7;
        let fc = i64::from(far) << 4;
        // IR = lim_b((FC << 12 - IR << 12) >> 12); MAC = (IR * IR0 + IR << 12) >> 12.
        let difference = (fc - ir).clamp(-0x8000, 0x7fff);
        let mac = (difference * level + (ir << 12)) >> 12;
        let ir = mac.clamp(-0x8000, 0x7fff);
        let value = (ir >> 7).clamp(0, 0x1f) as u16;
        out |= value << shift;
    }
    out | (source & 0x8000)
}

// ---- the redirect -------------------------------------------------------------------

/// Where in a flight frame a primitive draws, by the world ordering-table
/// slot it is linked under (4096 slots, walked from 4095 down).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Slot 4095: the sky, then the fog strip 16 fill.
    Sky,
    /// Slots 4094..=2: terrain, the fog strips 0..=15, objects.
    World,
    /// Slot 1: the lens flare.
    Flare,
    /// Slot 0: the HUD.
    Hud,
    /// Not in the world ordering table (menus, movies, the frame's own
    /// environment packets).
    Other,
}

/// The phase of a world ordering-table slot (`None`: outside the table).
pub fn phase_of_slot(slot: Option<u16>) -> Phase {
    match slot {
        Some(4095) => Phase::Sky,
        Some(2..=4094) => Phase::World,
        Some(1) => Phase::Flare,
        Some(0) => Phase::Hud,
        _ => Phase::Other,
    }
}

/// The palette position of a GP0 CLUT half-word: (16 (word & 0x3F),
/// (word >> 6) & 0x1FF).
pub fn clut_position(word: u16) -> [u16; 2] {
    [(word & 0x3f) * 16, (word >> 6) & 0x1ff]
}

/// The GP0 CLUT half-word of palette position (x, y) (x a multiple of 16).
pub fn clut_word(position: [u16; 2]) -> u16 {
    ((position[0] / 16) & 0x3f) | ((position[1] & 0x1ff) << 6)
}

/// **The redirect rule.** A palette read of `entries` colours (16 for
/// 4-bit texels, 256 for 8-bit) at `clut` by a primitive in `phase`:
/// `Some(position)` when it must read `position` instead to draw unfogged.
///
/// Only world-phase reads that lie wholly inside [`FOG_PALETTES`] are
/// redirected, to the same palette of step 0 (y + 128). Elsewhere the block
/// already holds step 0 (after strip 0) or holds what the game means (the
/// fill, in the sky slot, is read by nothing), and other palettes are never
/// fogged by the strips. Animated palette entries (mission section 0
/// +0x74 > 0; none in mission 1) read their base colour (see the study).
pub fn clut_redirect(clut: [u16; 2], entries: u16, phase: Phase) -> Option<[u16; 2]> {
    let [x, y] = clut;
    let area = FOG_PALETTES;
    let inside =
        area.contains(x, y) && u32::from(x) + u32::from(entries) <= u32::from(area.x + area.w);
    (phase == Phase::World && inside).then(|| {
        [
            x - area.x + UNFOGGED_PALETTES[0],
            y - area.y + UNFOGGED_PALETTES[1],
        ]
    })
}

/// [`clut_redirect`] on a GP0 CLUT half-word.
pub fn clut_word_redirect(word: u16, entries: u16, phase: Phase) -> Option<u16> {
    clut_redirect(clut_position(word), entries, phase).map(clut_word)
}

// ---- the view -----------------------------------------------------------------------

/// The flight camera as the GTE sees it: world-to-view rotation `rotation`
/// (rows), projection distance `h` (screen pixels), the camera position in
/// world units (about 5 per metre; y down). View space: x right, y down, z
/// forward; a view point (x, y, z) projects to screen (x h / z, y h / z)
/// relative to the projection centre.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub rotation: [[f32; 3]; 3],
    pub h: f32,
    pub position: [f32; 3],
}

impl View {
    /// From the RAM words: the 4.12 matrix at world +0x4C, H (camera
    /// +0x24) and the camera position (camera +4..+0xC, 20.12).
    pub fn from_gte(matrix: [i16; 9], h: i16, position: [i32; 3]) -> Self {
        let mut rotation = [[0.0; 3]; 3];
        for (index, &value) in matrix.iter().enumerate() {
            rotation[index / 3][index % 3] = f32::from(value) / 4096.0;
        }
        Self {
            rotation,
            h: f32::from(h),
            position: position.map(|value| value as f32 / 4096.0),
        }
    }

    /// A view-space vector in world space (the transpose rotation).
    pub fn to_world(&self, v: [f32; 3]) -> [f32; 3] {
        let r = &self.rotation;
        [
            r[0][0] * v[0] + r[1][0] * v[1] + r[2][0] * v[2],
            r[0][1] * v[0] + r[1][1] * v[1] + r[2][1] * v[2],
            r[0][2] * v[0] + r[1][2] * v[1] + r[2][2] * v[2],
        ]
    }

    /// A world-space vector in view space.
    pub fn to_view(&self, v: [f32; 3]) -> [f32; 3] {
        let r = &self.rotation;
        [
            r[0][0] * v[0] + r[0][1] * v[1] + r[0][2] * v[2],
            r[1][0] * v[0] + r[1][1] * v[1] + r[1][2] * v[2],
            r[2][0] * v[0] + r[2][1] * v[1] + r[2][2] * v[2],
        ]
    }

    /// The world direction (unit) through screen point (`sx`, `sy`),
    /// relative to the projection centre.
    pub fn direction(&self, sx: f32, sy: f32) -> [f32; 3] {
        normalize(self.to_world([sx, sy, self.h]))
    }

    /// The camera-to-point vector (world) of screen point (`sx`, `sy`) at
    /// view depth `z`.
    pub fn offset(&self, sx: f32, sy: f32, z: f32) -> [f32; 3] {
        self.to_world([sx * z / self.h, sy * z / self.h, z])
    }
}

/// The elevation (radians, positive up) of a world direction (y down).
pub fn elevation(direction: [f32; 3]) -> f32 {
    let horizontal = (direction[0] * direction[0] + direction[2] * direction[2]).sqrt();
    (-direction[1]).atan2(horizontal)
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length > 0.0 {
        v.map(|c| c / length)
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

// ---- the sky model --------------------------------------------------------------------

/// A vertex of a sky polygon: the screen position relative to the
/// projection centre (the GP0 vertex word; the GTE's OFX/OFY are 0 in
/// flight) and its 24-bit colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyVertex {
    pub x: f32,
    pub y: f32,
    pub color: [u8; 3],
}

/// An untextured sky polygon (POLY_F3/F4/G3/G4) of the sky slot.
#[derive(Clone, Debug, PartialEq)]
pub struct SkyPolygon {
    /// Three or four vertices, in GP0 order (a quad is (0,1,2) and (1,3,2)).
    pub vertices: Vec<SkyVertex>,
}

/// The untextured, opaque polygon of a GP0 command (`words` from its
/// command word), as the sky phase draws its bands; `None` for anything
/// else (the sun strip, stars, the fog fill).
pub fn sky_polygon(words: &[u32]) -> Option<SkyPolygon> {
    let command = *words.first()?;
    let op = command >> 24;
    if !(0x20..=0x3f).contains(&op) || op & 4 != 0 || op & 2 != 0 {
        return None;
    }
    let count = if op & 8 != 0 { 4 } else { 3 };
    let gouraud = op & 0x10 != 0;
    let color = |word: u32| [word as u8, (word >> 8) as u8, (word >> 16) as u8];
    let mut vertices = Vec::with_capacity(count);
    let mut at = 1;
    let mut current = color(command);
    for index in 0..count {
        if gouraud && index > 0 {
            current = color(*words.get(at)?);
            at += 1;
        }
        let xy = *words.get(at)?;
        at += 1;
        let x = ((xy << 21) as i32 >> 21) as f32;
        let y = ((xy >> 16 << 21) as i32 >> 21) as f32;
        vertices.push(SkyVertex {
            x,
            y,
            color: current,
        });
    }
    Some(SkyPolygon { vertices })
}

/// Colour bins of the sky model.
pub const SKY_BINS: usize = 32;

/// The bin of an elevation (radians): uniform in `sign(s) sqrt(|s|)` with
/// `s = sin(elevation)`, so half the bins lie within about 14.5 degrees of
/// the horizon, where the sky's gradient is.
pub fn sky_bin(elevation: f32) -> usize {
    let s = elevation.sin();
    let t = s.signum() * s.abs().sqrt();
    (((t + 1.0) * 0.5 * SKY_BINS as f32).floor() as isize).clamp(0, SKY_BINS as isize - 1) as usize
}

/// The elevation (radians) at the centre of bin `bin`.
pub fn bin_elevation(bin: usize) -> f32 {
    let t = (bin as f32 + 0.5) / SKY_BINS as f32 * 2.0 - 1.0;
    (t.signum() * t * t).asin()
}

/// The sky's colour by elevation (linear 0..1 RGB of the 8-bit colours).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyTable {
    pub colors: [[f32; 3]; SKY_BINS],
    /// Bins that got samples (the others are filled from the nearest).
    pub sampled: [bool; SKY_BINS],
}

impl SkyTable {
    /// One colour at every elevation.
    pub fn uniform(color: [f32; 3]) -> Self {
        Self {
            colors: [color; SKY_BINS],
            sampled: [true; SKY_BINS],
        }
    }

    /// The colour at `elevation`, interpolated between bin centres.
    pub fn sample(&self, elevation: f32) -> [f32; 3] {
        let s = elevation.sin();
        let t = s.signum() * s.abs().sqrt();
        let position = (t + 1.0) * 0.5 * SKY_BINS as f32 - 0.5;
        let low = position.floor().clamp(0.0, (SKY_BINS - 1) as f32);
        let fraction = (position - low).clamp(0.0, 1.0);
        let low = low as usize;
        let high = (low + 1).min(SKY_BINS - 1);
        mix(self.colors[low], self.colors[high], fraction)
    }
}

/// **The sky model.** Bin the sky slot's polygons by elevation: each
/// polygon is sampled on a `step`-pixel screen grid (Gouraud colours
/// interpolated as drawn), each sample's direction through `view` gives its
/// bin, and a bin is the mean of its samples. Empty bins take the nearest
/// sampled bin's colour (above the top sample: the top; below: the
/// bottom). `None` without any sample.
pub fn bin_sky(polygons: &[SkyPolygon], view: &View, step: f32) -> Option<SkyTable> {
    let step = step.max(0.5);
    let mut sums = [[0.0f64; 3]; SKY_BINS];
    let mut counts = [0u32; SKY_BINS];
    for polygon in polygons {
        let v = &polygon.vertices;
        let triangles: &[[usize; 3]] = match v.len() {
            3 => &[[0, 1, 2]],
            4 => &[[0, 1, 2], [1, 3, 2]],
            _ => &[],
        };
        for &[a, b, c] in triangles {
            let (a, b, c) = (v[a], v[b], v[c]);
            let area = (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y);
            if area.abs() < 1e-3 {
                continue;
            }
            let (x0, x1) = (a.x.min(b.x).min(c.x), a.x.max(b.x).max(c.x));
            let (y0, y1) = (a.y.min(b.y).min(c.y), a.y.max(b.y).max(c.y));
            let mut y = (y0 / step).floor() * step + step * 0.5;
            while y <= y1 {
                let mut x = (x0 / step).floor() * step + step * 0.5;
                while x <= x1 {
                    let wa = ((b.x - x) * (c.y - y) - (c.x - x) * (b.y - y)) / area;
                    let wb = ((c.x - x) * (a.y - y) - (a.x - x) * (c.y - y)) / area;
                    let wc = 1.0 - wa - wb;
                    if wa >= 0.0 && wb >= 0.0 && wc >= 0.0 {
                        let bin = sky_bin(elevation(view.direction(x, y)));
                        for (channel, sum) in sums[bin].iter_mut().enumerate() {
                            let value = wa * f32::from(a.color[channel])
                                + wb * f32::from(b.color[channel])
                                + wc * f32::from(c.color[channel]);
                            *sum += f64::from(value / 255.0);
                        }
                        counts[bin] += 1;
                    }
                    x += step;
                }
                y += step;
            }
        }
    }
    let sampled: Vec<usize> = (0..SKY_BINS).filter(|&bin| counts[bin] > 0).collect();
    if sampled.is_empty() {
        return None;
    }
    let mut table = SkyTable {
        colors: [[0.0; 3]; SKY_BINS],
        sampled: [false; SKY_BINS],
    };
    for bin in 0..SKY_BINS {
        let source = *sampled
            .iter()
            .min_by_key(|&&other| (other as isize - bin as isize).unsigned_abs())
            .expect("a sampled bin");
        let n = f64::from(counts[source]);
        table.colors[bin] = sums[source].map(|sum| (sum / n) as f32);
        table.sampled[bin] = counts[bin] > 0;
    }
    Some(table)
}

// ---- the fog colour -----------------------------------------------------------------

/// The fog colour of mission section 0's word +8 (`[[world+0x244]+8]`):
/// RGB bytes, the colour of strip 16's fill and of the sky's lowest bands.
pub fn fog_color(word: u32) -> [u8; 3] {
    [word as u8, (word >> 8) as u8, (word >> 16) as u8]
}

/// [`fog_color`] read through `read_word` (a RAM word reader, KSEG0).
pub fn read_fog_color(read_word: impl Fn(u32) -> u32) -> [u8; 3] {
    let world = read_word(jp::WORLD);
    let mission = read_word(world + jp::MISSION);
    fog_color(read_word(mission + jp::FOG_COLOR))
}

/// 8-bit RGB as linear 0..1 floats (the haze works on the 8-bit values).
pub fn unit_color(color: [u8; 3]) -> [f32; 3] {
    color.map(|c| f32::from(c) / 255.0)
}

// ---- the GTE depth cue -----------------------------------------------------------

/// The depth-cue fraction (IR0 / 4096) the game's GTE set-up (8005E7EC,
/// from camera +0x28/+0x2A) gives a vertex at view depth `z`:
/// `far (z - near) / (z (far - near))`, clamped to 0..1. Terrain colours
/// move toward 0x83 grey by it, object colours toward their far colour.
pub fn depth_cue(z: f32, near: f32, far: f32) -> f32 {
    if z <= 0.0 || far <= near {
        return 0.0;
    }
    (far * (z - near) / (z * (far - near))).clamp(0.0, 1.0)
}

/// The game's terrain fog fraction at view depth `z` (its strip level /
/// 4096), for slot `z / 8 + 1` and the strips' `slots`.
pub fn palette_fog(z: f32, slots: &[u16; STRIPS]) -> f32 {
    let slot = ((z / 8.0).floor() as i32 + 1).clamp(0, 4095) as u16;
    f32::from(strip_level(strip_for_slot(slot, slots))) / 4096.0
}

// ---- the haze ---------------------------------------------------------------------

/// **The haze model** (the post pass at world end): an exponential height
/// fog integrated analytically between the camera's and the point's
/// altitudes, on a curved Earth, coloured by the sky and the fog colour,
/// with a Henyey-Greenstein sun term. World units are about 5 per metre;
/// y is down, altitudes are `sea_level - y`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Haze {
    /// Extinction per world unit at sea level (1/110,000: 37% haze over
    /// 10 km at sea level).
    pub density: f32,
    /// The density's e-folding height (7,000 units, 1.4 km).
    pub scale_height: f32,
    /// The world y of sea level.
    pub sea_level: f32,
    /// The Earth's radius in world units (6.371e6 m x 5).
    pub earth_radius: f32,
    /// How much of the fog colour is in the haze colour (the rest is the
    /// sky near the horizon).
    pub fog_mix: f32,
    /// Henyey-Greenstein asymmetry of the sun term.
    pub sun_g: f32,
    /// The sun term's strength (times the sun colour and the phase function
    /// normalised to a mean of 1 over the sphere).
    pub sun_strength: f32,
    /// The game's terrain ends at its draw distance: view depths from
    /// `edge[0]` to `edge[1]` fade into the sky behind, so no edge shows
    /// while nothing is drawn beyond it (`None` with far terrain).
    pub edge: Option<[f32; 2]>,
}

impl Default for Haze {
    fn default() -> Self {
        Self {
            density: 1.0 / 110_000.0,
            scale_height: 7_000.0,
            sea_level: 0.0,
            earth_radius: 6.371e6 * 5.0,
            fog_mix: 0.35,
            sun_g: 0.6,
            sun_strength: 0.08,
            edge: None,
        }
    }
}

/// The inputs of one pixel of the haze pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HazeInput {
    /// The camera's world y.
    pub camera_y: f32,
    /// Camera to point (world units, y down).
    pub offset: [f32; 3],
    /// The point's view depth (the game's z).
    pub depth: f32,
}

/// The scene inputs of the haze, per frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HazeScene<'a> {
    pub sky: &'a SkyTable,
    /// The fog colour, 0..1.
    pub fog: [f32; 3],
    /// Toward the sun (world, unit).
    pub sun: [f32; 3],
    /// The sun's colour, 0..1.
    pub sun_color: [f32; 3],
}

/// The Henyey-Greenstein phase function (per steradian) at `cos_theta`.
pub fn henyey_greenstein(cos_theta: f32, g: f32) -> f32 {
    let denominator = (1.0 + g * g - 2.0 * g * cos_theta).max(1e-6);
    (1.0 - g * g) / (4.0 * PI * denominator * denominator.sqrt())
}

impl Haze {
    /// How far the ground falls below the tangent plane at horizontal
    /// distance `d`: `d^2 / 2R`.
    pub fn earth_drop(&self, d: f32) -> f32 {
        d * d / (2.0 * self.earth_radius)
    }

    /// Optical depth from altitude `from` to altitude `to` over `distance`
    /// (density `density exp(-altitude / scale_height)`, the altitude
    /// linear along the ray): `density distance exp(-from/H) (1 - e^-k)/k`
    /// with `k = (to - from)/H` (1 at k = 0).
    pub fn optical_depth(&self, from: f32, to: f32, distance: f32) -> f32 {
        let h = self.scale_height;
        let k = (to - from) / h;
        let shape = if k.abs() < 1e-4 {
            1.0 - k * 0.5
        } else {
            (1.0 - (-k).exp()) / k
        };
        self.density * distance * (-from / h).exp() * shape
    }

    /// The altitudes of the camera and of the point of `input`, the
    /// point's above the curved sea (its flat altitude plus the drop).
    pub fn altitudes(&self, input: &HazeInput) -> (f32, f32) {
        let camera = self.sea_level - input.camera_y;
        let [x, y, z] = input.offset;
        let horizontal = (x * x + z * z).sqrt();
        (camera, camera - y + self.earth_drop(horizontal))
    }

    /// The light the haze lets through from the point of `input`.
    pub fn transmittance(&self, input: &HazeInput) -> f32 {
        let (camera, point) = self.altitudes(input);
        let [x, y, z] = input.offset;
        let distance = (x * x + y * y + z * z).sqrt();
        (-self.optical_depth(camera.max(0.0), point.max(0.0), distance)).exp()
    }

    /// The haze's colour looking along `direction` (world, unit):
    /// `mix(sky just above the horizon, fog, fog_mix)` plus the sun term.
    pub fn haze_color(&self, scene: &HazeScene<'_>, direction: [f32; 3]) -> [f32; 3] {
        let horizon = scene.sky.sample(elevation(direction).max(0.0));
        let base = mix(horizon, scene.fog, self.fog_mix);
        let phase = 4.0 * PI * henyey_greenstein(dot(direction, scene.sun), self.sun_g);
        let sun = self.sun_strength * phase;
        [
            base[0] + scene.sun_color[0] * sun,
            base[1] + scene.sun_color[1] * sun,
            base[2] + scene.sun_color[2] * sun,
        ]
    }

    /// **The haze of one pixel**: `color` (0..1) of the point of `input`
    /// seen through the haze, then faded into the sky behind across the
    /// game's draw-distance edge.
    pub fn shade(&self, color: [f32; 3], input: &HazeInput, scene: &HazeScene<'_>) -> [f32; 3] {
        let direction = normalize(input.offset);
        let through = self.transmittance(input);
        let haze = self.haze_color(scene, direction);
        let hazed = mix(haze, color, through);
        match self.edge {
            Some([start, end]) if end > start => {
                let t = ((input.depth - start) / (end - start)).clamp(0.0, 1.0);
                let fade = t * t * (3.0 - 2.0 * t);
                mix(hazed, scene.sky.sample(elevation(direction)), fade)
            }
            _ => hazed,
        }
        .map(|c| c.clamp(0.0, 1.0))
    }
}

/// The WGSL twin of [`Haze::shade`] (`haze_shade`), a snippet for the post
/// pass: no bindings or entry points.
pub const HAZE_WGSL: &str = include_str!("haze.wgsl");

/// Rows (`vec4<f32>`) of `HazeUniform` in [`HAZE_WGSL`].
pub const HAZE_UNIFORM_ROWS: usize = 6 + SKY_BINS;

impl Haze {
    /// `HazeUniform` of [`HAZE_WGSL`] for a frame: this haze, `scene` and
    /// the camera's world y. Every member is a `vec4<f32>`, so the rows are
    /// the uniform buffer's bytes in order (16 bytes each).
    pub fn uniform(&self, scene: &HazeScene<'_>, camera_y: f32) -> [[f32; 4]; HAZE_UNIFORM_ROWS] {
        let mut rows = [[0.0; 4]; HAZE_UNIFORM_ROWS];
        let [start, end] = self.edge.unwrap_or([0.0, 0.0]);
        rows[0] = [
            self.density,
            self.scale_height,
            self.sea_level,
            self.earth_radius,
        ];
        rows[1] = [self.fog_mix, self.sun_g, self.sun_strength, camera_y];
        rows[2] = [start, end, 0.0, 0.0];
        let vector = |v: [f32; 3]| [v[0], v[1], v[2], 0.0];
        rows[3] = vector(scene.fog);
        rows[4] = vector(scene.sun);
        rows[5] = vector(scene.sun_color);
        for (row, color) in rows[6..].iter_mut().zip(&scene.sky.colors) {
            *row = vector(*color);
        }
        rows
    }
}

/// A 4x4 ordered-dither threshold (-0.5..0.5 of one 5-bit step) for pixel
/// (`x`, `y`): the post pass writes 5-bit channels back.
pub fn dither(x: u32, y: u32) -> f32 {
    const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    (f32::from(BAYER[(y & 3) as usize][(x & 3) as usize]) + 0.5) / 16.0 - 0.5
}

/// A 0..1 colour as RGB555 (no mask bit), with the dither offset `bias`
/// (from [`dither`]) in 5-bit steps.
pub fn to_rgb555(color: [f32; 3], bias: f32) -> u16 {
    let channel = |c: f32| ((c * 31.0 + 0.5 + bias).floor().clamp(0.0, 31.0)) as u16;
    channel(color[0]) | (channel(color[1]) << 5) | (channel(color[2]) << 10)
}

/// RGB555 as 0..1 colours (`c / 31`).
pub fn from_rgb555(pixel: u16) -> [f32; 3] {
    [0, 5, 10].map(|shift| f32::from((pixel >> shift) & 0x1f) / 31.0)
}
