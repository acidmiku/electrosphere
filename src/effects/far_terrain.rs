//! Far terrain to the horizon (Milestone 1 Phase 4 item 6,
//! `RECOMP_ENHANCEMENTS.md` §2.4 and §4.5): in Japanese flight, with the GPU
//! renderer above 1x, the mission's terrain beyond the game's own range,
//! drawn at world begin beneath every world primitive, so the game's
//! terrain draws over it. `docs/EFFECTS_TERRAIN.md` ("Far terrain: the
//! drawing") is the long form; `docs/EFFECTS.md` the contract.
//!
//! Display only: nothing here changes machine RAM, VRAM as the game sees
//! it, audio or timing. `[effects] far_terrain` (with `enabled`, and
//! `atmosphere`: without the atmosphere's palette redirect the game's
//! terrain ends in a fog wall) turns it on.
//!
//! # The two halves
//!
//! - [`FarTerrain`] ([`fx::RecordEffect`], the recorder): at each effects
//!   frame's kick it keeps the terrain map up to date
//!   ([`TerrainMap::update`], fx-map's `terrain_map`): the first flight frame
//!   builds it (on a worker thread, from a copy of RAM and VRAM), later
//!   frames apply what changed. It hands the card side a [`FarFrame`]: the
//!   upload-ready [`MapData`] (shared, `Arc`) with a version and the
//!   changes since the previous version. It drops the game's backdrop strip
//!   in the sky slot ([`is_backdrop_strip`]) while it draws, and drops the
//!   map when a pass is no effects frame (flight left, effects off).
//! - [`FarTerrainCard`] ([`fx::CardEffect`], the card): uploads the map
//!   (whole for a new version line, else only the changed tiles and grid),
//!   then at world begin draws, over the world's area at N x: the tile
//!   meshes of a ring of cells around the camera (frustum-culled, instanced
//!   per tile type, textured from the atlas filtered by distance), then a
//!   heightmap ray march to the horizon where no mesh drew; both with the
//!   curved Earth's drop. The result goes into the N x VRAM (dithered) and
//!   the z target (view z, draw order 0).
//!
//! # Colours
//!
//! The game cues its terrain colours toward a neutral grey by depth (the GTE
//! depth cue, far colour 0x83 per channel; `EFFECTS_ATMOSPHERE.md` §1.3), so
//! at its edge (camera far, 28,000 in mission 1) its terrain is the texels
//! times 0x83 / 128, about 45% brighter than the baked vertex colours of
//! mission 1. The map is therefore built twice: as the game shades it, and
//! with every face colour 0x83 ([`neutral_ram`]); the shaders mix the two
//! with the game's own cue at each pixel's depth, so the colours continue
//! across the seam. Beyond the game's range a relief term from the sun's
//! direction fades in, so far hills read.
//!
//! # Projection
//!
//! The game's own (`fx::Scene`): the eye, the view matrix `R` (world to
//! view), `H`, the projection centre (world `+0x298`), widescreen's k: VRAM
//! x = centre.x + k H x / z, y = centre.y + H y / z, z the view z, at N x
//! VRAM pixel `VRAM * N` (the renderer's polygon convention). [`View`] is
//! the CPU form, [`reference_march`] the CPU ray march the tests compare the
//! card with.
use super::terrain_map::{
    ATLAS_COLUMNS, ATLAS_HEIGHT, ATLAS_WIDTH, Atlas, CELL_TEXELS, GRID_SIZE, MapError, Source,
    TILE_UNITS, TerrainMap, Update, flags, wrap_cell,
};
use crate::gpu_renderer::fx::{self, Phase, Primitive, Route, Scene};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[path = "far_terrain_card.rs"]
mod card;
pub use card::{CardStats, FarTerrainCard, GpuTimer, PassTimes};

