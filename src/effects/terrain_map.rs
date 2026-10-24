//! Far terrain, the data half (Phase 4 item 6): the mission's whole terrain
//! map, built from a flight frame's machine state as plain data a renderer
//! uploads to draw the terrain to the horizon. No GPU work happens here.
//! `docs/EFFECTS_TERRAIN.md` is the long form; `RECOMP_ENHANCEMENTS.md`
//! §2.4 and §4.5 the background.
//!
//! # Inputs (Japanese release)
//!
//! [`Source`]: main RAM and the 1x VRAM, read at a main-loop boundary in
//! flight (context state 2). The layouts, as `ac3-flight-jp` uses them:
//!
//! - `[gp+0x2E0]` ([`ac3_flight_jp::common::GAMEPLAY`]) is the world block.
//!   Its `+0x244..` words are the mission's section pointers; the terrain
//!   uses `+0x248` and `+0x24C` (`terrain::draw_terrain` 8005E6DC reads them
//!   as `params + 4` and `params + 8`).
//! - **Grid** `[world+0x248]`: 256 x 256 halfwords, row-major (`z * 256 +
//!   x`), low 9 bits the tile type, 0 = no terrain (`select_cells`
//!   8005E4CC; the collision rays 8009A5D0 / 800711FC). Bits 9..15 are
//!   flags the renderer ignores; `8005E460` keeps them on a rewrite.
//! - **Tile directory** `[world+0x24C]`: word 0 the tile count, then one
//!   mesh pointer per tile type 1..=count (`draw_cells` 80061768 indexes it
//!   by `type * 4`).
//! - **Tile mesh** (0x18-byte header):
//!   - `+4` the vertex list: a count, then three vertices per 20 bytes (x0
//!     y0, x1 y1, x2 y2, z0 z1, z2 pad halfwords); vertex `3g + i` of group
//!     `g` is the projected slot `3g + i`. Corners: vertex 0 (-x, -z), 1
//!     (+x, -z), 2 (-x, +z), 3 (+x, +z) at +-1024, shared with the
//!     neighbours' seams.
//!   - `+0xC` flat faces (count, 0x24 bytes each), `+0x10` Gouraud faces
//!     (count, 0x30 bytes each; `mesh_flat_quads` 800619E0,
//!     `mesh_gouraud_quads` 80061C3C).
//!   - `+0x14` halfword: the lowest point (largest y, the "base height" at
//!     vertex list - 4), `+0x16`: the highest point (`world_collision`
//!     8006F670 uses both).
//! - **Face**: `+0` flags (0x100 a triangle, 0x200 a split quad; the
//!   collision ray's split), `+4` normal offsets, `+8/+0xC` byte offsets for
//!   the near-face subdivision, `+0x10` four vertex index bytes, `+0x14` uv0
//!   and CLUT, `+0x18` uv1 and TPAGE, `+0x1C` uv2 and uv3, `+0x20` colour 0
//!   (r, g, b, GP0 code), Gouraud `+0x24/+0x28/+0x2C` colours 1..3. A face
//!   draws as the GPU quad (v0, v1, v2), (v1, v3, v2); a 0x100 triangle
//!   repeats v2 as v3.
//! - **VRAM**: textures through each face's TPAGE and CLUT. The terrain
//!   CLUTs sit in the fog block (320..335, 256..319), which the JP depth fog
//!   800943A8 overwrites per depth bucket during a frame; its unfogged
//!   source is the prepared block (320, 384) (even strip 0), which
//!   [`clut_entry`] reads instead. At a main-loop boundary the two are equal.
//!
//! **RAM, not the disc files, is the source.** The disc's mission container
//! (`ACE.BPB`) holds the same grid and directory as sections 1 and 2, but
//! for the JP release nothing parses it yet (`flight_assets` validates PAL
//! BPB 7/48 only), the game rewrites the grid at run time (8005E460), so RAM
//! has to be watched anyway, and the texels come from VRAM in any case. The
//! RAM copy is exactly what the game draws and collides with.
//!
//! # Outputs
//!
//! [`TerrainMap`]: the grid, per tile type a [`Tile`] (heights, a 32 x 32
//! cell of the [`Atlas`], a range of [`Mesh`] vertices), the detected
//! [`Sea`], and the texture keys ([`KeyStats`]). [`Classifier`] flags
//! texels and triangles water, forest or built.
//!
//! # Coordinates
//!
//! World units are the 20.12 positions `>> 12` (camera record `[world+0x28]`
//! `+4..+0xC`), about 5 per metre. x and z are horizontal, **y points
//! down** (negative is up). Grid cell `(cx, cz)` covers `x` in `[(cx -
//! 128) * 2048, (cx - 127) * 2048)` (same for z); its mesh origin, which
//! mesh positions are relative to, is the cell centre at y = 0
//! ([`cell_origin`]). A world point is in cell `(x >> 11) + 128`, with the
//! game's edge wrap ([`wrap_cell`]) outside 0..=255. The view transform is
//! `p_view = R (p_world - C)` with `R` the 4.12 matrix at `world+0x4C` and
//! `C` the camera position; the screen is `(H x / z, H y / z)` about the
//! screen centre with `H` at camera `+0x24` ([`MapCamera`]).
//!
//! # Change detection
//!
//! [`TerrainMap::update`] re-reads the layout words, compares the grid (128
//! KiB) and the VRAM the textures read with the copies it keeps, and
//! applies what changed: grid cells (no rebuild: tiles are per type),
//! texture changes (the affected tiles), or another map (a full rebuild).
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::time::{Duration, Instant};

/// Grid side in cells.
pub const GRID_SIZE: usize = 256;
/// Tile (grid cell) side in world units.
pub const TILE_UNITS: i32 = 2048;
/// Half a tile: mesh positions run over +-1024 about the cell centre.
pub const TILE_HALF: i32 = 1024;
/// World units per metre (RECOMP_ENHANCEMENTS §2.3).
pub const UNITS_PER_METRE: f32 = 5.0;
/// Atlas texels per tile side.
pub const CELL_TEXELS: usize = 32;
/// World units per atlas texel.
pub const TEXEL_UNITS: i32 = TILE_UNITS / CELL_TEXELS as i32;
/// Atlas cells per row (tile type `t` is at column `t % 16`, row `t / 16`).
pub const ATLAS_COLUMNS: usize = 16;
/// Atlas size in texels: 16 x 32 cells of 32 x 32 for the 512 tile types
/// the 9-bit grid can name.
pub const ATLAS_WIDTH: usize = ATLAS_COLUMNS * CELL_TEXELS;
pub const ATLAS_HEIGHT: usize = 32 * CELL_TEXELS;
/// Tile types a grid cell can name (9 bits).
pub const MAX_TILE_TYPES: usize = 512;
/// A sea face's vertices are within this many units of the sea level
/// (their rule for dropping game sea polygons, §2.5).
pub const SEA_TOLERANCE: f32 = 12.0;
/// [`Atlas::keys`] of a texel without a surface.
pub const NO_KEY: u16 = u16::MAX;

/// Atlas texel and mesh triangle flags.
pub mod flags {
    /// Open water: the sea's texture at the sea's height.
    pub const WATER: u8 = 1;
    /// Forest ([`super::Classifier`]).
    pub const FOREST: u8 = 2;
    /// Built: buildings, roads, pavement ([`super::Classifier`]).
    pub const BUILT: u8 = 4;
    /// A mesh surface covers the texel (else the texel holds the tile's
    /// floor and mean colour).
    pub const COVERED: u8 = 0x80;
}

/// The fog CLUT block the depth fog rewrites (VRAM x, y, width, height)
/// and the row offset of its unfogged source.
const FOG_BLOCK: (usize, usize, usize, usize) = (320, 256, 16, 64);
const FOG_SOURCE_ROWS: usize = 128;

/// Samples per atlas texel side: 2 x 2, 32 units apart, one per texel of
/// the common 64-texel tile textures (32 units per texel).
const SUB: usize = 2;
const SAMPLES: usize = CELL_TEXELS * SUB;
const SAMPLE_UNITS: f32 = TILE_UNITS as f32 / SAMPLES as f32;
/// Most UV-space samples a triangle's average colour takes.
const TRIANGLE_SAMPLES: f32 = 256.0;
const NO_TRIANGLE: u32 = u32::MAX;
/// Sea candidates: luma standard deviation over the whole tile below this.
const SEA_LUMA_STD: f32 = 24.0;

/// What the map is built from: the machine at a main-loop boundary.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    /// Main RAM (2 MiB, `Machine::ram`).
    pub ram: &'a [u8],
    /// The 1x VRAM (1024 x 512 halfwords, `Gpu::vram().words()`).
    pub vram: &'a [u16],
}

/// Why no map could be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MapError {
    /// A pointer outside RAM.
    Pointer { what: &'static str, address: u32 },
    /// A count out of range.
    Count { what: &'static str, count: u32 },
    /// The VRAM slice is not 1024 x 512.
    Vram(usize),
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pointer { what, address } => write!(f, "{what} pointer {address:#010x}"),
            Self::Count { what, count } => write!(f, "{what} count {count}"),
            Self::Vram(len) => write!(f, "VRAM of {len} halfwords"),
        }
    }
}

impl std::error::Error for MapError {}

/// A face's texture: GP0 TPAGE and CLUT halfwords.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextureKey {
    pub tpage: u16,
    pub clut: u16,
}