/// The name joining the two halves.
pub const NAME: &str = "far_terrain";
/// The ring's radius in tiles by default (theirs: 44, 89 x 89 cells).
pub const DEFAULT_TILES: u32 = 44;
/// The ring's radius is kept within these.
pub const MIN_TILES: u32 = 16;
pub const MAX_TILES: u32 = 96;
/// The Earth's radius in world units (5 per metre).
pub const EARTH_RADIUS: f64 = 6.371e6 * 5.0;
/// Atlas levels: 32, 16, 8, 4, 2 and 1 texels per tile side. Every level of
/// a tile type's cell holds only its own texels (cells are aligned powers
/// of two), so the levels filter without bleeding across cells.
pub const LEVELS: usize = 6;
/// Bytes per mesh vertex: position (3 x f32, relative to the cell's mesh
/// origin), the triangle's colour as the game shades it (RGB, A its
/// [`flags`]), its neutral colour (RGB, A 255).
pub const VERTEX_BYTES: usize = 20;
/// The neutral vertex colour: the GTE's far colour for terrain (0x830).
pub const NEUTRAL: u8 = 0x83;
/// The backdrop strip's palette (`draw_sun` 80095930 writes it into every
/// packet) and its texture pages (clut row 0x39, or 0x59 with a DR_TPAGE).
pub const STRIP_CLUT: u16 = 0x4ed6;
pub const STRIP_TPAGES: [u16; 2] = [0x39, 0x59];
/// The cell tops' grid: cells -16..271 (grid indices) per side; beyond, the
/// game's wrap repeats every 8 cells.
pub const TOPS_SIDE: usize = 288;
/// Levels of the cell tops (288, 144, 72 and 36 per side: blocks of 8 x 8
/// cells at the last, aligned with the wrap's repeat of 8).
pub const TOPS_LEVELS: usize = 4;
const TOPS_FIRST: i32 = -16;
/// A height where there is no terrain (cells of tile type 0).
pub const NO_GROUND: f32 = 1.0e9;
/// [`Params::near_cut`] as a share of the game's range (camera far).
pub const NEAR_CUT: f64 = 0.75;
/// The relief term's strength (at full fade-in, a 30° slope facing the sun
/// is 1.3 times as bright as flat ground).
pub const RELIEF: f32 = 0.6;
/// A failed build is tried again after this many effects frames.
const RETRY_FRAMES: u32 = 120;
/// The worker's map is taken in the effects frame this many frames after
/// the one that started it, waiting for it then if need be: far terrain
/// appears in the same frame of a flight every run (headless pictures),
/// and a window run (about 30 effects frames a second) gives the worker
/// about 100 ms before any wait.
pub const BUILD_FRAMES: u32 = 3;

// ---- settings ----------------------------------------------------------------------------

/// `[effects] far_terrain_tiles`: the radius of the ring of tile meshes, in
/// tiles (2048 units, about 410 m); beyond it the ray march draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Ring(pub u32);

impl Default for Ring {
    fn default() -> Self {
        Self(DEFAULT_TILES)
    }
}

impl Ring {
    /// The radius used: within [`MIN_TILES`]..=[`MAX_TILES`].
    pub fn tiles(self) -> u32 {
        self.0.clamp(MIN_TILES, MAX_TILES)
    }
}

/// `[effects] far_terrain_quality`: the ray march's step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// Two texels of the pixel's level per step.
    Low,
    /// One texel.
    #[default]
    Medium,
    /// Half a texel.
    High,
}

impl Quality {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// The march's step in texels of the pixel's level, and its most steps.
    pub fn march(self) -> (f32, u32) {
        match self {
            Self::Low => (2.0, 512),
            Self::Medium => (1.0, 1024),
            Self::High => (0.5, 2048),
        }
    }
}

/// How far terrain draws ([`Settings::from_settings`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// `[effects] far_terrain` with `atmosphere` (both also need `enabled`,
    /// which F6 flips). Off, both halves are registered and do nothing.
    pub on: bool,
    pub ring: Ring,
    pub quality: Quality,
    /// Build the map within the first effects frame instead of on a worker
    /// thread (tests). Not a user setting.
    pub blocking: bool,
}

impl Settings {
    pub fn with_blocking(self, blocking: bool) -> Self {
        Self { blocking, ..self }
    }

    /// From `[effects]`: on when `far_terrain` and `atmosphere` are both
    /// set. Far terrain without the atmosphere would end the game's fogged
    /// terrain in a wall of fog: it stays off, and says so.
    pub fn from_settings(effects: &crate::settings::Effects) -> Self {
        if effects.far_terrain && !effects.atmosphere {
            eprintln!(
                "ac3: effects: far_terrain needs atmosphere (without it the game's terrain ends \
                 in a wall of fog); far terrain off"
            );
        }
        Self {
            on: effects.far_terrain && effects.atmosphere,
            ring: effects.far_terrain_tiles,
            quality: effects.far_terrain_quality,
            blocking: false,
        }
    }
}

/// Register the recorder half on `recorder` (after
/// [`Recorder::set_effects`](crate::gpu_renderer::Recorder::set_effects):
/// nothing when effects are not configured, so at 1x and outside the
/// Japanese release). Off, it does nothing.
pub fn register_recorder(recorder: &mut crate::gpu_renderer::Recorder, settings: Settings) {
    if recorder.effects_config().is_none() {
        return;
    }
    recorder.add_effect(Box::new(FarTerrain::new(settings)));
    if settings.on {
        eprintln!(
            "ac3: effects: far terrain on: a ring of {} tiles, then the horizon ({} quality)",
            settings.ring.tiles(),
            settings.quality.name()
        );
    }
}

/// Register the card half on `executor` (above 1x). It draws only what the
/// recorder half hands over.
pub fn register_card(executor: &mut crate::gpu_renderer::Executor) {
    if executor.scale() > 1 {
        executor.add_effect(Box::new(FarTerrainCard::new()));
    }
}

// ---- the map, upload-ready ---------------------------------------------------------------

/// One tile type of [`MapData`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TileData {
    /// Its triangles' vertices in [`MapData::vertices`].
    pub first: u32,
    pub count: u32,
    /// Its geometry's highest and lowest points (world y; the mesh header's
    /// `+0x16` / `+0x14`, widened by the sea level for flattened water).
    pub top: f32,
    pub floor: f32,
    /// The highest point of its level-0 heights ([`NO_GROUND`] for type 0).
    pub height_top: f32,
}

/// The terrain map as the card uploads it ([`MapData::new`] from the two
/// [`TerrainMap`]s). Atlas levels are row-major, `512 >> level` texels per
/// row, tile type `t`'s cell at `((t % 16) n, (t / 16) n)`, `n = 32 >> level`.
#[derive(Clone, Debug)]
pub struct MapData {
    /// Tile type per grid cell, `z * 256 + x` (`R16Uint`).
    pub grid: Vec<u16>,
    /// Per tile type (0: none).
    pub tiles: Vec<TileData>,
    /// [`VERTEX_BYTES`] per vertex, three per triangle; water at the sea
    /// level.
    pub vertices: Vec<u8>,
    /// Mean heights (world y) per level, water at the sea level (`R32Float`).
    pub heights: Vec<Vec<f32>>,
    /// Colours per level (sRGB bytes; levels averaged in linear light): as
    /// the game shades its terrain, and neutral (vertex colour 0x83).
    pub baked: Vec<Vec<[u8; 4]>>,
    pub neutral: Vec<Vec<[u8; 4]>>,
    /// Per grid cell -16..271 (both axes; `TOPS_SIDE` per row): the highest
    /// level-0 height of the cell and its eight neighbours, after the wrap;
    /// then [`TOPS_LEVELS`] - 1 coarser levels, each the highest of 2 x 2
    /// (level 3: blocks of 8 x 8 cells, 36 per row, the march's coarse
    /// skip).
    pub tops: Vec<Vec<f32>>,
    /// The highest point of the whole map (smallest y over the grid's
    /// tiles), and the lowest.
    pub top: f32,
    pub floor: f32,
    /// The sea level (world y), when a sea was detected.
    pub sea_level: Option<f32>,
}

/// Level `level`'s width and height in texels.
pub fn level_size(level: usize) -> (usize, usize) {
    (ATLAS_WIDTH >> level, ATLAS_HEIGHT >> level)
}

/// The index of texel `(i, j)` of tile type `tile`'s cell at `level`.
pub fn level_index(tile: u16, i: usize, j: usize, level: usize) -> usize {
    let n = CELL_TEXELS >> level;
    let tile = usize::from(tile);
    let (width, _) = level_size(level);
    ((tile / ATLAS_COLUMNS) * n + j) * width + (tile % ATLAS_COLUMNS) * n + i
}