/// One tile type.
#[derive(Clone, Debug, Default)]
pub struct Tile {
    /// Mesh address in RAM (0 for type 0, no terrain).
    pub address: u32,
    /// Lowest point, the largest y (mesh `+0x14`, the "base height").
    pub floor: i16,
    /// Highest point, the smallest y (mesh `+0x16`).
    pub top: i16,
    /// Its triangles in [`Mesh::vertices`] (three vertices each).
    pub vertices: Range<u32>,
    /// Faces read (flat + Gouraud).
    pub faces: u32,
    /// Grid cells of this type.
    pub cells: u32,
    /// Mean colour of the covered atlas texels (RGBA8; A = 255).
    pub mean_color: [u8; 4],
    /// Mean height of the atlas cell (world y).
    pub mean_height: f32,
    /// Luma standard deviation over every colour sample of the tile.
    pub luma_std: f32,
    /// Fraction of the tile's area a surface covers (top-down).
    pub coverage: f32,
    /// [`flags`] held by more than half of the atlas cell's texels.
    pub flags: u8,
}

/// The atlas: 16 x 32 cells of 32 x 32 texels, one cell per tile type
/// ([`Atlas::cell_origin`]). Texel `(i, j)` of a cell covers mesh x in
/// `[-1024 + 64 i, -1024 + 64 (i + 1))`, z likewise with `j`. Row-major,
/// [`ATLAS_WIDTH`] texels per row.
#[derive(Clone)]
pub struct Atlas {
    /// World y of the top surface (mean of 2 x 2 samples), y down
    /// (`R32Float`).
    pub heights: Vec<f32>,
    /// Colour: texel x vertex colour / 128 (the GPU's modulation), without
    /// the depth fog; mean of 2 x 2 samples (`Rgba8Unorm`, display-referred
    /// values: sample as sRGB to shade in linear). A = 255 where a texture
    /// covers the texel, else 0 with the tile's mean colour.
    pub colors: Vec<[u8; 4]>,
    /// [`flags`] (`R8Uint`).
    pub flags: Vec<u8>,
    /// The texture key most samples show, as an index into
    /// [`TerrainMap::keys`] ([`NO_KEY`] for none; `R16Uint`).
    pub keys: Vec<u16>,
    /// Luma standard deviation of the 2 x 2 samples (a classification
    /// input; not needed on the GPU).
    pub luma_std: Vec<f32>,
}

impl Atlas {
    fn new() -> Self {
        let n = ATLAS_WIDTH * ATLAS_HEIGHT;
        Self {
            heights: vec![0.0; n],
            colors: vec![[0; 4]; n],
            flags: vec![0; n],
            keys: vec![NO_KEY; n],
            luma_std: vec![0.0; n],
        }
    }

    /// Texel position of tile type `tile`'s cell.
    pub fn cell_origin(tile: u16) -> (usize, usize) {
        let tile = usize::from(tile) % MAX_TILE_TYPES;
        (
            (tile % ATLAS_COLUMNS) * CELL_TEXELS,
            (tile / ATLAS_COLUMNS) * CELL_TEXELS,
        )
    }

    /// Index of texel `(i, j)` of `tile`'s cell.
    pub fn index(tile: u16, i: usize, j: usize) -> usize {
        let (x, y) = Self::cell_origin(tile);
        (y + j) * ATLAS_WIDTH + x + i
    }

    /// The heights as little-endian bytes (`R32Float`).
    pub fn height_bytes(&self) -> Vec<u8> {
        self.heights.iter().flat_map(|h| h.to_le_bytes()).collect()
    }

    /// The colours as bytes (`Rgba8Unorm`).
    pub fn color_bytes(&self) -> Vec<u8> {
        self.colors.iter().flatten().copied().collect()
    }
}

/// One mesh vertex (16 bytes): position relative to the cell's mesh origin
/// ([`cell_origin`]) in world units (y down), then the triangle's average
/// colour (RGB) and its [`flags`] (A). Triangles are non-indexed, three
/// vertices each, flat-coloured, in the game's winding: front faces are
/// clockwise on the game's screen (x right, y down; NCLIP > 0), and still
/// clockwise in NDC after the y flip (`FrontFace::Cw`; found by `fx-far`).
/// Walls are included.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub color: [u8; 4],
}

/// Every tile type's triangles; [`Tile::vertices`] is each type's range.
#[derive(Clone, Default)]
pub struct Mesh {
    pub vertices: Vec<MeshVertex>,
}

impl Mesh {
    /// The vertex buffer as little-endian bytes: per vertex `f32 x, y, z`
    /// then `u8 r, g, b, flags` (16 bytes; wgpu `Float32x3`, `Unorm8x4`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.vertices.len() * 16);
        for vertex in &self.vertices {
            for c in vertex.position {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
            bytes.extend_from_slice(&vertex.color);
        }
        bytes
    }
}

/// The detected sea.
#[derive(Clone, Debug, PartialEq)]
pub struct Sea {
    /// World y of the sea surface (y down; JP mission 1: -1024).
    pub level: f32,
    /// The tile type of the open sea.
    pub tile: u16,
    /// Texture keys of the sea tile's faces. Any face with one of them and
    /// every vertex within [`SEA_TOLERANCE`] of the level is sea (the shore
    /// tiles' water parts too).
    pub keys: Vec<TextureKey>,
    /// Grid cells of the sea tile type.
    pub cells: u32,
}

/// One texture key over the whole map (top-down, weighted by the grid
/// cells of each tile type, as of the build).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyStats {
    pub key: TextureKey,
    /// Faces using it, over all tile types.
    pub faces: u32,
    /// Tile types using it.
    pub tiles: u32,
    /// Map area it covers top-down, in tiles (2048 x 2048 units).
    pub area: f64,
    /// Mean modulated colour.
    pub mean_color: [u8; 3],
    /// Map area flagged water / forest / built, in tiles (by the texels
    /// whose majority key it is).
    pub water: f64,
    pub forest: f64,
    pub built: f64,
}

/// The texel and triangle classification: the colour heuristics of §2.4
/// (forest dark and green, built bright or grey or noisy), with thresholds
/// tuned on JP mission 1, whose palette is olive throughout; then per-key
/// flags, which replace the colour rules for that key. Water is the sea's
/// keys (or a key given [`flags::WATER`]) at the sea level.
///
/// A texel's colour input is the mean over its 3 x 3 neighbours of the same
/// key within the cell, its deviation its own samples'; a triangle's are
/// those of its texture over its UV area.
#[derive(Clone, Debug, PartialEq)]
pub struct Classifier {
    /// Forest: luma below this ...
    pub forest_luma: f32,
    /// ... green at least red, blue below this times green.
    pub forest_blue: f32,
    /// Built (when not forest): luma from this, ...
    pub built_luma: f32,
    /// ... or blue from this times green (grey, not olive), ...
    pub built_blue: f32,
    /// ... or a luma deviation of the samples above this.
    pub built_std: f32,
    /// Flags by texture key (a per-mission table), replacing the colour
    /// rules for texels and triangles of that key.
    pub keys: Vec<(TextureKey, u8)>,
}

impl Default for Classifier {
    /// Tuned on the texels of JP mission 1 by the class of their actual
    /// texture key (`tests/effects_terrain_map.rs`): forest texels have
    /// blue/green 0.52..0.77 and luma 21..61 (2nd..98th percentile), built
    /// ones 0.60..0.90 and 35..112; sample deviations are below 12.5 for 98%
    /// of forest and above 16 for about 12% of built. Measured: 99.6% of the
    /// forest area flagged forest, 90.8% of the built area built.
    fn default() -> Self {
        Self {
            forest_luma: 60.0,
            forest_blue: 0.74,
            built_luma: 60.0,
            built_blue: 0.74,
            built_std: 16.0,
            keys: Vec::new(),
        }
    }
}

impl Classifier {
    /// The colour rule for a mean colour and its luma deviation.
    pub fn color_flags(&self, rgb: [f32; 3], luma_std: f32) -> u8 {
        let [r, g, b] = rgb;
        let y = luma(rgb);
        let blue = b / g.max(1.0);
        if g >= r && blue < self.forest_blue && y < self.forest_luma {
            flags::FOREST
        } else if y >= self.built_luma || blue >= self.built_blue || luma_std > self.built_std {
            flags::BUILT
        } else {
            0
        }
    }

    /// Flags of a surface of `key` (without [`flags::COVERED`]); `sea_level`
    /// tells whether it lies at the sea level.
    fn flags(
        &self,
        key: TextureKey,
        sea: Option<&Sea>,
        sea_level: bool,
        rgb: [f32; 3],
        luma_std: f32,
    ) -> u8 {
        let given = self.keys.iter().find(|(k, _)| *k == key).map(|(_, f)| *f);
        let water_key = sea.is_some_and(|sea| sea.keys.contains(&key))
            || given.is_some_and(|f| f & flags::WATER != 0);
        if water_key && sea_level {
            return flags::WATER;
        }
        match given {
            Some(f) => f & (flags::FOREST | flags::BUILT),
            None => self.color_flags(rgb, luma_std),
        }
    }
}

/// How long the last build took, and what it saw.
#[derive(Clone, Debug, Default)]
pub struct BuildStats {
    pub duration: Duration,
    /// Of which: rasterizing and texturing the tiles (threaded).
    pub shading: Duration,
    pub tiles: usize,
    pub faces: usize,
    pub triangles: usize,
}

/// What [`TerrainMap::update`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// Another map (or its first build): upload everything.
    pub rebuilt: bool,
    /// Grid cells (x, z) whose tile type changed: upload those grid texels.
    pub cells: Vec<[u8; 2]>,
    /// Tile types whose textures changed: their atlas cell and mesh
    /// colours were rebuilt (the vertex ranges are unchanged).
    pub tiles: Vec<u16>,
}

impl Update {
    pub fn is_empty(&self) -> bool {
        !self.rebuilt && self.cells.is_empty() && self.tiles.is_empty()
    }
}