/// sRGB byte to linear light.
fn linear(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light to an sRGB byte.
fn srgb(value: f32) -> u8 {
    let c = value.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

impl MapData {
    /// From the map as the game shades it (`baked`) and the same map with
    /// neutral face colours (`neutral`, built from [`neutral_ram`]).
    pub fn new(baked: &TerrainMap, neutral: &TerrainMap) -> Self {
        let levels_f32 = || {
            (0..LEVELS)
                .map(|l| {
                    let (w, h) = level_size(l);
                    vec![0.0f32; w * h]
                })
                .collect::<Vec<_>>()
        };
        let levels_rgba = || {
            (0..LEVELS)
                .map(|l| {
                    let (w, h) = level_size(l);
                    vec![[0u8, 0, 0, 255]; w * h]
                })
                .collect::<Vec<_>>()
        };
        let mut data = Self {
            grid: baked.grid.clone(),
            tiles: vec![TileData::default(); baked.tiles.len()],
            vertices: vec![0; baked.mesh.vertices.len() * VERTEX_BYTES],
            heights: levels_f32(),
            baked: levels_rgba(),
            neutral: levels_rgba(),
            tops: (0..TOPS_LEVELS)
                .map(|l| vec![NO_GROUND; (TOPS_SIDE >> l) * (TOPS_SIDE >> l)])
                .collect(),
            top: NO_GROUND,
            floor: -NO_GROUND,
            sea_level: baked.sea.as_ref().map(|sea| sea.level),
        };
        data.tiles[0].height_top = NO_GROUND;
        for tile in 1..baked.tiles.len() {
            data.shade_tile(baked, neutral, tile as u16);
        }
        data.refresh_tops();
        data
    }

    /// Bring tile types `tiles` up to date (their textures changed).
    pub fn update_tiles(&mut self, baked: &TerrainMap, neutral: &TerrainMap, tiles: &[u16]) {
        for &tile in tiles {
            if usize::from(tile) < self.tiles.len() && tile != 0 {
                self.shade_tile(baked, neutral, tile);
            }
        }
        self.refresh_tops();
    }

    /// Bring the grid up to date (cells were rewritten).
    pub fn update_grid(&mut self, baked: &TerrainMap) {
        self.grid.copy_from_slice(&baked.grid);
        self.refresh_tops();
    }

    /// One tile type's atlas cell at every level, its vertices and summary.
    fn shade_tile(&mut self, baked: &TerrainMap, neutral: &TerrainMap, tile: u16) {
        let sea = self.sea_level;
        let water = |flag: u8| flag & flags::WATER != 0;
        // Level 0.
        let mut height_top = NO_GROUND;
        for j in 0..CELL_TEXELS {
            for i in 0..CELL_TEXELS {
                let at = Atlas::index(tile, i, j);
                let height = match sea {
                    Some(level) if water(baked.atlas.flags[at]) => level,
                    _ => baked.atlas.heights[at],
                };
                height_top = height_top.min(height);
                let to = level_index(tile, i, j, 0);
                self.heights[0][to] = height;
                let [r, g, b, _] = baked.atlas.colors[at];
                self.baked[0][to] = [r, g, b, 255];
                let [r, g, b, _] = neutral
                    .atlas
                    .colors
                    .get(at)
                    .copied()
                    .unwrap_or([r, g, b, 0]);
                self.neutral[0][to] = [r, g, b, 255];
            }
        }
        // Coarser levels: 2 x 2 means within the cell.
        for level in 1..LEVELS {
            let n = CELL_TEXELS >> level;
            for j in 0..n {
                for i in 0..n {
                    let from = [
                        level_index(tile, 2 * i, 2 * j, level - 1),
                        level_index(tile, 2 * i + 1, 2 * j, level - 1),
                        level_index(tile, 2 * i, 2 * j + 1, level - 1),
                        level_index(tile, 2 * i + 1, 2 * j + 1, level - 1),
                    ];
                    let to = level_index(tile, i, j, level);
                    self.heights[level][to] = from
                        .iter()
                        .map(|&f| self.heights[level - 1][f])
                        .sum::<f32>()
                        / 4.0;
                    for colours in [&mut self.baked, &mut self.neutral] {
                        let mut sum = [0.0f32; 3];
                        for &f in &from {
                            for (s, &c) in sum.iter_mut().zip(&colours[level - 1][f][..3]) {
                                *s += linear(c);
                            }
                        }
                        let [r, g, b] = sum.map(|s| srgb(s / 4.0));
                        colours[level][to] = [r, g, b, 255];
                    }
                }
            }
        }
        // The vertices.
        let source = &baked.tiles[usize::from(tile)];
        let range = source.vertices.clone();
        let mut top = f32::from(source.top);
        let mut floor = f32::from(source.floor);
        if let Some(level) = sea {
            top = top.min(level);
            floor = floor.max(level);
        }
        for v in range.clone() {
            let v = v as usize;
            let vertex = baked.mesh.vertices[v];
            let mut position = vertex.position;
            if let Some(level) = sea
                && water(vertex.color[3])
            {
                position[1] = level;
            }
            let neutral_rgb = neutral
                .mesh
                .vertices
                .get(v)
                .map_or([NEUTRAL; 3], |n| [n.color[0], n.color[1], n.color[2]]);
            let bytes = &mut self.vertices[v * VERTEX_BYTES..(v + 1) * VERTEX_BYTES];
            for (k, c) in position.iter().enumerate() {
                bytes[4 * k..4 * k + 4].copy_from_slice(&c.to_le_bytes());
            }
            bytes[12..16].copy_from_slice(&vertex.color);
            bytes[16..19].copy_from_slice(&neutral_rgb);
            bytes[19] = 255;
        }
        self.tiles[usize::from(tile)] = TileData {
            first: range.start,
            count: range.end - range.start,
            top,
            floor,
            height_top,
        };
    }

    /// The tile type of grid cell `(cx, cz)` (grid indices, before the wrap).
    pub fn tile_at(&self, cx: i32, cz: i32) -> u16 {
        self.grid[wrap_cell(cz) * GRID_SIZE + wrap_cell(cx)]
    }

    /// The cell tops and the map's range of heights.
    fn refresh_tops(&mut self) {
        let height_top = |tile: u16| {
            self.tiles
                .get(usize::from(tile))
                .map_or(NO_GROUND, |t| t.height_top)
        };
        let mut own = vec![NO_GROUND; TOPS_SIDE * TOPS_SIDE];
        for (index, value) in own.iter_mut().enumerate() {
            let cx = (index % TOPS_SIDE) as i32 + TOPS_FIRST;
            let cz = (index / TOPS_SIDE) as i32 + TOPS_FIRST;
            *value = height_top(self.tile_at(cx, cz));
        }
        for z in 0..TOPS_SIDE as i32 {
            for x in 0..TOPS_SIDE as i32 {
                let mut top = NO_GROUND;
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        let (nx, nz) = (x + dx, z + dz);
                        let value = if (0..TOPS_SIDE as i32).contains(&nx)
                            && (0..TOPS_SIDE as i32).contains(&nz)
                        {
                            own[nz as usize * TOPS_SIDE + nx as usize]
                        } else {
                            height_top(self.tile_at(nx + TOPS_FIRST, nz + TOPS_FIRST))
                        };
                        top = top.min(value);
                    }
                }
                self.tops[0][z as usize * TOPS_SIDE + x as usize] = top;
            }
        }
        for level in 1..TOPS_LEVELS {
            let side = TOPS_SIDE >> level;
            for z in 0..side {
                for x in 0..side {
                    let below = &self.tops[level - 1];
                    let at = |i: usize, j: usize| below[(2 * z + j) * 2 * side + 2 * x + i];
                    self.tops[level][z * side + x] =
                        at(0, 0).min(at(1, 0)).min(at(0, 1)).min(at(1, 1));
                }
            }
        }
        let used: Vec<&TileData> = {
            let mut seen = vec![false; self.tiles.len()];
            for &tile in &self.grid {
                if let Some(s) = seen.get_mut(usize::from(tile)) {
                    *s = true;
                }
            }
            self.tiles
                .iter()
                .enumerate()
                .filter(|(t, _)| *t != 0 && seen[*t])
                .map(|(_, data)| data)
                .collect()
        };
        self.top = used
            .iter()
            .map(|t| t.top.min(t.height_top))
            .fold(NO_GROUND, f32::min);
        self.floor = used.iter().map(|t| t.floor).fold(-NO_GROUND, f32::max);
    }

    /// The atlas index (into a level's data) of global texel `g` (floor of
    /// world / (64 << level)); `None` over a cell without terrain.
    fn atlas_at(&self, g: [i64; 2], level: usize) -> Option<usize> {
        let n = (CELL_TEXELS >> level) as i64;
        let cell = g.map(|v| v.div_euclid(n));
        let tile = self.tile_at(cell[0] as i32 + 128, cell[1] as i32 + 128);
        if tile == 0 {
            return None;
        }
        let inner = g.map(|v| v.rem_euclid(n) as usize);
        Some(level_index(tile, inner[0], inner[1], level))
    }

    /// The height (world y) at world `(x, z)`, bilinear between texel
    /// centres at `level`, as the shader filters it; `None` without terrain.
    pub fn height_at(&self, x: f64, z: f64, level: usize) -> Option<f64> {
        let size = f64::from(64u32 << level);
        let u = [x / size - 0.5, z / size - 0.5];
        let base = u.map(f64::floor);
        let f = [u[0] - base[0], u[1] - base[1]];
        let g0 = base.map(|v| v as i64);
        let mut sum = 0.0;
        let mut total = 0.0;
        for c in 0..4 {
            let (cx, cz) = (c & 1, c >> 1);
            if let Some(at) = self.atlas_at([g0[0] + cx, g0[1] + cz], level) {
                let w = (if cx == 1 { f[0] } else { 1.0 - f[0] })
                    * (if cz == 1 { f[1] } else { 1.0 - f[1] });
                sum += w * f64::from(self.heights[level][at]);
                total += w;
            }
        }
        (total > 0.0).then(|| sum / total)
    }
}