/// The camera of a flight frame (for placing the map; the renderer's scene
/// constants are the same words).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapCamera {
    /// Position in world units (camera record `+4..+0xC` / 4096).
    pub position: [f64; 3],
    /// View rotation rows (`world+0x4C`, 4.12 / 4096): `p_view = R (p -
    /// position)`; row 2 is the view direction.
    pub rotation: [[f32; 3]; 3],
    /// Projection distance (camera record `+0x24`).
    pub h: f32,
}

impl MapCamera {
    /// The camera of `ram`'s flight frame.
    pub fn read(ram: &[u8]) -> Result<Self, MapError> {
        let ram = Ram(ram);
        let world = ram.pointer("world", WORLD)?;
        let camera = ram.pointer("camera", world + 0x28)?;
        let word = |address: u32| ram.word(address).map_or(0, |w| w as i32);
        let position = [4, 8, 12].map(|o| f64::from(word(camera + o)) / 4096.0);
        let mut rotation = [[0.0; 3]; 3];
        for (r, row) in rotation.iter_mut().enumerate() {
            for (c, value) in row.iter_mut().enumerate() {
                let half = ram.half(world + 0x4c + 2 * (3 * r + c) as u32).unwrap_or(0);
                *value = f32::from(half as i16) / 4096.0;
            }
        }
        let h = f32::from(ram.half(camera + 0x24).unwrap_or(0) as i16);
        Ok(Self {
            position,
            rotation,
            h,
        })
    }

    /// The grid cell the camera is in (before the edge wrap).
    pub fn cell(&self) -> [i32; 2] {
        [world_cell(self.position[0]), world_cell(self.position[2])]
    }
}

/// The game's edge wrap of a grid index (8005E4CC on 16-bit indices, the
/// rays 8009A5D0 alike): below 0 it repeats cells 0..=7, from 256 on cells
/// 248..=255.
pub fn wrap_cell(cell: i32) -> usize {
    let cell = i32::from(cell as i16);
    let wrapped = if cell < 0 {
        cell & 7
    } else if cell >= 0x100 {
        (cell & 7) | 0xf8
    } else {
        cell
    };
    wrapped as usize
}

/// The grid index (before the wrap) of a world coordinate.
pub fn world_cell(coordinate: f64) -> i32 {
    (coordinate / f64::from(TILE_UNITS)).floor() as i32 + 128
}

/// World position of cell `(cx, cz)`'s mesh origin: its centre at y = 0.
pub fn cell_origin(cx: i32, cz: i32) -> [f64; 3] {
    let centre = |c: i32| f64::from((c - 128) * TILE_UNITS + TILE_HALF);
    [centre(cx), 0.0, centre(cz)]
}

/// One cell of [`TerrainMap::cells_around`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellInstance {
    /// Grid index before the wrap (may lie outside 0..=255).
    pub cell: [i32; 2],
    /// Its tile type after the wrap (never 0).
    pub tile: u16,
}

/// The mission's terrain, ready to upload.
#[derive(Clone)]
pub struct TerrainMap {
    /// Tile type per cell, `z * 256 + x` (`R16Uint`, 256 x 256).
    pub grid: Vec<u16>,
    /// Per tile type `0..=count` (type 0: no terrain).
    pub tiles: Vec<Tile>,
    pub atlas: Atlas,
    pub mesh: Mesh,
    pub sea: Option<Sea>,
    /// The texture keys, largest map area first (as of the build).
    pub keys: Vec<KeyStats>,
    /// The rules the flags were made with ([`TerrainMap::classify`]).
    pub classifier: Classifier,
    pub stats: BuildStats,
    layout: Layout,
    /// The grid's bytes as last read (with the flag bits).
    raw_grid: Vec<u8>,
    raw_tiles: Vec<RawTile>,
    /// Per mesh triangle: its average colour, luma deviation and key index.
    triangles: Vec<TriangleStats>,
    /// Per texture key: the VRAM it reads, its hash and the tiles using it.
    textures: BTreeMap<TextureKey, TextureUse>,
}