/// A copy of `ram` with every terrain face colour (r, g, b) of `map`'s tile
/// meshes set to [`NEUTRAL`], the GPU code bytes kept: the source of the
/// neutral map.
pub fn neutral_ram(ram: &[u8], map: &TerrainMap) -> Vec<u8> {
    let mut out = ram.to_vec();
    let at = |address: u32| (address & 0x1f_ffff) as usize;
    let word = |ram: &[u8], address: u32| -> Option<u32> {
        let o = at(address);
        ram.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    for tile in map.tiles.iter().skip(1) {
        if tile.address == 0 {
            continue;
        }
        for (offset, stride, colours) in [(0xc, 0x24, 1), (0x10, 0x30, 4)] {
            let Some(list) = word(ram, tile.address + offset) else {
                continue;
            };
            let Some(count) = word(ram, list) else {
                continue;
            };
            for face in 0..count.min(4096) {
                for c in 0..colours {
                    let o = at(list + 4 + stride * face + 0x20 + 4 * c);
                    if let Some(bytes) = out.get_mut(o..o + 3) {
                        bytes.fill(NEUTRAL);
                    }
                }
            }
        }
    }
    out
}

// ---- the recorder half ---------------------------------------------------------------------

/// What one frame hands the card side.
#[derive(Clone, Debug)]
pub struct FarFrame {
    pub map: Arc<MapData>,
    /// The version of `map` (unique in the process).
    pub version: u64,
    /// The changes from version `changes.base` to `version`, when this
    /// frame made any; a card holding another version uploads everything.
    pub changes: Option<Changes>,
    pub settings: Settings,
}

/// What changed between two versions of the map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub base: u64,
    /// The grid (and the cell tops).
    pub grid: bool,
    /// Tile types whose atlas cells and vertex colours changed.
    pub tiles: Vec<u16>,
}

/// What the recorder side did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RecordStats {
    pub frames: u64,
    pub builds: u64,
    pub failed_builds: u64,
    /// The last build, in milliseconds (both maps and the upload data).
    pub build_ms: f64,
    /// Frames whose update changed something, grid cells and tile types.
    pub updates: u64,
    pub grid_updates: u64,
    pub tile_updates: u64,
    /// Backdrop strip quads dropped.
    pub dropped: u64,
    /// Passes that were no effects frame and dropped a map.
    pub dropped_maps: u64,
}

/// Both maps: as the game shades its terrain, and neutral.
struct Maps {
    baked: TerrainMap,
    neutral: TerrainMap,
}

type Built = Result<(Maps, MapData), MapError>;