impl TerrainMap {
    /// Build the whole map, classified with [`Classifier::default`].
    pub fn build(source: &Source) -> Result<Self, MapError> {
        let started = Instant::now();
        if source.vram.len() != 1024 * 512 {
            return Err(MapError::Vram(source.vram.len()));
        }
        let ram = Ram(source.ram);
        let layout = Layout::read(&ram)?;
        let raw_grid = grid_bytes(&ram, layout.grid)?.to_vec();
        let mut raw_tiles = vec![RawTile::default()];
        for &address in &layout.meshes[1..] {
            raw_tiles.push(RawTile::read(&ram, address)?);
        }
        let mut map = Self {
            grid: raw_grid
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]) & 0x1ff)
                .collect(),
            tiles: vec![Tile::default(); raw_tiles.len()],
            atlas: Atlas::new(),
            mesh: Mesh::default(),
            sea: None,
            keys: Vec::new(),
            classifier: Classifier::default(),
            stats: BuildStats::default(),
            layout,
            raw_grid,
            raw_tiles,
            triangles: Vec::new(),
            textures: BTreeMap::new(),
        };
        map.count_cells();
        // Pass 1: geometry, texels and statistics of every tile type.
        let shading = Instant::now();
        let works = shade_tiles(&map.raw_tiles.iter().collect::<Vec<_>>(), source.vram);
        let shading = shading.elapsed();
        map.keys = key_stats(&map.raw_tiles, &works, &map.tiles);
        for (index, (raw, work)) in map.raw_tiles.iter().zip(&works).enumerate() {
            let tile = &mut map.tiles[index];
            tile.address = raw.address;
            tile.floor = raw.floor;
            tile.top = raw.top;
            tile.faces = raw.faces.len() as u32;
            let first = map.mesh.vertices.len() as u32;
            tile.vertices = first..first + 3 * work.triangles.len() as u32;
            map.mesh
                .vertices
                .resize(tile.vertices.end as usize, MeshVertex::default());
        }
        map.triangles = vec![TriangleStats::default(); map.mesh.vertices.len() / 3];
        for (index, work) in works.iter().enumerate() {
            map.store_shading(index as u16, work);
        }
        // The sea, then pass 2: the flags.
        map.sea = detect_sea(&map.raw_tiles, &works, &map.tiles);
        map.classify(Classifier::default());
        map.textures = texture_uses(&map.raw_tiles, source.vram);
        map.stats = BuildStats {
            duration: started.elapsed(),
            shading,
            tiles: map.raw_tiles.len() - 1,
            faces: map.raw_tiles.iter().map(|t| t.faces.len()).sum(),
            triangles: map.triangles.len(),
        };
        Ok(map)
    }

    /// Bring the map up to date with `source`: a full rebuild for another
    /// map (layout words, directory or mesh headers differ), else the
    /// changed grid cells and the tile types whose texture VRAM changed.
    /// A rebuild keeps the classifier.
    pub fn update(&mut self, source: &Source) -> Result<Update, MapError> {
        let ram = Ram(source.ram);
        let layout = Layout::read(&ram)?;
        if layout != self.layout {
            let classifier = self.classifier.clone();
            *self = Self::build(source)?;
            if classifier != self.classifier {
                self.classify(classifier);
            }
            return Ok(Update {
                rebuilt: true,
                ..Update::default()
            });
        }
        let mut update = Update::default();
        // One memcmp of the 128 KiB; cell by cell only when it differs.
        let grid = grid_bytes(&ram, layout.grid)?;
        if grid != self.raw_grid.as_slice() {
            for (index, (old, new)) in self
                .raw_grid
                .chunks_exact(2)
                .zip(grid.chunks_exact(2))
                .enumerate()
            {
                let old = u16::from_le_bytes([old[0], old[1]]);
                let new = u16::from_le_bytes([new[0], new[1]]);
                if (old ^ new) & 0x1ff != 0 {
                    update
                        .cells
                        .push([(index % GRID_SIZE) as u8, (index / GRID_SIZE) as u8]);
                    self.grid[index] = new & 0x1ff;
                }
            }
            self.raw_grid.copy_from_slice(grid);
            if !update.cells.is_empty() {
                self.count_cells();
            }
        }
        let mut changed = BTreeSet::new();
        for texture in self.textures.values_mut() {
            if texture.changed(source.vram) {
                texture.refresh(source.vram);
                changed.extend(texture.tiles.iter().copied());
            }
        }
        if !changed.is_empty() {
            update.tiles = changed.into_iter().collect();
            let raws: Vec<&RawTile> = update
                .tiles
                .iter()
                .map(|&t| &self.raw_tiles[usize::from(t)])
                .collect();
            let works = shade_tiles(&raws, source.vram);
            for (&tile, work) in update.tiles.iter().zip(&works) {
                self.store_shading(tile, work);
            }
            self.classify(self.classifier.clone());
        }
        Ok(update)
    }

    /// Flag every atlas texel and mesh triangle with `classifier` (cheap: no
    /// texturing), and redo the per-key and per-tile flag summaries.
    pub fn classify(&mut self, classifier: Classifier) {
        self.classifier = classifier;
        for stat in &mut self.keys {
            stat.water = 0.0;
            stat.forest = 0.0;
            stat.built = 0.0;
        }
        let texel_area = 1.0 / (CELL_TEXELS * CELL_TEXELS) as f64;
        for index in 1..self.tiles.len() {
            let tile = index as u16;
            let mut votes = [0usize; 8];
            let area = texel_area * f64::from(self.tiles[index].cells);
            let colors = self.neighbourhood_colors(tile);
            for j in 0..CELL_TEXELS {
                for i in 0..CELL_TEXELS {
                    let at = Atlas::index(tile, i, j);
                    let key = self.atlas.keys[at];
                    let flag = if key == NO_KEY {
                        0
                    } else {
                        let height = self.atlas.heights[at];
                        let at_sea = self
                            .sea
                            .as_ref()
                            .is_some_and(|sea| (height - sea.level).abs() <= SEA_TOLERANCE);
                        let rgb = colors[j * CELL_TEXELS + i];
                        let stat = &mut self.keys[usize::from(key)];
                        let flag = flags::COVERED
                            | self.classifier.flags(
                                stat.key,
                                self.sea.as_ref(),
                                at_sea,
                                rgb,
                                self.atlas.luma_std[at],
                            );
                        if flag & flags::WATER != 0 {
                            stat.water += area;
                        }
                        if flag & flags::FOREST != 0 {
                            stat.forest += area;
                        }
                        if flag & flags::BUILT != 0 {
                            stat.built += area;
                        }
                        flag
                    };
                    self.atlas.flags[at] = flag;
                    for (bit, vote) in votes.iter_mut().enumerate() {
                        if flag & (1 << bit) != 0 {
                            *vote += 1;
                        }
                    }
                }
            }
            self.tiles[index].flags = votes
                .iter()
                .enumerate()
                .filter(|(_, v)| **v * 2 > CELL_TEXELS * CELL_TEXELS)
                .fold(0, |f, (bit, _)| f | (1 << bit));
        }
        for (k, triangle) in self.triangles.iter().enumerate() {
            let corners = &mut self.mesh.vertices[3 * k..3 * k + 3];
            let at_sea = self.sea.as_ref().is_some_and(|sea| {
                corners
                    .iter()
                    .all(|v| (v.position[1] - sea.level).abs() <= SEA_TOLERANCE)
            });
            let Some(key) = self.keys.get(usize::from(triangle.key)).map(|k| k.key) else {
                continue;
            };
            let flag = flags::COVERED
                | self.classifier.flags(
                    key,
                    self.sea.as_ref(),
                    at_sea,
                    triangle.rgb,
                    triangle.luma_std,
                );
            for vertex in corners {
                vertex.color[3] = flag;
            }
        }
    }

    /// Per texel of `tile`'s cell (row-major), the mean colour of the texel
    /// and its neighbours within the cell that show the same texture key:
    /// the colour rules' input (a 3 x 3 texel window, 192 units, steadies
    /// the hue of noisy canopy textures without blurring across texture
    /// edges).
    fn neighbourhood_colors(&self, tile: u16) -> Vec<[f32; 3]> {
        const N: usize = CELL_TEXELS;
        // (colour, key, textured) per texel of the cell.
        let texels: Vec<([f32; 3], u16, bool)> = (0..N * N)
            .map(|t| {
                let at = Atlas::index(tile, t % N, t / N);
                let [r, g, b, a] = self.atlas.colors[at];
                ([r, g, b].map(f32::from), self.atlas.keys[at], a != 0)
            })
            .collect();
        texels
            .iter()
            .enumerate()
            .map(|(t, &(color, key, _))| {
                let (i, j) = (t % N, t / N);
                let mut sum = [0.0f32; 3];
                let mut n = 0.0f32;
                for nj in j.saturating_sub(1)..(j + 2).min(N) {
                    for ni in i.saturating_sub(1)..(i + 2).min(N) {
                        let (other, other_key, textured) = texels[nj * N + ni];
                        if other_key == key && textured {
                            for (s, c) in sum.iter_mut().zip(other) {
                                *s += c;
                            }
                            n += 1.0;
                        }
                    }
                }
                if n == 0.0 { color } else { sum.map(|s| s / n) }
            })
            .collect()
    }

    /// Tile type of grid cell `(cx, cz)` after the edge wrap.
    pub fn tile_at(&self, cx: i32, cz: i32) -> u16 {
        self.grid[wrap_cell(cz) * GRID_SIZE + wrap_cell(cx)]
    }

    /// The cells of the +-`radius` square around `(cx, cz)` that have
    /// terrain, row by row (for instancing the tile meshes).
    pub fn cells_around(&self, cx: i32, cz: i32, radius: i32) -> Vec<CellInstance> {
        let mut cells = Vec::new();
        for z in cz - radius..=cz + radius {
            for x in cx - radius..=cx + radius {
                let tile = self.tile_at(x, z);
                if tile != 0 {
                    cells.push(CellInstance { cell: [x, z], tile });
                }
            }
        }
        cells
    }

    /// Atlas index of the texel over world `(x, z)`, `None` over a cell
    /// without terrain.
    pub fn texel_index(&self, x: f64, z: f64) -> Option<usize> {
        let tx = (x / f64::from(TEXEL_UNITS)).floor() as i64;
        let tz = (z / f64::from(TEXEL_UNITS)).floor() as i64;
        self.texel_index_at(tx, tz)
    }

    /// [`Self::texel_index`] by global texel indices (`floor(x / 64)`).
    pub fn texel_index_at(&self, tx: i64, tz: i64) -> Option<usize> {
        let texels = CELL_TEXELS as i64;
        let cx = (tx.div_euclid(texels) + 128) as i32;
        let cz = (tz.div_euclid(texels) + 128) as i32;
        let tile = self.tile_at(cx, cz);
        (tile != 0).then(|| {
            Atlas::index(
                tile,
                tx.rem_euclid(texels) as usize,
                tz.rem_euclid(texels) as usize,
            )
        })
    }

    /// Terrain height (world y) at `(x, z)`, bilinear between texel
    /// centres as a shader would filter it through the grid; `None` over a
    /// cell without terrain.
    pub fn height_at(&self, x: f64, z: f64) -> Option<f32> {
        let fx = x / f64::from(TEXEL_UNITS) - 0.5;
        let fz = z / f64::from(TEXEL_UNITS) - 0.5;
        let (tx, tz) = (fx.floor(), fz.floor());
        let (ax, az) = ((fx - tx) as f32, (fz - tz) as f32);
        let (tx, tz) = (tx as i64, tz as i64);
        let h = |dx: i64, dz: i64| {
            self.texel_index_at(tx + dx, tz + dz)
                .map(|i| self.atlas.heights[i])
        };
        let (h00, h10, h01, h11) = (h(0, 0)?, h(1, 0)?, h(0, 1)?, h(1, 1)?);
        let top = h00 + (h10 - h00) * ax;
        let bottom = h01 + (h11 - h01) * ax;
        Some(top + (bottom - top) * az)
    }

    fn count_cells(&mut self) {
        for tile in &mut self.tiles {
            tile.cells = 0;
        }
        for &cell in &self.grid {
            if let Some(tile) = self.tiles.get_mut(usize::from(cell)) {
                tile.cells += 1;
            }
        }
    }

    /// Pass-1 results of one tile type into the atlas, the mesh and its
    /// summary.
    fn store_shading(&mut self, index: u16, work: &TileWork) {
        let indices: BTreeMap<TextureKey, u16> = self
            .keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.key, i as u16))
            .collect();
        let key_index = |key: TextureKey| indices.get(&key).copied().unwrap_or(NO_KEY);
        let mut color_sum = [0.0f64; 3];
        let mut colored = 0usize;
        let mut height_sum = 0.0f64;
        for (t, texel) in work.texels.iter().enumerate() {
            let at = Atlas::index(index, t % CELL_TEXELS, t / CELL_TEXELS);
            if texel.colored {
                for (sum, c) in color_sum.iter_mut().zip(texel.rgb) {
                    *sum += f64::from(c);
                }
                colored += 1;
            }
            height_sum += f64::from(texel.height);
            self.atlas.heights[at] = texel.height;
            self.atlas.luma_std[at] = texel.luma_std;
            self.atlas.keys[at] = texel.key.map_or(NO_KEY, key_index);
        }
        let mean = if colored > 0 {
            color_sum.map(|s| to_u8((s / colored as f64) as f32))
        } else {
            [0; 3]
        };
        for (t, texel) in work.texels.iter().enumerate() {
            let at = Atlas::index(index, t % CELL_TEXELS, t / CELL_TEXELS);
            self.atlas.colors[at] = if texel.colored {
                let [r, g, b] = texel.rgb.map(to_u8);
                [r, g, b, 255]
            } else {
                [mean[0], mean[1], mean[2], 0]
            };
        }
        let tile = &mut self.tiles[usize::from(index)];
        tile.mean_color = [mean[0], mean[1], mean[2], 255];
        tile.mean_height = (height_sum / work.texels.len().max(1) as f64) as f32;
        tile.luma_std = work.luma_std;
        tile.coverage = work.coverage;
        let first = tile.vertices.start as usize;
        for (k, triangle) in work.triangles.iter().enumerate() {
            let [r, g, b] = triangle.rgb.map(to_u8);
            for (corner, position) in triangle.positions.iter().enumerate() {
                let vertex = &mut self.mesh.vertices[first + 3 * k + corner];
                vertex.position = *position;
                vertex.color = [r, g, b, vertex.color[3]];
            }
            self.triangles[first / 3 + k] = TriangleStats {
                rgb: triangle.rgb,
                luma_std: triangle.luma_std,
                key: key_index(triangle.key),
            };
        }
    }
}

/// `[gp+0x2E0]`: the world block pointer.
const WORLD: u32 = ac3_flight_jp::GP + ac3_flight_jp::common::GAMEPLAY;

// ---- reading RAM -------------------------------------------------------------------

/// Main RAM by guest address (KUSEG, KSEG0 and KSEG1 mirrors).
#[derive(Clone, Copy)]
struct Ram<'a>(&'a [u8]);