/// Build both maps and the upload data from a flight frame.
fn build(ram: &[u8], vram: &[u16]) -> Built {
    let baked = TerrainMap::build(&Source { ram, vram })?;
    let patched = neutral_ram(ram, &baked);
    let neutral = TerrainMap::build(&Source {
        ram: &patched,
        vram,
    })?;
    let data = MapData::new(&baked, &neutral);
    Ok((Maps { baked, neutral }, data))
}

static VERSIONS: AtomicU64 = AtomicU64::new(1);

fn next_version() -> u64 {
    VERSIONS.fetch_add(1, Ordering::Relaxed)
}

/// Whether `primitive` is one of the game's backdrop strip quads: the sky
/// slot's chain of textured four-point polygons along the horizon that
/// `draw_sun` (80095930) builds, with palette [`STRIP_CLUT`] and texture
/// page 0x39 (additive) or 0x59 (subtractive). The same palette also
/// serves the type-2 cloud layer and the type-4 night dome (Gouraud
/// quads, kept) and the falling leaves (world slots, kept).
pub fn is_backdrop_strip(primitive: &Primitive<'_>) -> bool {
    primitive.phase == Phase::Sky
        && primitive.command & 0xfc == 0x2c
        && primitive.words.len() >= 9
        && (primitive.words[2] >> 16) as u16 == STRIP_CLUT
        && STRIP_TPAGES.contains(&((primitive.words[4] >> 16) as u16 & 0x1ff))
}

/// The recorder half; see the module documentation.
pub struct FarTerrain {
    settings: Settings,
    maps: Option<Maps>,
    /// The worker, when it started, and the effects frames since.
    building: Option<(std::thread::JoinHandle<Built>, Instant, u32)>,
    data: Option<Arc<MapData>>,
    version: u64,
    changes: Option<Changes>,
    /// Effects frames to wait before trying a failed build again.
    wait: u32,
    stats: Arc<Mutex<RecordStats>>,
}

impl FarTerrain {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            maps: None,
            building: None,
            data: None,
            version: 0,
            changes: None,
            wait: 0,
            stats: Arc::default(),
        }
    }

    /// A handle on the statistics (shared with the boxed effect).
    pub fn stats(&self) -> Arc<Mutex<RecordStats>> {
        self.stats.clone()
    }

    fn note(&self, f: impl FnOnce(&mut RecordStats)) {
        if let Ok(mut stats) = self.stats.lock() {
            f(&mut stats);
        }
    }

    /// Forget the map (a build in progress finishes unused).
    fn reset(&mut self) {
        self.maps = None;
        self.data = None;
        self.building = None;
        self.changes = None;
    }

    fn finish_build(&mut self, built: Built, started: Instant) {
        match built {
            Ok((maps, data)) => {
                let ms = started.elapsed().as_secs_f64() * 1e3;
                eprintln!(
                    "ac3: far terrain: map of {} tile types built in {ms:.0} ms{}",
                    maps.baked.stats.tiles,
                    match data.sea_level {
                        Some(level) => format!(", sea level {level}"),
                        None => ", no sea".to_owned(),
                    }
                );
                self.maps = Some(maps);
                self.data = Some(Arc::new(data));
                self.version = next_version();
                self.changes = None;
                self.note(|s| {
                    s.builds += 1;
                    s.build_ms = ms;
                });
            }
            Err(error) => {
                eprintln!("ac3: far terrain: no map ({error}); trying again later");
                self.wait = RETRY_FRAMES;
                self.note(|s| s.failed_builds += 1);
            }
        }
    }

    /// Start a build, or take a finished one.
    fn poll_build(&mut self, ram: &[u8], vram: &[u16]) {
        if let Some((_, _, frames)) = self.building.as_mut() {
            *frames += 1;
            if *frames < BUILD_FRAMES {
                return;
            }
            let (handle, started, _) = self.building.take().expect("checked");
            let built = handle.join().unwrap_or(Err(MapError::Count {
                what: "build (the worker panicked)",
                count: 0,
            }));
            self.finish_build(built, started);
            return;
        }
        if self.wait > 0 {
            self.wait -= 1;
            return;
        }
        let started = Instant::now();
        if self.settings.blocking {
            let built = build(ram, vram);
            self.finish_build(built, started);
            return;
        }
        let (ram_copy, vram_copy) = (ram.to_vec(), vram.to_vec());
        match std::thread::Builder::new()
            .name("ac3 far terrain".into())
            .spawn(move || build(&ram_copy, &vram_copy))
        {
            Ok(handle) => self.building = Some((handle, started, 0)),
            Err(error) => {
                eprintln!("ac3: far terrain: no worker thread ({error}); building here");
                let built = build(ram, vram);
                self.finish_build(built, started);
            }
        }
    }

    /// Apply a non-empty update of the baked map.
    fn apply(&mut self, update: Update, ram: &[u8], vram: &[u16]) {
        let Some(maps) = self.maps.as_mut() else {
            return;
        };
        let patched = neutral_ram(ram, &maps.baked);
        if let Err(error) = maps.neutral.update(&Source {
            ram: &patched,
            vram,
        }) {
            eprintln!("ac3: far terrain: neutral map: {error}");
        }
        let Some(data) = self.data.as_mut() else {
            return;
        };
        let data = Arc::make_mut(data);
        if !update.cells.is_empty() {
            data.update_grid(&maps.baked);
        }
        if !update.tiles.is_empty() {
            data.update_tiles(&maps.baked, &maps.neutral, &update.tiles);
        }
        let base = self.version;
        self.version = next_version();
        let mut changes = Changes {
            base,
            grid: !update.cells.is_empty(),
            tiles: update.tiles.clone(),
        };
        if let Some(previous) = self.changes.take() {
            changes.base = previous.base;
            changes.grid |= previous.grid;
            changes.tiles.extend(previous.tiles);
            changes.tiles.sort_unstable();
            changes.tiles.dedup();
        }
        self.changes = Some(changes);
        let (cells, tiles) = (update.cells.len() as u64, update.tiles.len() as u64);
        self.note(|s| {
            s.updates += 1;
            s.grid_updates += cells;
            s.tile_updates += tiles;
        });
    }
}

impl fx::RecordEffect for FarTerrain {
    fn name(&self) -> &'static str {
        NAME
    }

    fn kick(&mut self, _scene: &Scene, ram: &[u8], vram: &ac3_graphics::Vram) {
        if !self.settings.on {
            return;
        }
        self.note(|s| s.frames += 1);
        let vram = vram.words();
        let result = self
            .maps
            .as_mut()
            .map(|maps| maps.baked.update(&Source { ram, vram }));
        match result {
            Some(Ok(update)) if update.rebuilt => {
                eprintln!("ac3: far terrain: another map; building it again");
                self.reset();
            }
            Some(Ok(update)) => {
                if !update.is_empty() {
                    self.apply(update, ram, vram);
                }
                return;
            }
            Some(Err(error)) => {
                eprintln!("ac3: far terrain: the map went away ({error})");
                self.reset();
            }
            None => {}
        }
        self.poll_build(ram, vram);
    }

    fn primitive(&mut self, primitive: &Primitive<'_>) -> Route {
        if self.data.is_some() && is_backdrop_strip(primitive) {
            self.note(|s| s.dropped += 1);
            Route::Drop
        } else {
            Route::Draw
        }
    }

    fn take(&mut self) -> Option<Arc<dyn Any + Send + Sync>> {
        let map = self.data.clone()?;
        Some(Arc::new(FarFrame {
            map,
            version: self.version,
            changes: self.changes.take(),
            settings: self.settings,
        }))
    }

    fn idle(&mut self) {
        if self.maps.is_some() || self.building.is_some() {
            self.note(|s| s.dropped_maps += 1);
        }
        self.reset();
    }
}

// ---- the projection and the march, on the CPU ----------------------------------------------

/// The game's projection of a frame (`fx::Scene`), in f64.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The eye, world units.
    pub eye: [f64; 3],
    /// The view matrix rows (world to view).
    pub rows: [[f64; 3]; 3],
    /// Its inverse (view to world; the 1.3.12 matrix is not exactly
    /// orthonormal, so not quite its transpose).
    pub inverse: [[f64; 3]; 3],
    pub h: f64,
    /// Widescreen's squeeze.
    pub k: f64,
    /// The projection centre, 1x VRAM pixels.
    pub centre: [f64; 2],
}

/// The inverse of a 3 x 3 matrix (the identity for a singular one).
fn inverse(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let cofactor = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        m[r1][c1] * m[r2][c2] - m[r1][c2] * m[r2][c1]
    };
    let det = m[0][0] * cofactor(0, 0) + m[0][1] * cofactor(0, 1) + m[0][2] * cofactor(0, 2);
    if det.abs() < 1e-12 {
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    // The inverse is the transposed cofactor matrix over the determinant.
    std::array::from_fn(|r| std::array::from_fn(|c| cofactor(c, r) / det))
}

impl View {
    pub fn of(scene: &Scene) -> Self {
        let rows = scene.view.map(|row| row.map(|v| f64::from(v) / 4096.0));
        Self {
            eye: scene.eye.map(|v| f64::from(v) / 4096.0),
            rows,
            inverse: inverse(rows),
            h: f64::from(scene.h),
            k: f64::from(scene.x_scale.0) / f64::from(scene.x_scale.1.max(1)),
            centre: scene.centre.map(f64::from),
        }
    }

    /// The view position of eye-relative world point `rel`.
    pub fn to_view(&self, rel: [f64; 3]) -> [f64; 3] {
        self.rows
            .map(|row| row[0] * rel[0] + row[1] * rel[1] + row[2] * rel[2])
    }

    /// The 1x VRAM position and view z of eye-relative world point `rel`;
    /// `None` behind the eye.
    pub fn project(&self, rel: [f64; 3]) -> Option<([f64; 2], f64)> {
        let v = self.to_view(rel);
        (v[2] > 0.0).then(|| {
            (
                [
                    self.centre[0] + self.k * self.h * v[0] / v[2],
                    self.centre[1] + self.h * v[1] / v[2],
                ],
                v[2],
            )
        })
    }