impl Ram<'_> {
    fn offset(&self, address: u32, width: usize) -> Option<usize> {
        let physical = (address & 0x1fff_ffff) as usize;
        let segment = address >> 29;
        let inside = physical.is_multiple_of(width) && physical + width <= self.0.len();
        (matches!(segment, 0 | 4 | 5) && inside).then_some(physical)
    }
    fn word(&self, address: u32) -> Option<u32> {
        let o = self.offset(address, 4)?;
        Some(u32::from_le_bytes([
            self.0[o],
            self.0[o + 1],
            self.0[o + 2],
            self.0[o + 3],
        ]))
    }
    fn half(&self, address: u32) -> Option<u16> {
        let o = self.offset(address, 2)?;
        Some(u16::from_le_bytes([self.0[o], self.0[o + 1]]))
    }
    /// The word at `address`, which must point into RAM.
    fn pointer(&self, what: &'static str, address: u32) -> Result<u32, MapError> {
        let value = self
            .word(address)
            .ok_or(MapError::Pointer { what, address })?;
        self.offset(value, 4)
            .map(|_| value)
            .ok_or(MapError::Pointer {
                what,
                address: value,
            })
    }
}

/// The words that identify a map: a change means another map.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Layout {
    world: u32,
    grid: u32,
    directory: u32,
    /// Mesh pointer per tile type (index 0 unused).
    meshes: Vec<u32>,
    /// Hash of the mesh headers and their vertex and face counts.
    headers: u64,
}

impl Layout {
    fn read(ram: &Ram) -> Result<Self, MapError> {
        let world = ram.pointer("world", WORLD)?;
        let grid = ram.pointer("grid", world + 0x248)?;
        let end = grid + (GRID_SIZE * GRID_SIZE * 2) as u32 - 2;
        if ram.offset(end, 2).is_none() {
            return Err(MapError::Pointer {
                what: "grid end",
                address: end,
            });
        }
        let directory = ram.pointer("tile directory", world + 0x24c)?;
        let count = ram.word(directory).unwrap_or(0);
        if count == 0 || count as usize >= MAX_TILE_TYPES {
            return Err(MapError::Count {
                what: "tile type",
                count,
            });
        }
        let mut meshes = vec![0];
        let mut hash = Fnv::new();
        for t in 1..=count {
            let mesh = ram.pointer("tile mesh", directory + 4 * t)?;
            meshes.push(mesh);
            for offset in (0..0x18).step_by(4) {
                hash.word(ram.word(mesh + offset).unwrap_or(0));
            }
            // The vertex and face counts the header points to.
            for offset in [4, 0xc, 0x10] {
                let list = ram.word(mesh + offset).unwrap_or(0);
                hash.word(ram.word(list).unwrap_or(0));
            }
        }
        Ok(Self {
            world,
            grid,
            directory,
            meshes,
            headers: hash.finish(),
        })
    }
}

/// The grid's 128 KiB in RAM.
fn grid_bytes<'a>(ram: &Ram<'a>, grid: u32) -> Result<&'a [u8], MapError> {
    let bad = MapError::Pointer {
        what: "grid",
        address: grid,
    };
    let start = ram.offset(grid, 2).ok_or(bad.clone())?;
    ram.0
        .get(start..start + GRID_SIZE * GRID_SIZE * 2)
        .ok_or(bad)
}

/// One face as stored.
#[derive(Clone, Debug, Default)]
struct RawFace {
    indices: [u8; 4],
    uv: [[u8; 2]; 4],
    key: TextureKey,
    colors: [[u8; 3]; 4],
}

/// One tile mesh as stored.
#[derive(Clone, Debug, Default)]
struct RawTile {
    address: u32,
    floor: i16,
    top: i16,
    vertices: Vec<[i16; 3]>,
    faces: Vec<RawFace>,
}

impl RawTile {
    fn read(ram: &Ram, address: u32) -> Result<Self, MapError> {
        let bad = |what| MapError::Pointer { what, address };
        let half = |a: u32| ram.half(a).map(|h| h as i16).ok_or(bad("mesh data"));
        let list = ram.pointer("vertex list", address + 4)?;
        let count = ram.word(list).ok_or(bad("vertex list"))?;
        if count > 255 {
            return Err(MapError::Count {
                what: "tile vertex",
                count,
            });
        }
        // The drawing projects whole groups of three.
        let padded = count.div_ceil(3) * 3;
        let mut vertices = Vec::with_capacity(padded as usize);
        for k in 0..padded {
            let base = list + 4 + 20 * (k / 3);
            let i = k % 3;
            vertices.push([
                half(base + 4 * i)?,
                half(base + 4 * i + 2)?,
                half(base + 12 + 2 * i)?,
            ]);
        }
        let mut faces = Vec::new();
        for (offset, stride, gouraud) in [(0xc, 0x24, false), (0x10, 0x30, true)] {
            let faces_at = ram.pointer("face list", address + offset)?;
            let n = ram.word(faces_at).ok_or(bad("face list"))?;
            if n > 4096 {
                return Err(MapError::Count {
                    what: "tile face",
                    count: n,
                });
            }
            for f in 0..n {
                let face = faces_at + 4 + stride * f;
                let word = |o: u32| ram.word(face + o).ok_or(bad("face"));
                let indices = word(0x10)?.to_le_bytes();
                let uv01 = [word(0x14)?, word(0x18)?];
                let uv23 = word(0x1c)?;
                let uv = [
                    [uv01[0] as u8, (uv01[0] >> 8) as u8],
                    [uv01[1] as u8, (uv01[1] >> 8) as u8],
                    [uv23 as u8, (uv23 >> 8) as u8],
                    [(uv23 >> 16) as u8, (uv23 >> 24) as u8],
                ];
                let key = TextureKey {
                    tpage: (uv01[1] >> 16) as u16,
                    clut: (uv01[0] >> 16) as u16,
                };
                let color = |o: u32| -> Result<[u8; 3], MapError> {
                    let [r, g, b, _] = word(o)?.to_le_bytes();
                    Ok([r, g, b])
                };
                let colors = if gouraud {
                    [color(0x20)?, color(0x24)?, color(0x28)?, color(0x2c)?]
                } else {
                    [color(0x20)?; 4]
                };
                faces.push(RawFace {
                    indices,
                    uv,
                    key,
                    colors,
                });
            }
        }
        Ok(Self {
            address,
            floor: half(address + 0x14)?,
            top: half(address + 0x16)?,
            vertices,
            faces,
        })
    }