    /// The unit world direction through 1x VRAM position `s`.
    pub fn ray(&self, s: [f64; 2]) -> [f64; 3] {
        let v = [
            (s[0] - self.centre[0]) / (self.k * self.h),
            (s[1] - self.centre[1]) / self.h,
            1.0,
        ];
        let d = self
            .inverse
            .map(|row| row[0] * v[0] + row[1] * v[1] + row[2] * v[2]);
        let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        d.map(|c| c / length)
    }
}

/// The Earth's drop (world y, down) at horizontal distance `d` from the eye,
/// starting at `start` (the game's range: within it the world is flat as
/// the game draws it, so the seam has no step).
pub fn earth_drop(d: f64, start: f64) -> f64 {
    let e = (d - start).max(0.0);
    e * e / (2.0 * EARTH_RADIUS)
}

/// A frame's drawing parameters (both the card and [`reference_march`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// The near clip of the meshes (view z).
    pub near: f64,
    /// The Earth's drop starts here (horizontal distance): the game's range.
    pub drop_start: f64,
    /// Ring cells wholly nearer than this view z lie within the game's
    /// range: its terrain covers them, so they draw cheaply (their
    /// triangles' colours, seen through the game's cracks only).
    pub near_cut: f64,
    /// The march starts at this horizontal distance (inside the ring) and
    /// ends at this distance along the ray.
    pub march_start: f64,
    pub march_end: f64,
    /// Step in texels of the pixel's level; most steps; bisection steps.
    pub step: f32,
    pub max_steps: u32,
    pub bisect: u32,
    /// The game's depth cue (near, far), `None`: off.
    pub cue: Option<(f64, f64)>,
    /// The relief fades in between these view depths.
    pub relief: (f64, f64),
}

impl Params {
    pub fn new(scene: &Scene, map: &MapData, settings: &Settings) -> Self {
        let view = View::of(scene);
        let far = f64::from(scene.far.max(0));
        let near = f64::from(scene.near.max(0));
        let base = f64::from(map.sea_level.unwrap_or(map.floor).min(map.floor));
        let altitude = (base - view.eye[1]).max(0.0);
        let relief = (base - f64::from(map.top)).clamp(0.0, 1.0e5);
        let horizon = (2.0 * EARTH_RADIUS * altitude).sqrt();
        let beyond = (2.0 * EARTH_RADIUS * relief).sqrt();
        let march_start = f64::from(settings.ring.tiles() * TILE_UNITS as u32);
        let (step, max_steps) = settings.quality.march();
        Self {
            near: 16.0,
            drop_start: far,
            near_cut: NEAR_CUT * far,
            march_start,
            march_end: (far + horizon + beyond + 8192.0).clamp(march_start + 1.0, 4.0e6),
            step,
            max_steps,
            bisect: 8,
            cue: (far > near && near > 0.0).then_some((near, far)),
            relief: (far.max(1.0), 3.0 * far.max(1.0)),
        }
    }
}

/// The height level whose texels match a pixel's footprint at distance `t`
/// (`h_n`: H times the scale N), as the shader picks it.
pub fn level_at(t: f64, h_n: f64) -> usize {
    let texels = t / (h_n * 64.0);
    texels
        .max(1e-6)
        .log2()
        .floor()
        .clamp(0.0, (LEVELS - 1) as f64) as usize
}

/// The ray march on the CPU, fine-stepped (a quarter of the card's medium
/// step, no cell skipping): the distance to the first point along `d` (a
/// unit world direction) below the heightmap with the Earth's drop, or
/// `None`.
pub fn reference_march(
    map: &MapData,
    view: &View,
    params: &Params,
    d: [f64; 3],
    h_n: f64,
) -> Option<f64> {
    let dxz = (d[0] * d[0] + d[2] * d[2]).sqrt();
    if dxz < 1e-6 {
        return None;
    }
    let ray_y = |t: f64| view.eye[1] + d[1] * t - earth_drop(t * dxz, params.drop_start);
    let below = |t: f64, level: usize| {
        let (x, z) = (view.eye[0] + d[0] * t, view.eye[2] + d[2] * t);
        map.height_at(x, z, level).is_some_and(|h| ray_y(t) >= h)
    };
    let mut t = (params.march_start / dxz).max(params.near);
    let mut previous = t;
    while t <= params.march_end {
        let level = level_at(t, h_n);
        if ray_y(t) < f64::from(map.top)
            && d[1] - 2.0 * (t * dxz - params.drop_start).max(0.0) / (2.0 * EARTH_RADIUS) * dxz
                <= 0.0
        {
            return None;
        }
        if below(t, level) {
            let (mut lo, mut hi) = (previous, t);
            for _ in 0..24 {
                let mid = 0.5 * (lo + hi);
                if below(mid, level) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            return Some(hi);
        }
        previous = t;
        t += 16.0 * f64::from(1u32 << level);
    }
    None
}

#[cfg(test)]
#[path = "far_terrain_tests.rs"]
mod tests;