    /// The faces' triangles as the GPU draws them: (v0, v1, v2) and (v1,
    /// v3, v2), without repeated vertices or zero area.
    fn triangles(&self, vram: &[u16]) -> Vec<Triangle> {
        let mut triangles = Vec::new();
        for face in &self.faces {
            if face
                .indices
                .iter()
                .any(|&i| usize::from(i) >= self.vertices.len())
            {
                continue;
            }
            let palette = Palette::new(vram, face.key);
            for corners in [[0, 1, 2], [1, 3, 2]] {
                let [a, b, c] = corners.map(|k| face.indices[k]);
                if a == b || b == c || a == c {
                    continue;
                }
                let p = [a, b, c].map(|i| self.vertices[usize::from(i)].map(f32::from));
                let n = cross(sub(p[1], p[0]), sub(p[2], p[0]));
                if n.iter().all(|c| c.abs() < 1e-3) {
                    continue;
                }
                let mut lo = [u8::MAX; 2];
                let mut hi = [0; 2];
                for k in corners {
                    for axis in 0..2 {
                        lo[axis] = lo[axis].min(face.uv[k][axis]);
                        hi[axis] = hi[axis].max(face.uv[k][axis]);
                    }
                }
                triangles.push(Triangle {
                    positions: p,
                    uv: corners.map(|k| face.uv[k].map(f32::from)),
                    colors: corners.map(|k| face.colors[k].map(f32::from)),
                    key: face.key,
                    palette: palette.clone(),
                    uv_lo: lo.map(f32::from),
                    uv_hi: hi.map(f32::from),
                });
            }
        }
        triangles
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// A texture key resolved for sampling: the page origin and depth, the
/// 4-bit CLUT decoded once.
#[derive(Clone, Debug)]
enum Palette {
    Four { x: usize, y: usize, clut: [u16; 16] },
    Eight { x: usize, y: usize, clut: u16 },
    Direct { x: usize, y: usize },
    Reserved,
}

impl Palette {
    fn new(vram: &[u16], key: TextureKey) -> Self {
        let x = usize::from(key.tpage & 15) * 64;
        let y = usize::from((key.tpage >> 4) & 1) * 256;
        match (key.tpage >> 7) & 3 {
            0 => Self::Four {
                x,
                y,
                clut: std::array::from_fn(|i| clut_entry(vram, key.clut, i)),
            },
            1 => Self::Eight {
                x,
                y,
                clut: key.clut,
            },
            2 => Self::Direct { x, y },
            _ => Self::Reserved,
        }
    }

    /// The 15-bit texel at `(u, v)`; `None` for 0x0000 (transparent).
    fn texel(&self, vram: &[u16], u: u8, v: u8) -> Option<u16> {
        let (u, v) = (usize::from(u), usize::from(v));
        let texel = match self {
            Self::Four { x, y, clut } => {
                let word = vram[((y + v) & 511) * 1024 + ((x + u / 4) & 1023)];
                clut[usize::from((word >> (4 * (u & 3))) & 15)]
            }
            Self::Eight { x, y, clut } => {
                let word = vram[((y + v) & 511) * 1024 + ((x + u / 2) & 1023)];
                clut_entry(vram, *clut, usize::from((word >> (8 * (u & 1))) & 255))
            }
            Self::Direct { x, y } => vram[((y + v) & 511) * 1024 + ((x + u) & 1023)],
            Self::Reserved => 0,
        };
        (texel != 0).then_some(texel)
    }
}

/// One drawable triangle of a tile mesh.
#[derive(Clone, Debug)]
struct Triangle {
    positions: [[f32; 3]; 3],
    uv: [[f32; 2]; 3],
    colors: [[f32; 3]; 3],
    key: TextureKey,
    palette: Palette,
    uv_lo: [f32; 2],
    uv_hi: [f32; 2],
}

impl Triangle {
    /// Modulated colour at barycentric weights `(w1, w2)`; `None` for a
    /// transparent texel.
    fn shade(&self, vram: &[u16], w1: f32, w2: f32) -> Option<[f32; 3]> {
        let w0 = 1.0 - w1 - w2;
        let at = |axis: usize| {
            let value = w0 * self.uv[0][axis] + w1 * self.uv[1][axis] + w2 * self.uv[2][axis];
            value.floor().clamp(self.uv_lo[axis], self.uv_hi[axis]) as u8
        };
        let texel = self.palette.texel(vram, at(0), at(1))?;
        let color = [0, 1, 2]
            .map(|c| w0 * self.colors[0][c] + w1 * self.colors[1][c] + w2 * self.colors[2][c]);
        Some(modulate(texel, color))
    }
}

// ---- VRAM --------------------------------------------------------------------------

/// The CLUT entry `index` of `clut`, read from the unfogged source when it
/// falls in the fog block.
pub fn clut_entry(vram: &[u16], clut: u16, index: usize) -> u16 {
    vram[clut_address(clut, index)]
}

/// The VRAM word [`clut_entry`] reads.
fn clut_address(clut: u16, index: usize) -> usize {
    let x = (usize::from(clut & 63) * 16 + index) & 1023;
    let mut y = usize::from((clut >> 6) & 511);
    let (fx, fy, fw, fh) = FOG_BLOCK;
    if (fx..fx + fw).contains(&x) && (fy..fy + fh).contains(&y) {
        y += FOG_SOURCE_ROWS;
    }
    y * 1024 + x
}

/// The 15-bit texel at `(u, v)` of `key` (4-bit, 8-bit or direct), as the
/// GPU samples it without a texture window, through [`clut_entry`]. `None`
/// for 0x0000 (transparent) and the reserved depth.
pub fn sample_texture(vram: &[u16], key: TextureKey, u: u8, v: u8) -> Option<u16> {
    Palette::new(vram, key).texel(vram, u, v)
}

/// The GPU's texture modulation: texel x colour / 128, per channel, in
/// 0..255 (5-bit texels expanded).
fn modulate(texel: u16, color: [f32; 3]) -> [f32; 3] {
    let channel = |shift: u16, c: f32| {
        let t = f32::from((texel >> shift) & 31) * (255.0 / 31.0);
        (t * c / 128.0).min(255.0)
    };
    [
        channel(0, color[0]),
        channel(5, color[1]),
        channel(10, color[2]),
    ]
}

fn luma(rgb: [f32; 3]) -> f32 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

fn to_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// The VRAM a texture key reads: its CLUT entries (through the fog
/// redirect) and the texel rows of its faces' UV rectangle, as runs of
/// consecutive words, with their contents when the tiles were last shaded.
#[derive(Clone, Debug)]
struct TextureUse {
    /// (first word, length) runs.
    spans: Vec<(usize, usize)>,
    words: Vec<u16>,
    tiles: Vec<u16>,
}

impl TextureUse {
    fn new(key: TextureKey, rect: [u8; 4], tiles: Vec<u16>, vram: &[u16]) -> Self {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut push = |address: usize| match spans.last_mut() {
            Some((start, len)) if *start + *len == address => *len += 1,
            _ => spans.push((address, 1)),
        };
        let depth = (key.tpage >> 7) & 3;
        let entries = match depth {
            0 => 16,
            1 => 256,
            _ => 0,
        };
        for index in 0..entries {
            push(clut_address(key.clut, index));
        }
        let x = usize::from(key.tpage & 15) * 64;
        let y = usize::from((key.tpage >> 4) & 1) * 256;
        let [u0, v0, u1, v1] = rect.map(usize::from);
        let (w0, w1) = match depth {
            0 => (u0 / 4, u1 / 4),
            1 => (u0 / 2, u1 / 2),
            _ => (u0, u1),
        };
        for v in v0..=v1 {
            for w in w0..=w1 {
                push(((y + v) & 511) * 1024 + ((x + w) & 1023));
            }
        }
        let mut texture = Self {
            spans,
            words: Vec::new(),
            tiles,
        };
        texture.refresh(vram);
        texture
    }

    /// Whether the VRAM differs from the kept words.
    fn changed(&self, vram: &[u16]) -> bool {
        let mut offset = 0;
        for &(start, len) in &self.spans {
            if vram[start..start + len] != self.words[offset..offset + len] {
                return true;
            }
            offset += len;
        }
        false
    }

    fn refresh(&mut self, vram: &[u16]) {
        self.words.clear();
        for &(start, len) in &self.spans {
            self.words.extend_from_slice(&vram[start..start + len]);
        }
    }
}

/// Every texture key's [`TextureUse`] (the union of its faces' UVs, and
/// the tile types using it).
fn texture_uses(tiles: &[RawTile], vram: &[u16]) -> BTreeMap<TextureKey, TextureUse> {
    let mut rects: BTreeMap<TextureKey, ([u8; 4], Vec<u16>)> = BTreeMap::new();
    for (index, tile) in tiles.iter().enumerate() {
        for face in &tile.faces {
            let (rect, users) = rects
                .entry(face.key)
                .or_insert(([u8::MAX, u8::MAX, 0, 0], Vec::new()));
            for uv in face.uv {
                rect[0] = rect[0].min(uv[0]);
                rect[1] = rect[1].min(uv[1]);
                rect[2] = rect[2].max(uv[0]);
                rect[3] = rect[3].max(uv[1]);
            }
            if users.last() != Some(&(index as u16)) {
                users.push(index as u16);
            }
        }
    }
    rects
        .into_iter()
        .map(|(key, (rect, users))| (key, TextureUse::new(key, rect, users, vram)))
        .collect()
}

/// 64-bit FNV-1a.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn byte(&mut self, byte: u8) {
        self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    fn word(&mut self, value: u32) {
        value.to_le_bytes().into_iter().for_each(|b| self.byte(b));
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

// ---- rasterizing a tile --------------------------------------------------------------

/// The top-down sample grid of one tile (64 x 64 samples, 32 units
/// apart): the topmost triangle per sample and its weights.
struct Raster {
    best_y: Vec<f32>,
    triangle: Vec<u32>,
    weights: Vec<[f32; 2]>,
}

impl Raster {
    fn new() -> Self {
        Self {
            best_y: vec![f32::INFINITY; SAMPLES * SAMPLES],
            triangle: vec![NO_TRIANGLE; SAMPLES * SAMPLES],
            weights: vec![[0.0; 2]; SAMPLES * SAMPLES],
        }
    }

    fn clear(&mut self) {
        self.best_y.fill(f32::INFINITY);
        self.triangle.fill(NO_TRIANGLE);
    }

    /// Keep, per sample, the highest (smallest y) surface over it.
    fn draw(&mut self, index: usize, triangle: &Triangle) {
        let [a, b, c] = triangle.positions;
        let area = (b[0] - a[0]) * (c[2] - a[2]) - (b[2] - a[2]) * (c[0] - a[0]);
        if area.abs() < 1.0 {
            return; // a wall: no top-down area
        }
        let inverse = 1.0 / area;
        let range = |axis: usize| {
            let lo = a[axis].min(b[axis]).min(c[axis]);
            let hi = a[axis].max(b[axis]).max(c[axis]);
            let first = ((lo + TILE_HALF as f32) / SAMPLE_UNITS - 0.5)
                .ceil()
                .max(0.0) as usize;
            let last = ((hi + TILE_HALF as f32) / SAMPLE_UNITS - 0.5).floor();
            // Past the end (or before the start): an empty range.
            let end = if last < 0.0 {
                0
            } else {
                (last as usize + 1).min(SAMPLES)
            };
            first.min(end)..end
        };
        const EPSILON: f32 = -1e-4;
        for kz in range(2) {
            let pz = -(TILE_HALF as f32) + (kz as f32 + 0.5) * SAMPLE_UNITS;
            for kx in range(0) {
                let px = -(TILE_HALF as f32) + (kx as f32 + 0.5) * SAMPLE_UNITS;
                let w1 = ((px - a[0]) * (c[2] - a[2]) - (pz - a[2]) * (c[0] - a[0])) * inverse;
                let w2 = ((b[0] - a[0]) * (pz - a[2]) - (b[2] - a[2]) * (px - a[0])) * inverse;
                let w0 = 1.0 - w1 - w2;
                if w0 < EPSILON || w1 < EPSILON || w2 < EPSILON {
                    continue;
                }
                let y = w0 * a[1] + w1 * b[1] + w2 * c[1];
                let s = kz * SAMPLES + kx;
                if y < self.best_y[s] {
                    self.best_y[s] = y;
                    self.triangle[s] = index as u32;
                    self.weights[s] = [w1, w2];
                }
            }
        }
    }
}

/// Pass-1 result of one texel.
#[derive(Clone, Copy, Debug, Default)]
struct TexelWork {
    rgb: [f32; 3],
    height: f32,
    luma_std: f32,
    colored: bool,
    /// The texture key most of its samples show (`None`: no surface).
    key: Option<TextureKey>,
}

/// Pass-1 result of one triangle.
#[derive(Clone, Debug)]
struct TriangleWork {
    positions: [[f32; 3]; 3],
    rgb: [f32; 3],
    luma_std: f32,
    key: TextureKey,
}

/// What the classification keeps of a mesh triangle.
#[derive(Clone, Copy, Debug, Default)]
struct TriangleStats {
    rgb: [f32; 3],
    luma_std: f32,
    /// Index into [`TerrainMap::keys`].
    key: u16,
}

/// Pass-1 result of one tile type.
#[derive(Clone, Debug, Default)]
struct TileWork {
    texels: Vec<TexelWork>,
    triangles: Vec<TriangleWork>,
    /// Over all colour samples.
    mean: [f32; 3],
    luma_std: f32,
    coverage: f32,
    /// Per key: colour samples and their colour sum.
    key_samples: Vec<(TextureKey, u32, [f64; 3])>,
}

/// Pass 1 of every tile type, the tiles spread over a few threads (each
/// tile's result depends on its own data only).
fn shade_tiles(raws: &[&RawTile], vram: &[u16]) -> Vec<TileWork> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8);
    let chunk = raws.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = raws
            .chunks(chunk)
            .map(|tiles| {
                scope.spawn(move || {
                    let mut raster = Raster::new();
                    tiles
                        .iter()
                        .map(|raw| shade_tile(raw, vram, &mut raster))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// Rasterize one tile type top-down and average its triangles' textures.
fn shade_tile(raw: &RawTile, vram: &[u16], raster: &mut Raster) -> TileWork {
    let triangles = raw.triangles(vram);
    let mut work = TileWork {
        texels: vec![TexelWork::default(); CELL_TEXELS * CELL_TEXELS],
        ..TileWork::default()
    };
    // The tile's texture keys, by small index.
    let mut keys: Vec<TextureKey> = triangles.iter().map(|t| t.key).collect();
    keys.sort();
    keys.dedup();
    let key_of: Vec<usize> = triangles
        .iter()
        .map(|t| keys.binary_search(&t.key).unwrap_or(0))
        .collect();
    let mut key_samples = vec![(0u32, [0.0f64; 3]); keys.len()];
    raster.clear();
    for (index, triangle) in triangles.iter().enumerate() {
        raster.draw(index, triangle);
    }
    let mut all = Moments::default();
    let mut hits = 0usize;
    let mut votes = vec![0u32; keys.len()];
    for j in 0..CELL_TEXELS {
        for i in 0..CELL_TEXELS {
            let mut moments = Moments::default();
            let mut height = 0.0;
            let mut count = 0;
            votes.fill(0);
            for sz in 0..SUB {
                let row = (j * SUB + sz) * SAMPLES + i * SUB;
                for s in row..row + SUB {
                    let t = raster.triangle[s];
                    if t == NO_TRIANGLE {
                        continue;
                    }
                    count += 1;
                    height += raster.best_y[s];
                    let key = key_of[t as usize];
                    votes[key] += 1;
                    let [w1, w2] = raster.weights[s];
                    if let Some(rgb) = triangles[t as usize].shade(vram, w1, w2) {
                        moments.add(rgb);
                        all.add(rgb);
                        let entry = &mut key_samples[key];
                        entry.0 += 1;
                        for (sum, c) in entry.1.iter_mut().zip(rgb) {
                            *sum += f64::from(c);
                        }
                    }
                }
            }
            hits += count;
            let texel = &mut work.texels[j * CELL_TEXELS + i];
            texel.height = if count > 0 {
                height / count as f32
            } else {
                f32::from(raw.floor)
            };
            texel.colored = moments.n > 0;
            texel.rgb = moments.mean();
            texel.luma_std = moments.luma_std();
            texel.key = votes
                .iter()
                .enumerate()
                .filter(|(_, n)| **n > 0)
                .max_by_key(|(k, n)| (**n, std::cmp::Reverse(*k)))
                .map(|(k, _)| keys[k]);
        }
    }
    work.key_samples = keys
        .iter()
        .zip(key_samples)
        .map(|(k, (n, sum))| (*k, n, sum))
        .collect();
    work.mean = all.mean();
    work.luma_std = all.luma_std();
    work.coverage = hits as f32 / (SAMPLES * SAMPLES) as f32;
    work.triangles = triangles
        .iter()
        .map(|triangle| {
            let moments = triangle_average(vram, triangle);
            TriangleWork {
                positions: triangle.positions,
                rgb: moments.mean(),
                luma_std: moments.luma_std(),
                key: triangle.key,
            }
        })
        .collect();
    work
}

/// The triangle's texture over its UV area (at most about 256 samples),
/// modulated by its vertex colours.
fn triangle_average(vram: &[u16], triangle: &Triangle) -> Moments {
    let [a, b, c] = triangle.uv;
    let area = (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
    let mut moments = Moments::default();
    if area.abs() >= 1.0 {
        let step = (area.abs() * 0.5 / TRIANGLE_SAMPLES).sqrt().max(1.0);
        let inverse = 1.0 / area;
        let lo = [0, 1].map(|k| a[k].min(b[k]).min(c[k]));
        let hi = [0, 1].map(|k| a[k].max(b[k]).max(c[k]));
        let mut v = lo[1] + 0.5 * step.min(hi[1] - lo[1]);
        while v <= hi[1] {
            let mut u = lo[0] + 0.5 * step.min(hi[0] - lo[0]);
            while u <= hi[0] {
                let w1 = ((u - a[0]) * (c[1] - a[1]) - (v - a[1]) * (c[0] - a[0])) * inverse;
                let w2 = ((b[0] - a[0]) * (v - a[1]) - (b[1] - a[1]) * (u - a[0])) * inverse;
                let inside = w1 >= -1e-4 && w2 >= -1e-4 && w1 + w2 <= 1.0 + 1e-4;
                if inside && let Some(rgb) = triangle.shade(vram, w1, w2) {
                    moments.add(rgb);
                }
                u += step;
            }
            v += step;
        }
    }
    if moments.n == 0 {
        // A sliver or a single texel: the centroid.
        if let Some(rgb) = triangle.shade(vram, 1.0 / 3.0, 1.0 / 3.0) {
            moments.add(rgb);
        }
    }
    moments
}

/// Running colour mean and luma variance.
#[derive(Clone, Copy, Debug, Default)]
struct Moments {
    n: u32,
    sum: [f32; 3],
    luma: f32,
    luma2: f32,
}

impl Moments {
    fn add(&mut self, rgb: [f32; 3]) {
        self.n += 1;
        for (sum, c) in self.sum.iter_mut().zip(rgb) {
            *sum += c;
        }
        let y = luma(rgb);
        self.luma += y;
        self.luma2 += y * y;
    }
    fn mean(&self) -> [f32; 3] {
        if self.n == 0 {
            return [0.0; 3];
        }
        self.sum.map(|s| s / self.n as f32)
    }
    fn luma_std(&self) -> f32 {
        if self.n == 0 {
            return 0.0;
        }
        let n = self.n as f32;
        let mean = self.luma / n;
        (self.luma2 / n - mean * mean).max(0.0).sqrt()
    }
}

// ---- classification -------------------------------------------------------------------

/// The sea (§2.4): among the flat, fully covered, low-variance tile types
/// that are not warm (blue at least red), those at the lowest level; the
/// most common of them in the grid is the sea and its height the sea level.
fn detect_sea(raws: &[RawTile], works: &[TileWork], tiles: &[Tile]) -> Option<Sea> {
    let candidates: Vec<usize> = (1..raws.len())
        .filter(|&t| {
            let raw = &raws[t];
            let work = &works[t];
            let [r, _, b] = work.mean;
            raw.floor == raw.top
                && work.coverage > 0.99
                && work.luma_std < SEA_LUMA_STD
                && b >= r
                && tiles[t].cells > 0
        })
        .collect();
    let lowest = candidates.iter().map(|&t| raws[t].floor).max()?;
    let tile = candidates
        .iter()
        .copied()
        .filter(|&t| raws[t].floor == lowest)
        .max_by_key(|&t| (tiles[t].cells, std::cmp::Reverse(t)))?;
    let mut keys: Vec<TextureKey> = raws[tile].faces.iter().map(|f| f.key).collect();
    keys.sort();
    keys.dedup();
    Some(Sea {
        level: f32::from(lowest),
        tile: tile as u16,
        keys,
        cells: tiles[tile].cells,
    })
}

/// Every texture key with its faces, tile types, map area and mean colour,
/// largest area first.
fn key_stats(raws: &[RawTile], works: &[TileWork], tiles: &[Tile]) -> Vec<KeyStats> {
    let mut stats: BTreeMap<TextureKey, (KeyStats, f64, [f64; 3])> = BTreeMap::new();
    let sample_area = 1.0 / (SAMPLES * SAMPLES) as f64;
    for (t, (raw, work)) in raws.iter().zip(works).enumerate() {
        let cells = f64::from(tiles[t].cells);
        let mut seen = BTreeSet::new();
        for face in &raw.faces {
            let (entry, _, _) = stats.entry(face.key).or_insert_with(|| {
                let stat = KeyStats {
                    key: face.key,
                    faces: 0,
                    tiles: 0,
                    area: 0.0,
                    mean_color: [0; 3],
                    water: 0.0,
                    forest: 0.0,
                    built: 0.0,
                };
                (stat, 0.0, [0.0; 3])
            });
            entry.faces += 1;
            if seen.insert(face.key) {
                entry.tiles += 1;
            }
        }
        for (key, n, sum) in &work.key_samples {
            if let Some((stat, samples, colors)) = stats.get_mut(key) {
                let weight = f64::from(*n) * cells;
                stat.area += weight * sample_area;
                *samples += weight;
                for (s, c) in colors.iter_mut().zip(sum) {
                    *s += c * cells;
                }
            }
        }
    }
    let mut list: Vec<KeyStats> = stats
        .into_values()
        .map(|(mut stat, samples, colors)| {
            if samples > 0.0 {
                stat.mean_color = colors.map(|s| to_u8((s / samples) as f32));
            }
            stat
        })
        .collect();
    list.sort_by(|a, b| b.area.total_cmp(&a.area).then(a.key.cmp(&b.key)));
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_wrap_follows_the_game() {
        assert_eq!(wrap_cell(0), 0);
        assert_eq!(wrap_cell(255), 255);
        assert_eq!(wrap_cell(-1), 7);
        assert_eq!(wrap_cell(-8), 0);
        assert_eq!(wrap_cell(-9), 7);
        assert_eq!(wrap_cell(256), 248);
        assert_eq!(wrap_cell(263), 255);
        assert_eq!(wrap_cell(264), 248);
        // 16-bit indices, as 8005E4CC keeps them.
        assert_eq!(wrap_cell(0x1_0003), 3);
    }

    #[test]
    fn world_cells_and_origins() {
        assert_eq!(world_cell(0.0), 128);
        assert_eq!(world_cell(-0.5), 127);
        assert_eq!(world_cell(2047.9), 128);
        assert_eq!(world_cell(2048.0), 129);
        assert_eq!(cell_origin(128, 127), [1024.0, 0.0, -1024.0]);
        // The camera of the JP mission 1 state (VBlank 50000): cell (91, 85).
        assert_eq!(world_cell(-74_946.8), 91);
        assert_eq!(world_cell(-86_091.0), 85);
    }

    #[test]
    fn atlas_cells() {
        assert_eq!(Atlas::cell_origin(0), (0, 0));
        assert_eq!(Atlas::cell_origin(17), (32, 32));
        assert_eq!(Atlas::cell_origin(511), (480, 992));
        assert_eq!(Atlas::index(1, 2, 3), 3 * ATLAS_WIDTH + 32 + 2);
    }

    #[test]
    fn fog_block_cluts_read_their_source() {
        let mut vram = vec![0u16; 1024 * 512];
        vram[260 * 1024 + 320 + 5] = 0x1111;
        vram[388 * 1024 + 320 + 5] = 0x2222;
        vram[200 * 1024 + 320 + 5] = 0x3333;
        let clut = |x: u16, y: u16| (y << 6) | (x / 16);
        assert_eq!(clut_entry(&vram, clut(320, 260), 5), 0x2222);
        assert_eq!(clut_entry(&vram, clut(320, 200), 5), 0x3333);
    }

    #[test]
    fn texture_depths() {
        let mut vram = vec![0u16; 1024 * 512];
        // 4-bit page at (704, 0): texel (5, 2) is nibble 1 of word 1.
        vram[2 * 1024 + 704 + 1] = 0x00a0;
        vram[100 * 1024 + 16 * 3 + 0xa] = 0x7c1f;
        let key = TextureKey {
            tpage: 0x2b,
            clut: (100 << 6) | 3,
        };
        assert_eq!(sample_texture(&vram, key, 5, 2), Some(0x7c1f));
        // Entry 0 of that CLUT is 0x0000: transparent.
        assert_eq!(sample_texture(&vram, key, 4, 2), None);
        // 8-bit page at (128, 256): texel (3, 1) is byte 1 of word 1.
        vram[257 * 1024 + 128 + 1] = 0x4200;
        vram[50 * 1024 + 0x42] = 0x0421;
        let eight = TextureKey {
            tpage: 0x80 | 0x10 | 2,
            clut: 50 << 6,
        };
        assert_eq!(sample_texture(&vram, eight, 3, 1), Some(0x0421));
        // 15-bit page.
        vram[7 * 1024 + 64 + 9] = 0x1234;
        let direct = TextureKey {
            tpage: 0x101,
            clut: 0,
        };
        assert_eq!(sample_texture(&vram, direct, 9, 7), Some(0x1234));
    }

    #[test]
    fn modulation_is_the_gpus() {
        let white = 0x7fff;
        assert_eq!(modulate(white, [128.0; 3]).map(to_u8), [255; 3]);
        assert_eq!(modulate(white, [64.0; 3]).map(to_u8), [128; 3]);
        assert_eq!(modulate(white, [255.0; 3]).map(to_u8), [255; 3]);
        assert_eq!(modulate(0x001f, [128.0; 3]).map(to_u8), [255, 0, 0]);
    }

    /// A synthetic flat tile: one quad over the whole cell, a 4-bit texture
    /// of one colour.
    fn flat_tile(y: i16, colour: u16) -> (RawTile, Vec<u16>) {
        let mut vram = vec![0u16; 1024 * 512];
        for v in 0..64 {
            for w in 0..16 {
                vram[v * 1024 + 704 + w] = 0x1111; // index 1 everywhere
            }
        }
        vram[100 * 1024 + 48 + 1] = colour;
        let corner = |x: i16, z: i16| [x, y, z];
        let tile = RawTile {
            address: 0x8010_0000,
            floor: y,
            top: y,
            vertices: vec![
                corner(-1024, -1024),
                corner(1024, -1024),
                corner(-1024, 1024),
                corner(1024, 1024),
                corner(0, 0),
                corner(0, 0),
            ],
            faces: vec![RawFace {
                indices: [2, 3, 0, 1],
                uv: [[0, 0], [63, 0], [0, 63], [63, 63]],
                key: TextureKey {
                    tpage: 0x2b,
                    clut: (100 << 6) | 3,
                },
                colors: [[128, 128, 128]; 4],
            }],
        };
        (tile, vram)
    }

    #[test]
    fn a_flat_tile_fills_its_cell() {
        let (tile, vram) = flat_tile(-1024, 0x2108); // (8, 8, 8)
        assert_eq!(tile.triangles(&vram).len(), 2);
        let mut raster = Raster::new();
        let work = shade_tile(&tile, &vram, &mut raster);
        assert!((work.coverage - 1.0).abs() < 1e-6);
        assert!(work.texels.iter().all(|t| t.key.is_some() && t.colored));
        assert!(work.texels.iter().all(|t| (t.height + 1024.0).abs() < 1e-3));
        let grey = 8.0 * 255.0 / 31.0;
        assert!(work.texels.iter().all(|t| (t.rgb[0] - grey).abs() < 0.01));
        assert!(work.luma_std < 1e-2);
        for triangle in &work.triangles {
            assert!((triangle.rgb[1] - grey).abs() < 0.01);
        }
    }

    #[test]
    fn the_topmost_surface_wins() {
        let (mut tile, vram) = flat_tile(-1024, 0x2108);
        // A raised quad over the -x half at y -1100, darker.
        let base = tile.vertices.len() as u8;
        for [x, z] in [[-1024, -1024], [0, -1024], [-1024, 1024], [0, 1024]] {
            tile.vertices.push([x, -1100, z]);
        }
        tile.vertices.extend([[0, 0, 0], [0, 0, 0]]);
        let mut raised = tile.faces[0].clone();
        raised.indices = [base + 2, base + 3, base, base + 1];
        raised.colors = [[64, 64, 64]; 4];
        tile.faces.push(raised);
        tile.top = -1100;
        let mut raster = Raster::new();
        let work = shade_tile(&tile, &vram, &mut raster);
        let left = &work.texels[10 * CELL_TEXELS + 3];
        let right = &work.texels[10 * CELL_TEXELS + 28];
        assert!((left.height + 1100.0).abs() < 1e-3);
        assert!((right.height + 1024.0).abs() < 1e-3);
        assert!(left.rgb[0] < right.rgb[0]);
    }

    #[test]
    fn triangles_follow_the_gpu_quad_split_and_drop_repeats() {
        let (mut tile, vram) = flat_tile(-1024, 0x2108);
        // A 0x100 triangle repeats v2 as v3.
        tile.faces[0].indices = [2, 3, 0, 0];
        let triangles = tile.triangles(&vram);
        assert_eq!(triangles.len(), 1);
        assert_eq!(triangles[0].positions[2], [-1024.0, -1024.0, -1024.0]);
        // A face naming a vertex past the list is skipped.
        tile.faces[0].indices = [2, 3, 0, 9];
        assert!(tile.triangles(&vram).is_empty());
    }

    #[test]
    fn classification_rules() {
        let rules = Classifier::default();
        // JP mission 1: forest (37, 41, 25), city (76, 76, 62), pavement.
        assert_eq!(rules.color_flags([37.0, 41.0, 25.0], 8.0), flags::FOREST);
        assert_eq!(rules.color_flags([76.0, 76.0, 62.0], 8.0), flags::BUILT);
        assert_eq!(rules.color_flags([200.0, 190.0, 180.0], 5.0), flags::BUILT);
        // Dark but grey (a roof), and dark green but noisy.
        assert_eq!(rules.color_flags([40.0, 42.0, 34.0], 4.0), flags::BUILT);
        assert_eq!(rules.color_flags([37.0, 41.0, 25.0], 20.0), flags::FOREST);
        assert_eq!(rules.color_flags([45.0, 42.0, 25.0], 20.0), flags::BUILT);
        // Brown, dark, smooth: neither.
        assert_eq!(rules.color_flags([45.0, 40.0, 25.0], 4.0), 0);
        let sea = Sea {
            level: -1024.0,
            tile: 1,
            keys: vec![TextureKey { tpage: 1, clut: 2 }],
            cells: 1,
        };
        let sea_key = sea.keys[0];
        let other = TextureKey { tpage: 3, clut: 4 };
        let dark = [29.0, 41.0, 35.0];
        assert_eq!(
            rules.flags(sea_key, Some(&sea), true, dark, 4.0),
            flags::WATER
        );
        // The sea's texture above the sea level is not water.
        assert_ne!(
            rules.flags(sea_key, Some(&sea), false, dark, 4.0),
            flags::WATER
        );
        // A key table replaces the colour rule, and can add water keys.
        let table = Classifier {
            keys: vec![(other, flags::BUILT), (sea_key, flags::WATER)],
            ..Classifier::default()
        };
        assert_eq!(
            table.flags(other, None, true, [37.0, 41.0, 25.0], 8.0),
            flags::BUILT
        );
        assert_eq!(table.flags(sea_key, None, true, dark, 4.0), flags::WATER);
    }
}
