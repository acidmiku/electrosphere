//! The atmosphere in the game (Milestone 1 Phase 4 item 5): in Japanese
//! flight with the GPU renderer above 1x, `[effects] atmosphere` removes the
//! game's palette fog and draws a height haze in its place. The rules and
//! the model are fx-fog's ([`super::atmosphere`]); this module wires them
//! into the effects infrastructure (`docs/EFFECTS.md` §7).
//! `docs/EFFECTS_ATMOSPHERE.md` §8 describes it, with the pictures.
//!
//! Display only, both halves:
//! - **The recorder half** ([`AtmosphereRecord`], with [`redirect`] as the
//!   recorder's [`Recorder::clut_redirect`]): world-phase reads of the live
//!   terrain palettes read the unfogged copy instead. Each effects frame's
//!   kick reads, from machine RAM, what the rule alone would get wrong
//!   ([`Palettes`]: the depth masks of world flag 4, the mission's animated
//!   palette entries) and the haze's sea level ([`ground_level`]).
//! - **The card half** ([`AtmospherePost`]): the haze post pass at world end,
//!   before the deferred transparency (smoke, explosions, trails), the flare,
//!   the HUD and the overlays. Each world pixel's view z (the depth
//!   snapshot; [`coarse_depth`] from its ordering-table slot where it has
//!   none) gives its world position, `haze_shade` (`haze.wgsl`) its colour,
//!   and `fx_encode` writes it back dithered (`atmosphere_post.wgsl`).
//!
//! Off (`atmosphere = false`, the default) both halves are registered and do
//! nothing: no redirect, no pass. Effects off (`[effects] enabled`, F6), at
//! 1x, outside flight and in movies there is no effects frame, so neither
//! half runs.
use super::atmosphere::{self, Haze, HazeScene, SkyPolygon, SkyTable, View};
use crate::gpu_renderer::execute::{self, VRAM_FORMAT};
use crate::gpu_renderer::fx::{self, CardEffect, Class, Context, FxFrame, RecordEffect, Scene};
use crate::gpu_renderer::{Executor, Recorder};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::cell::Cell;
use std::sync::Arc;

/// The effect's name (both halves).
pub const NAME: &str = "atmosphere";

// ---- settings ------------------------------------------------------------------------

/// `[effects] haze_density`: the haze's density as a multiple of fx-fog's
/// (§2.3 of the recompilation report: 1/110,000 per unit at sea level). 1 by
/// default; 3 is about the game's own depth at its short draw distance; 0
/// draws no haze (the game's fog removed alone).
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HazeDensity(pub f64);

impl HazeDensity {
    /// The largest value the settings accept.
    pub const MAX: f64 = 10.0;

    /// Whether loading accepts it (0 to [`Self::MAX`]).
    pub fn valid(self) -> bool {
        (0.0..=Self::MAX).contains(&self.0)
    }

    /// The factor as the haze uses it: within 0 to [`Self::MAX`] (1 for
    /// NaN).
    pub fn factor(self) -> f32 {
        if self.0.is_nan() {
            1.0
        } else {
            self.0.clamp(0.0, Self::MAX) as f32
        }
    }
}

impl Default for HazeDensity {
    fn default() -> Self {
        Self(1.0)
    }
}

/// What the atmosphere does, from the settings ([`Config::from_settings`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    /// `[effects] atmosphere` (it also needs `enabled`, which F6 flips).
    pub on: bool,
    /// The haze density factor ([`HazeDensity::factor`]); 0: no post pass.
    pub density: f32,
    /// Fade terrain into the sky across the game's draw distance (the
    /// terrain ends there); off when far terrain is drawn beyond it.
    pub edge: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            on: false,
            density: 1.0,
            edge: true,
        }
    }
}

impl Config {
    pub fn from_settings(effects: &crate::settings::Effects) -> Self {
        Self {
            on: effects.atmosphere,
            density: effects.haze_density.factor(),
            edge: !effects.far_terrain,
        }
    }
}

/// Register the recorder half on `recorder` (after
/// [`Recorder::set_effects`]: nothing when effects are not configured, so
/// at 1x and outside the Japanese release). On, the palette redirect is set.
pub fn register_recorder(recorder: &mut Recorder, config: Config) {
    if recorder.effects_config().is_none() {
        return;
    }
    recorder.clut_redirect = config.on.then_some(redirect as fx::ClutRedirect);
    recorder.add_effect(Box::new(AtmosphereRecord::new(config)));
    if config.on {
        eprintln!(
            "ac3: effects: atmosphere on: the game's fog removed, haze density {}x{}",
            config.density,
            if config.edge {
                ", terrain fading into the sky at the draw distance"
            } else {
                ""
            }
        );
    }
}

/// Register the card half on `executor` (above 1x).
pub fn register_card(executor: &mut Executor, config: Config) {
    if executor.scale() > 1 {
        executor.add_effect(Box::new(AtmospherePost::new(config)));
    }
}

// ---- the palette redirect -----------------------------------------------------------

/// Rows of the live block the depth masks replace (8005EFF0: a 16 x 24 copy
/// from (320, 280) to (320, 256), template 80024580, in every bucket).
pub const MASK_ROWS: u16 = 24;
/// World flags (world `+0`): depth masks; no terrain.
pub const WORLD_DEPTH_MASKS: u32 = 4;
pub const WORLD_NO_TERRAIN: u32 = 0x100;
/// Mission section 0: the animated palette entries' count and records
/// (12 bytes each, the halfword +0 their position in the block: x =
/// `& 0xF`, row = `>> 4`; 8005EDC8).
pub const ANIMATED_COUNT: u32 = 0x74;
pub const ANIMATED_RECORDS: u32 = 0x78;
const ANIMATED_STRIDE: u32 = 12;
/// More records than this: not the game's (keep every row).
const ANIMATED_LIMIT: u32 = 4096;

/// What the palette redirect needs to know about the live block in an
/// effects frame besides fx-fog's rule ([`atmosphere::clut_redirect`]).
///
/// In each bucket the game copies the bucket's palette step into the block,
/// then writes the animated entries (depth-cued, 8005EDC8), then, with world
/// flag 4, copies rows 24..47 over rows 0..23 (8005EFF0). What the nearest
/// terrain reads (bucket 0) is the unfogged palette, so:
/// - **depth masks** read rows 0..23 from the unfogged block's rows 24..47
///   (exact: the copy is the same in every bucket);
/// - **animated entries** have only their base colour in the unfogged block,
///   so the rows that read one ([`Self::keep`]) keep the game's palette
///   (animated, and fogged by the game far away).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Palettes {
    pub masks: bool,
    /// Bit `r`: reads of block row `r` (y = 256 + r) are not redirected.
    pub keep: u64,
}

impl Palettes {
    /// Nothing to keep: the rule alone (fx-fog's, mission 1).
    pub const PLAIN: Self = Self {
        masks: false,
        keep: 0,
    };
    /// Keep the game's palettes everywhere.
    pub const NONE: Self = Self {
        masks: false,
        keep: u64::MAX,
    };

    /// The live block's rows with an animated entry (`animated`, bit per
    /// row as the game writes them) as reads see them: with the masks, rows
    /// 0..23 read rows 24..47.
    pub fn new(masks: bool, animated: u64) -> Self {
        let low = (1u64 << MASK_ROWS) - 1;
        let keep = if masks {
            ((animated >> MASK_ROWS) & low) | (animated & !low)
        } else {
            animated
        };
        Self { masks, keep }
    }

    /// From machine RAM at an effects frame's kick (`scene`): world flag 4
    /// and the mission's animated entries. A record outside RAM keeps every
    /// row.
    pub fn read(ram: &[u8], scene: &Scene) -> Self {
        let word = |address| fx::scene::word(ram, address);
        let Some(flags) = word(scene.world) else {
            return Self::NONE;
        };
        let masks = flags & WORLD_DEPTH_MASKS != 0;
        let Some(count) = word(scene.mission + ANIMATED_COUNT) else {
            return Self::NONE;
        };
        if count > ANIMATED_LIMIT {
            return Self::NONE;
        }
        let mut animated = 0u64;
        for record in 0..count {
            let at = scene.mission + ANIMATED_RECORDS + ANIMATED_STRIDE * record;
            let Some(location) = fx::scene::half(ram, at) else {
                return Self::NONE;
            };
            let row = location >> 4;
            if (0..64).contains(&row) {
                animated |= 1u64 << row as u32;
            }
        }
        Self::new(masks, animated)
    }

    /// The redirect of a palette read (see [`redirect`]).
    pub fn redirect(&self, word: u16, entries: u16, phase: atmosphere::Phase) -> Option<u16> {
        let clut = atmosphere::clut_position(word);
        let [x, y] = atmosphere::clut_redirect(clut, entries, phase)?;
        let row = y - atmosphere::UNFOGGED_PALETTES[1];
        if (self.keep >> row) & 1 != 0 {
            return None;
        }
        let y = if self.masks && row < MASK_ROWS {
            y + MASK_ROWS
        } else {
            y
        };
        Some(atmosphere::clut_word([x, y]))
    }
}

thread_local! {
    /// The current effects frame's [`Palettes`], set by the recorder half's
    /// kick on the thread that records (the recorder calls the redirect
    /// while it records the list it kicked, on that thread).
    static PALETTES: Cell<Palettes> = const { Cell::new(Palettes::PLAIN) };
}

/// **The palette redirect** (`Recorder::clut_redirect`): fx-fog's rule
/// ([`atmosphere::clut_word_redirect`]: world-phase 4-bit reads of the live
/// terrain block read the unfogged copy 128 rows lower), with this frame's
/// depth masks and animated rows ([`Palettes`]).
pub fn redirect(word: u16, entries: u16, phase: atmosphere::Phase) -> Option<u16> {
    PALETTES.with(Cell::get).redirect(word, entries, phase)
}

/// The palettes the redirect uses on this thread (tests, diagnostics).
pub fn current_palettes() -> Palettes {
    PALETTES.with(Cell::get)
}

// ---- the sea level -------------------------------------------------------------------

/// The haze's sea level before a mission's is known: mission 1's sea
/// (fx-map's terrain map; world y, y down).
pub const DEFAULT_SEA_LEVEL: f32 = -1024.0;
/// Grid cells (of 65,536) a flat level needs to count as the ground.
const GROUND_CELLS: u32 = 4096;

/// **The haze's sea level** from the mission's terrain in RAM: the world y
/// of the lowest flat tile level (tile meshes whose floor and top heights,
/// mesh `+0x14`/`+0x16`, are equal) that covers at least 1/16 of the 256 x
/// 256 grid, else the most common flat level. Mission 1: the sea, -1024 (as
/// fx-map's terrain map finds it). `None` without a readable grid.
pub fn ground_level(ram: &[u8], grid: u32, tiles: u32) -> Option<f32> {
    let word = |address| fx::scene::word(ram, address);
    let count = word(tiles)?;
    if count == 0 || count >= 512 {
        return None;
    }
    let mut flat = vec![None; count as usize + 1];
    for (tile, level) in flat.iter_mut().enumerate().skip(1) {
        let mesh = word(tiles + 4 * tile as u32)?;
        let floor = fx::scene::half(ram, mesh + 0x14)?;
        let top = fx::scene::half(ram, mesh + 0x16)?;
        *level = (floor == top).then_some(floor);
    }
    let mut cells = std::collections::BTreeMap::<i16, u32>::new();
    for cell in 0..256 * 256u32 {
        let tile = fx::scene::half(ram, grid + 2 * cell)? as u16 & 0x1ff;
        if let Some(Some(level)) = flat.get(usize::from(tile)) {
            *cells.entry(*level).or_default() += 1;
        }
    }
    cells
        .iter()
        .rev()
        .find(|&(_, &n)| n >= GROUND_CELLS)
        .or_else(|| cells.iter().max_by_key(|&(_, &n)| n))
        .map(|(&level, _)| f32::from(level))
}

// ---- the recorder half ---------------------------------------------------------------

/// What the recorder half hands to the card half each effects frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameData {
    /// World y of the haze's sea level.
    pub sea_level: f32,
    /// Whether the haze applies (no terrain, world flag 0x100: none).
    pub haze: bool,
    /// The palettes the redirect used.
    pub palettes: Palettes,
    /// Semi-transparent world primitives with a terrain palette: redirected,
    /// but deferred past the haze (none in mission 1).
    pub semi_terrain: u32,
}

impl Default for FrameData {
    fn default() -> Self {
        Self {
            sea_level: DEFAULT_SEA_LEVEL,
            haze: true,
            palettes: Palettes::PLAIN,
            semi_terrain: 0,
        }
    }
}

/// A mission's terrain, as the sea level's cache knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerrainKey {
    mission: u32,
    grid: u32,
    tiles: u32,
    /// FNV-1a of the tile directory (its count and mesh pointers).
    directory: u64,
}

impl TerrainKey {
    fn read(ram: &[u8], scene: &Scene) -> Option<Self> {
        let count = fx::scene::word(ram, scene.tiles)?.min(512);
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for i in 0..=count {
            let value = fx::scene::word(ram, scene.tiles + 4 * i)?;
            for byte in value.to_le_bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
            }
        }
        Some(Self {
            mission: scene.mission,
            grid: scene.grid,
            tiles: scene.tiles,
            directory: hash,
        })
    }
}

/// The recorder half ([`RecordEffect`]); see the module documentation.
pub struct AtmosphereRecord {
    config: Config,
    /// The sea level of the mission last seen.
    ground: Option<(TerrainKey, f32)>,
    frame: Option<FrameData>,
}

impl AtmosphereRecord {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            ground: None,
            frame: None,
        }
    }

    fn sea_level(&mut self, ram: &[u8], scene: &Scene) -> f32 {
        let Some(key) = TerrainKey::read(ram, scene) else {
            return DEFAULT_SEA_LEVEL;
        };
        match self.ground {
            Some((known, level)) if known == key => level,
            _ => {
                let level = ground_level(ram, scene.grid, scene.tiles).unwrap_or(DEFAULT_SEA_LEVEL);
                eprintln!("ac3: effects: atmosphere: sea level y = {level}");
                self.ground = Some((key, level));
                level
            }
        }
    }
}

impl RecordEffect for AtmosphereRecord {
    fn name(&self) -> &'static str {
        NAME
    }

    fn kick(&mut self, scene: &Scene, ram: &[u8], _vram: &ac3_graphics::Vram) {
        self.frame = None;
        if !self.config.on {
            return;
        }
        let palettes = Palettes::read(ram, scene);
        PALETTES.with(|cell| cell.set(palettes));
        let flags = fx::scene::word(ram, scene.world).unwrap_or(0);
        self.frame = Some(FrameData {
            sea_level: self.sea_level(ram, scene),
            haze: flags & WORLD_NO_TERRAIN == 0,
            palettes,
            semi_terrain: 0,
        });
    }

    fn primitive(&mut self, primitive: &fx::Primitive<'_>) -> fx::Route {
        // Semi-transparent terrain is deferred past the haze (counted).
        if let Some(frame) = self.frame.as_mut()
            && primitive.phase == fx::Phase::World
            && primitive.semi
            && let Some(texture) = primitive.texture
            && texture.depth == 0
            && texture.clut[0] >= 0
            && texture.clut[1] >= 0
            && atmosphere::FOG_PALETTES.contains(texture.clut[0] as u16, texture.clut[1] as u16)
        {
            frame.semi_terrain += 1;
        }
        fx::Route::Draw
    }

    fn take(&mut self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.frame
            .take()
            .map(|frame| Arc::new(frame) as Arc<dyn Any + Send + Sync>)
    }

    fn idle(&mut self) {
        // Out of flight, effects off, a movie. The sea level stays cached
        // by its terrain's key.
        self.frame = None;
        PALETTES.with(|cell| cell.set(Palettes::PLAIN));
    }
}

// ---- the haze's inputs ---------------------------------------------------------------

/// The 8-bit colour that a 5-bit PlayStation step `c` stands for is `8 c`
/// (the GTE's INTPL and the GPU's 24-to-15-bit conversion), so 8-bit colours
/// go to the post pass's `c / 31` scale (`fx_decode`) as `v / 248`.
pub const PSX_8BIT_SCALE: f32 = 248.0;
/// The sky model's sampling step (1x screen pixels).
pub const SKY_STEP: f32 = 4.0;
/// The draw-distance fade starts at this fraction of the camera's far
/// distance (camera `+0x2A`; fx-fog's §4).
pub const EDGE_START: f32 = 0.8;
/// The coarse depth: view z per ordering-table slot (terrain hangs off
/// `(z >> 3) + 1` of its farthest vertex, 80061768's face test) and the
/// offset that puts it in the middle of the slot's range.
pub const SLOT_UNITS: f32 = 8.0;
pub const SLOT_OFFSET: f32 = -4.0;

/// An 8-bit colour on the post pass's scale (see [`PSX_8BIT_SCALE`]).
pub fn psx_unit(color: [u8; 3]) -> [f32; 3] {
    color.map(|c| (f32::from(c) / PSX_8BIT_SCALE).min(1.0))
}

/// The view of a scene (world `+0x4C`, camera H and position).
pub fn scene_view(scene: &Scene) -> View {
    let v = scene.view;
    View::from_gte(
        [
            v[0][0], v[0][1], v[0][2], v[1][0], v[1][1], v[1][2], v[2][0], v[2][1], v[2][2],
        ],
        scene.h as i16,
        scene.eye,
    )
}

/// **The sky model of an effects frame:** fx-fog's [`atmosphere::bin_sky`]
/// of the sky slot's polygons (vertex x unsqueezed by widescreen's k), on
/// the post pass's colour scale. Without a sky polygon (one band fills the
/// screen): the largest opaque tile's colour, else the fog colour.
pub fn sky_table(frame: &FxFrame) -> SkyTable {
    let scene = &frame.scene;
    let k = scene.k().max(1e-3);
    let polygons: Vec<SkyPolygon> = frame
        .sky
        .iter()
        .filter_map(|command| atmosphere::sky_polygon(&command.words))
        .map(|mut polygon| {
            for vertex in &mut polygon.vertices {
                vertex.x /= k;
            }
            polygon
        })
        .collect();
    let rescale = 255.0 / PSX_8BIT_SCALE;
    match atmosphere::bin_sky(&polygons, &scene_view(scene), SKY_STEP) {
        Some(mut table) => {
            for color in &mut table.colors {
                *color = color.map(|c| (c * rescale).min(1.0));
            }
            table
        }
        None => {
            let tile = frame
                .sky
                .iter()
                .filter(|command| command.words.len() >= 3)
                .filter(|command| command.words[0] >> 24 == 0x60)
                .max_by_key(|command| {
                    let size = command.words[2];
                    (size & 0x3ff) * ((size >> 16) & 0x1ff)
                })
                .map(|command| atmosphere::fog_color(command.words[0]));
            SkyTable::uniform(psx_unit(tile.unwrap_or(scene.fog)))
        }
    }
}

/// **The haze of an effects frame**: fx-fog's [`Haze`] with the density
/// times `config.density`, the frame's sea level, and the draw-distance
/// fade (`config.edge`) from [`EDGE_START`] of the camera's far distance.
pub fn frame_haze(config: &Config, scene: &Scene, data: &FrameData) -> Haze {
    let base = Haze::default();
    let far = scene.far as f32;
    Haze {
        density: base.density * config.density,
        sea_level: data.sea_level,
        edge: (config.edge && far > 0.0).then_some([EDGE_START * far, far]),
        ..base
    }
}

/// The haze's scene inputs: the sky model, the fog colour, the sun.
pub fn haze_inputs<'a>(scene: &Scene, sky: &'a SkyTable) -> HazeScene<'a> {
    let sun = scene.sun.map(|v| f32::from(v) / 4096.0);
    let length = (sun[0] * sun[0] + sun[1] * sun[1] + sun[2] * sun[2]).sqrt();
    HazeScene {
        sky,
        fog: psx_unit(scene.fog),
        sun: if length > 0.0 {
            sun.map(|c| c / length)
        } else {
            [0.0, -1.0, 0.0]
        },
        sun_color: scene.sun_colour.map(|v| (f32::from(v) / 4096.0).max(0.0)),
    }
}

/// The view-space ray at unit depth through the N x VRAM pixel centre
/// (`px`, `py`) (`fs_atmo`): the screen offset from the projection centre,
/// x unsqueezed.
pub fn pixel_ray(scene: &Scene, scale: u32, px: u32, py: u32) -> [f32; 3] {
    let n = scale as f32;
    let sx = (px as f32 + 0.5) / n - scene.centre[0] as f32;
    let sy = (py as f32 + 0.5) / n - scene.centre[1] as f32;
    let h = scene.h as f32;
    [sx / (scene.k() * h), sy / h, 1.0]
}

/// **The coarse depth** of a pixel without a view z (`fs_atmo`'s
/// `atmo_coarse_depth`): the primitive's ordering-table slot `slot` stands
/// for its farthest vertex's z (`slot * 8 - 4`); a ray going down
/// (`ray_y` > 0, its world y per unit of view z) meets the ground no farther
/// than the sea plane `sea_level` below the eye at `eye_y` (`None`: no
/// bound). `None` outside the world's slots (2..=4094).
pub fn coarse_depth(slot: u16, ray_y: f32, eye_y: f32, sea_level: Option<f32>) -> Option<f32> {
    if !(2..=4094).contains(&slot) {
        return None;
    }
    let mut z = f32::from(slot) * SLOT_UNITS + SLOT_OFFSET;
    if let Some(sea) = sea_level
        && ray_y > 1e-6
    {
        let below = sea - eye_y;
        if below > 0.0 {
            z = z.min(below / ray_y);
        }
    }
    Some(z)
}

/// The order table's entry of a draw order: its slot and class
/// (`slot | class << 16`).
pub fn order_entry(frame: &FxFrame, order: u32) -> u32 {
    let index = order as usize;
    if index == 0 || index > frame.slots.len() {
        return 0;
    }
    let class = frame.classes.get(index - 1).copied().unwrap_or(0);
    u32::from(frame.slots[index - 1]) | (u32::from(class) << 16)
}

// ---- the card half -------------------------------------------------------------------

/// The post pass's shader: `fx_common.wgsl`, `haze.wgsl`, then
/// `atmosphere_post.wgsl`.
pub const POST_WGSL: &str = include_str!("atmosphere_post.wgsl");

pub fn post_source() -> String {
    format!(
        "{}\n{}\n{}",
        fx::card::FX_COMMON_WGSL,
        atmosphere::HAZE_WGSL,
        POST_WGSL
    )
}

/// Draw orders per row of the order table (`ATMO_TABLE_WIDTH`).
const TABLE_WIDTH: u32 = 1024;
/// Bytes of `AtmoPost`.
const POST_UNIFORM_BYTES: u64 = 32;

/// What the card half has done.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PostStats {
    /// Post passes drawn, and effects frames left alone (off, density 0, no
    /// terrain, no snapshot).
    pub passes: u64,
    pub skipped: u64,
    /// Frames whose sky model had no sky polygon.
    pub flat_skies: u64,
}

struct Gpu {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    haze: wgpu::Buffer,
    post: wgpu::Buffer,
    /// The order table (R32Uint, [`TABLE_WIDTH`] wide) and its rows.
    table: wgpu::Texture,
    table_view: wgpu::TextureView,
}

fn float_texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn table_texture(device: &wgpu::Device, rows: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = execute::texture(
        device,
        "ac3 atmosphere orders",
        (TABLE_WIDTH, rows.max(1)),
        wgpu::TextureFormat::R32Uint,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
    );
    let view = texture.create_view(&Default::default());
    (texture, view)
}

impl Gpu {
    fn new(device: &wgpu::Device) -> Self {
        use wgpu::ShaderStages as Stages;
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ac3 atmosphere"),
            entries: &[
                execute::uniform_entry(0, Stages::FRAGMENT),
                execute::uniform_entry(1, Stages::FRAGMENT),
                execute::uniform_entry(2, Stages::FRAGMENT),
                execute::uint_texture_entry(3),
                float_texture_entry(4),
                execute::uint_texture_entry(5),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 atmosphere"),
            source: wgpu::ShaderSource::Wgsl(post_source().into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ac3 atmosphere"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ac3 atmosphere"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_atmo"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_atmo"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: VRAM_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let haze = execute::buffer(
            device,
            "ac3 atmosphere haze",
            (atmosphere::HAZE_UNIFORM_ROWS * 16) as u64,
            wgpu::BufferUsages::UNIFORM,
        );
        let post = execute::buffer(
            device,
            "ac3 atmosphere post",
            POST_UNIFORM_BYTES,
            wgpu::BufferUsages::UNIFORM,
        );
        let (table, table_view) = table_texture(device, 1);
        Self {
            pipeline,
            layout,
            haze,
            post,
            table,
            table_view,
        }
    }

    /// Upload the frame's order table (growing the texture as needed).
    fn upload_table(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &FxFrame) -> u32 {
        let orders = frame.slots.len().min(1 << 24) as u32;
        let rows = orders.div_ceil(TABLE_WIDTH).max(1);
        if self.table.height() < rows {
            (self.table, self.table_view) = table_texture(device, rows.next_power_of_two());
        }
        if orders == 0 {
            return 0;
        }
        let mut bytes = Vec::with_capacity((rows * TABLE_WIDTH * 4) as usize);
        for order in 1..=rows * TABLE_WIDTH {
            let entry = if order <= orders {
                order_entry(frame, order)
            } else {
                0
            };
            bytes.extend_from_slice(&entry.to_le_bytes());
        }
        queue.write_texture(
            self.table.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(TABLE_WIDTH * 4),
                rows_per_image: Some(rows),
            },
            wgpu::Extent3d {
                width: TABLE_WIDTH,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
        orders
    }
}

/// The card half ([`CardEffect`]); see the module documentation.
pub struct AtmospherePost {
    config: Config,
    gpu: Option<Gpu>,
    pub stats: PostStats,
}

impl AtmospherePost {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            gpu: None,
            stats: PostStats::default(),
        }
    }

    /// The haze pass with `haze` over `ctx`'s world area (`post` without
    /// its gates; tests run it at density 0 too).
    pub fn draw(&mut self, ctx: &mut Context<'_>, haze: &Haze, data: &FrameData) {
        let Some(snapshot) = ctx.snapshot else {
            self.stats.skipped += 1;
            return;
        };
        let frame = ctx.frame;
        let scene = &frame.scene;
        let gpu = self.gpu.get_or_insert_with(|| Gpu::new(ctx.device));
        // The haze's uniform.
        let sky = sky_table(frame);
        if !frame
            .sky
            .iter()
            .any(|c| atmosphere::sky_polygon(&c.words).is_some())
        {
            self.stats.flat_skies += 1;
        }
        let rows = haze.uniform(&haze_inputs(scene, &sky), scene.eye_units()[1]);
        let bytes: Vec<u8> = rows
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        ctx.queue.write_buffer(&gpu.haze, 0, &bytes);
        // The order table and the pass's own uniform.
        let orders = gpu.upload_table(ctx.device, ctx.queue, frame);
        let [x, y, _, _] = ctx.pixels;
        let mut post = Vec::with_capacity(POST_UNIFORM_BYTES as usize);
        for word in [x, y, ctx.scale, orders] {
            post.extend_from_slice(&word.to_le_bytes());
        }
        for value in [SLOT_UNITS, SLOT_OFFSET, data.sea_level, 1.0] {
            post.extend_from_slice(&value.to_le_bytes());
        }
        ctx.queue.write_buffer(&gpu.post, 0, &post);
        let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 atmosphere"),
            layout: &gpu.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ctx.scene_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: gpu.haze.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: gpu.post.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(ctx.shadow),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(snapshot),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&gpu.table_view),
                },
            ],
        });
        {
            let mut pass = ctx.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ac3 atmosphere"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: ctx.vram,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let [x, y, w, h] = ctx.pixels;
            pass.set_pipeline(&gpu.pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.set_scissor_rect(x, y, w, h);
            pass.draw(0..3, 0..1);
        }
        // The next post effect (the sun and bloom) reads the hazed picture.
        ctx.sync_area();
        self.stats.passes += 1;
    }
}

impl CardEffect for AtmospherePost {
    fn name(&self) -> &'static str {
        NAME
    }

    fn post(&mut self, ctx: &mut Context<'_>) {
        let data = ctx
            .data
            .and_then(|data| data.downcast_ref::<FrameData>())
            .copied()
            .unwrap_or_default();
        let [_, _, w, h] = ctx.pixels;
        if !self.config.on || self.config.density <= 0.0 || !data.haze || w == 0 || h == 0 {
            self.stats.skipped += 1;
            return;
        }
        let haze = frame_haze(&self.config, &ctx.frame.scene, &data);
        self.draw(ctx, &haze, &data);
    }
}

/// The class of terrain in the order table (`fx::Class::Terrain`).
pub const TERRAIN_CLASS: u8 = Class::Terrain as u8;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::atmosphere::Phase;
    use crate::gpu_renderer::Options;
    use crate::gpu_renderer::record::{VERTEX_WORDS, mode};
    use crate::gpu_renderer::tiles::Rect;
    use ac3_graphics::{Gpu as SoftGpu, Vram};

    // ---- settings and the redirect ----

    #[test]
    fn haze_density_reads_and_clamps() {
        assert_eq!(HazeDensity::default().factor(), 1.0);
        assert_eq!(HazeDensity(3.0).factor(), 3.0);
        assert_eq!(HazeDensity(-1.0).factor(), 0.0);
        assert_eq!(HazeDensity(50.0).factor(), 10.0);
        assert_eq!(HazeDensity(f64::NAN).factor(), 1.0);
        assert!(HazeDensity(0.0).valid() && HazeDensity(10.0).valid());
        assert!(!HazeDensity(-0.1).valid() && !HazeDensity(f64::NAN).valid());
        let config = Config::from_settings(&crate::settings::Effects::default());
        assert_eq!(config, Config::default(), "off by default, density 1");
    }

    /// The CLUT half-word of palette (x, y).
    fn clut(x: u16, y: u16) -> u16 {
        atmosphere::clut_word([x, y])
    }

    #[test]
    fn the_rule_alone_without_masks_or_animation() {
        let plain = Palettes::PLAIN;
        for (y, phase, out) in [
            (256, Phase::World, Some(384)),
            (319, Phase::World, Some(447)),
            (256, Phase::Sky, None),
            (256, Phase::Flare, None),
            (256, Phase::Hud, None),
            (256, Phase::Other, None),
            (320, Phase::World, None),
        ] {
            let got = plain.redirect(clut(320, y), 16, phase);
            assert_eq!(got, out.map(|y| clut(320, y)), "{y} {phase:?}");
            assert_eq!(
                got,
                atmosphere::clut_word_redirect(clut(320, y), 16, phase),
                "fx-fog's rule"
            );
        }
        // 8-bit palettes never.
        assert_eq!(plain.redirect(clut(320, 256), 256, Phase::World), None);
    }

    #[test]
    fn depth_masks_and_animated_rows() {
        // Masks: rows 0..23 read the unfogged rows 24..47.
        let masks = Palettes::new(true, 0);
        assert_eq!(
            masks.redirect(clut(320, 256), 16, Phase::World),
            Some(clut(320, 384 + 24))
        );
        assert_eq!(
            masks.redirect(clut(320, 256 + 23), 16, Phase::World),
            Some(clut(320, 384 + 47))
        );
        assert_eq!(
            masks.redirect(clut(320, 256 + 24), 16, Phase::World),
            Some(clut(320, 384 + 24))
        );
        // An animated entry in row 5 keeps row 5's reads as the game has them.
        let animated = Palettes::new(false, 1 << 5);
        assert_eq!(animated.redirect(clut(320, 261), 16, Phase::World), None);
        assert_eq!(
            animated.redirect(clut(320, 262), 16, Phase::World),
            Some(clut(320, 390))
        );
        // With the masks, row 30's animation shows in row 6 (and row 6's own
        // is overwritten).
        let both = Palettes::new(true, (1 << 30) | (1 << 6));
        assert_eq!(both.keep, (1 << 30) | (1 << 6));
        let both = Palettes::new(true, 1 << 30);
        assert_eq!(both.keep, (1 << 30) | (1 << 6));
        assert_eq!(both.redirect(clut(320, 262), 16, Phase::World), None);
        let both = Palettes::new(true, 1 << 6);
        assert_eq!(both.keep, 0, "row 6's own entry is overwritten by row 30");
        assert_eq!(
            Palettes::NONE.redirect(clut(320, 256), 16, Phase::World),
            None
        );
    }

    // ---- the recorder half in a recorded frame ----

    fn put(ram: &mut [u8], address: u32, value: u32) {
        let at = (address & 0x1f_ffff) as usize;
        ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// A 4-bit sprite of page 5 with the palette at (320, `y`).
    fn sprite(y: u16, x: u32) -> Vec<u32> {
        vec![
            0xe100_0005,
            0x7c80_8080,
            x | (0x40 << 16),
            u32::from(clut(320, y)) << 16,
        ]
    }

    const FULL_AREA: [u32; 2] = [0xe300_0000, 0xe407_ffff];

    /// A flight frame with a terrain-palette sprite in the sky, three in
    /// the world, one in the flare's slot and one in the HUD's.
    fn frame_packets() -> Vec<(u32, Vec<u32>)> {
        let mut sky = FULL_AREA.to_vec();
        sky.extend(sprite(256, 0));
        vec![
            (4095, sky),
            (4094, sprite(256, 8)),
            (3000, sprite(256 + 5, 16)),
            (2000, sprite(256 + 40, 24)),
            (1, sprite(256, 32)),
            (0, sprite(256, 40)),
        ]
    }

    fn record(
        ram: &[u8],
        start: u32,
        scale: u32,
        register: Option<Config>,
        enabled: bool,
    ) -> (crate::gpu_renderer::Frame, Recorder) {
        let mut gpu = SoftGpu::new(Vram::new());
        let mut recorder = Recorder::new(
            Options {
                scale,
                dither: false,
            },
            &gpu,
        );
        recorder.take_frame(gpu.vram());
        recorder.set_effects(Some(fx::Config {
            enabled,
            ..fx::Config::default()
        }));
        if let Some(config) = register {
            register_recorder(&mut recorder, config);
        }
        recorder.list_begin(ram, start, gpu.vram(), false);
        for (address, words) in crate::flight_host::walk_list_addressed(ram, start).unwrap() {
            for (i, &word) in words.iter().enumerate() {
                recorder.gp0(word, address + 4 * (i as u32 + 1), gpu.vram());
                gpu.write_gp0(word).unwrap();
            }
        }
        recorder.list_end(gpu.vram());
        (recorder.take_frame(gpu.vram()), recorder)
    }

    /// The palette rows the frame's textured vertices decode with, one per
    /// sprite (six vertices each).
    fn palette_rows(frame: &crate::gpu_renderer::Frame) -> Vec<u32> {
        frame
            .vertices
            .chunks_exact(VERTEX_WORDS)
            .filter(|v| v[5] & mode::TEXTURED != 0)
            .map(|v| (v[6] >> 13) & 0x1ff)
            .collect::<Vec<_>>()
            .chunks(6)
            .map(|rows| rows[0])
            .collect()
    }

    fn on() -> Config {
        Config {
            on: true,
            ..Config::default()
        }
    }

    #[test]
    fn the_redirect_applies_only_in_the_world_phase() {
        let mut ram = fx::scene::tests::flight_ram();
        let start = fx::walk::tests::flight_list_in(&mut ram, &frame_packets());
        // Sky, world (4094, 3000, 2000), flare, HUD.
        let (frame, recorder) = record(&ram, start, 2, Some(on()), true);
        assert_eq!(
            palette_rows(&frame),
            [256, 384, 389, 424, 256, 256],
            "only the world's three"
        );
        assert_eq!(recorder.effects_stats().unwrap().redirected, 3);
        assert!(recorder.clut_redirect.is_some());
        let fx = frame.fx.as_deref().unwrap();
        let data = fx
            .data
            .iter()
            .find(|(name, _)| *name == NAME)
            .and_then(|(_, data)| data.downcast_ref::<FrameData>())
            .copied()
            .expect("the card half's data");
        assert_eq!(data.palettes, Palettes::PLAIN);
        assert!(data.haze);
        // Off: registered, nothing redirected; effects off (F6): nothing
        // either, though the redirect is set; at 1x nothing is registered.
        let as_is = [256, 256, 261, 296, 256, 256];
        let (frame, recorder) = record(&ram, start, 2, Some(Config::default()), true);
        assert_eq!(palette_rows(&frame), as_is);
        assert!(recorder.clut_redirect.is_none());
        let (frame, recorder) = record(&ram, start, 2, Some(on()), false);
        assert_eq!(palette_rows(&frame), as_is);
        assert!(recorder.clut_redirect.is_some());
        assert_eq!(recorder.effects_stats().unwrap().redirected, 0);
        let (frame, recorder) = record(&ram, start, 1, Some(on()), true);
        assert_eq!(palette_rows(&frame), as_is);
        assert!(recorder.clut_redirect.is_none());
        // Outside the flight table (the frame after), nothing.
        let mut gpu = SoftGpu::new(Vram::new());
        let mut recorder = Recorder::new(
            Options {
                scale: 2,
                dither: false,
            },
            &gpu,
        );
        recorder.set_effects(Some(fx::Config {
            enabled: true,
            ..fx::Config::default()
        }));
        register_recorder(&mut recorder, on());
        recorder.take_frame(gpu.vram());
        let words: Vec<u32> = FULL_AREA.iter().copied().chain(sprite(256, 0)).collect();
        for (i, &word) in words.iter().enumerate() {
            recorder.gp0(word, 0x8010_0004 + 4 * i as u32, gpu.vram());
            gpu.write_gp0(word).unwrap();
        }
        assert_eq!(palette_rows(&recorder.take_frame(gpu.vram())), [256]);
        // A movie (24-bit display) is no effects frame.
        let mut gpu = SoftGpu::new(Vram::new());
        let mut recorder = Recorder::new(
            Options {
                scale: 2,
                dither: false,
            },
            &gpu,
        );
        recorder.set_effects(Some(fx::Config {
            enabled: true,
            ..fx::Config::default()
        }));
        register_recorder(&mut recorder, on());
        recorder.list_begin(&ram, start, gpu.vram(), true);
        for (address, words) in crate::flight_host::walk_list_addressed(&ram, start).unwrap() {
            for (i, &word) in words.iter().enumerate() {
                recorder.gp0(word, address + 4 * (i as u32 + 1), gpu.vram());
                gpu.write_gp0(word).unwrap();
            }
        }
        recorder.list_end(gpu.vram());
        assert_eq!(palette_rows(&recorder.take_frame(gpu.vram())), as_is);
    }

    #[test]
    fn the_kick_reads_masks_animation_and_no_terrain() {
        let mut ram = fx::scene::tests::flight_ram();
        let start = fx::walk::tests::flight_list_in(&mut ram, &frame_packets());
        let (world, mission) = (0x800f_0000u32, 0x800f_2000u32);
        // Depth masks; an animated entry at (x 3, row 5).
        put(&mut ram, world, WORLD_DEPTH_MASKS);
        put(&mut ram, mission + ANIMATED_COUNT, 1);
        put(&mut ram, mission + ANIMATED_RECORDS, (5 << 4) | 3);
        let (frame, _) = record(&ram, start, 2, Some(on()), true);
        // Rows 0 (masked: 24), 5 (masked, its own entry overwritten: 29),
        // 40 (+128).
        assert_eq!(palette_rows(&frame), [256, 408, 413, 424, 256, 256]);
        let fx = frame.fx.as_deref().unwrap();
        let data = fx.data[0].1.downcast_ref::<FrameData>().copied().unwrap();
        assert_eq!(data.palettes, Palettes::new(true, 1 << 5));
        // Without the masks, row 5 keeps the game's palette.
        put(&mut ram, world, 0);
        let (frame, _) = record(&ram, start, 2, Some(on()), true);
        assert_eq!(palette_rows(&frame), [256, 384, 261, 424, 256, 256]);
        // No terrain: no haze.
        put(&mut ram, world, WORLD_NO_TERRAIN);
        let (frame, _) = record(&ram, start, 2, Some(on()), true);
        let fx = frame.fx.as_deref().unwrap();
        let data = fx.data[0].1.downcast_ref::<FrameData>().copied().unwrap();
        assert!(!data.haze);
        // A semi-transparent sprite with a terrain palette is counted.
        let mut packets = frame_packets();
        let mut semi = sprite(256 + 7, 48);
        semi[1] = 0x7e80_8080;
        packets.push((2500, semi));
        let mut semi_ram = fx::scene::tests::flight_ram();
        let semi_start = fx::walk::tests::flight_list_in(&mut semi_ram, &packets);
        let (frame, _) = record(&semi_ram, semi_start, 2, Some(on()), true);
        let fx = frame.fx.as_deref().unwrap();
        let data = fx.data[0].1.downcast_ref::<FrameData>().copied().unwrap();
        assert_eq!(data.semi_terrain, 1);
        // A record outside RAM keeps every row.
        put(&mut ram, world, 0);
        put(&mut ram, mission + ANIMATED_COUNT, 100_000);
        let (frame, _) = record(&ram, start, 2, Some(on()), true);
        assert_eq!(palette_rows(&frame), [256, 256, 261, 296, 256, 256]);
    }

    #[test]
    fn the_ground_level_is_the_lowest_wide_flat_level() {
        let mut ram = vec![0u8; 0x20_0000];
        let (grid, tiles) = (0x8010_0000u32, 0x8013_0000u32);
        // Types 1 (flat at -1024), 2 (flat at -1126), 3 (hills), 4 (flat at
        // -900, a few cells).
        put(&mut ram, tiles, 4);
        for (t, (floor, top)) in [
            (-1024i16, -1024i16),
            (-1126, -1126),
            (-410, -3000),
            (-900, -900),
        ]
        .into_iter()
        .enumerate()
        {
            let mesh = 0x8014_0000 + 0x100 * t as u32;
            put(&mut ram, tiles + 4 * (t as u32 + 1), mesh);
            put(
                &mut ram,
                mesh + 0x14,
                u32::from(floor as u16) | (u32::from(top as u16) << 16),
            );
        }
        let set = |ram: &mut Vec<u8>, from: u32, to: u32, tile: u16| {
            for cell in from..to {
                let at = ((grid + 2 * cell) & 0x1f_ffff) as usize;
                ram[at..at + 2].copy_from_slice(&tile.to_le_bytes());
            }
        };
        set(&mut ram, 0, 20_000, 1 | 0x200);
        set(&mut ram, 20_000, 50_000, 2);
        set(&mut ram, 50_000, 65_000, 3);
        set(&mut ram, 65_000, 65_536, 4);
        assert_eq!(ground_level(&ram, grid, tiles), Some(-1024.0));
        // Too little sea: the lowest wide level is the land.
        set(&mut ram, 0, 20_000, 2);
        set(&mut ram, 0, 3000, 1);
        assert_eq!(ground_level(&ram, grid, tiles), Some(-1126.0));
        // No flat level covers 1/16: the most common.
        set(&mut ram, 0, 65_536, 3);
        set(&mut ram, 0, 3000, 1);
        set(&mut ram, 3000, 3500, 4);
        assert_eq!(ground_level(&ram, grid, tiles), Some(-1024.0));
        assert_eq!(ground_level(&ram, grid, 0x9000_0000), None);
    }

    // ---- the near-terrain fallback ----

    #[test]
    fn coarse_depth_cases() {
        let eye = -3000.0;
        let sea = Some(-1024.0);
        // Outside the world's slots: none.
        for slot in [0, 1, 4095] {
            assert_eq!(coarse_depth(slot, 0.5, eye, sea), None, "{slot}");
        }
        // The slot's depth: the middle of its 8 units.
        assert_eq!(coarse_depth(900, -0.2, eye, sea), Some(7196.0));
        assert_eq!(coarse_depth(2, 0.0, eye, sea), Some(12.0));
        // A ray going down meets the sea plane 1976 units below the eye.
        assert_eq!(coarse_depth(900, 0.5, eye, sea), Some(3952.0));
        // Nearer than the plane: the slot.
        assert_eq!(coarse_depth(300, 0.5, eye, sea), Some(2396.0));
        // No plane, or the eye below the sea: the slot.
        assert_eq!(coarse_depth(900, 0.5, eye, None), Some(7196.0));
        assert_eq!(coarse_depth(900, 0.5, 0.0, sea), Some(7196.0));
        // Level or up: the slot.
        assert_eq!(coarse_depth(900, 0.0, eye, sea), Some(7196.0));
    }

    // ---- the card half on the GPU ----

    fn device() -> Option<&'static (wgpu::Device, wgpu::Queue)> {
        static DEVICE: std::sync::OnceLock<Option<(wgpu::Device, wgpu::Queue)>> =
            std::sync::OnceLock::new();
        DEVICE
            .get_or_init(|| execute::offscreen_device().ok())
            .as_ref()
            .or_else(|| {
                assert!(
                    std::env::var_os("AC3_REQUIRE_GPU").is_none(),
                    "AC3_REQUIRE_GPU: no wgpu device"
                );
                eprintln!("skipped: no graphics adapter");
                None
            })
    }

    /// A flight scene: level, looking along +z with a slight pitch down,
    /// the eye 1976 units above the sea, H 256, the centre at (w/2, h/2).
    fn test_scene(w: i32, h: i32) -> Scene {
        // Pitch down by about 8 degrees: rows of the world-to-view matrix.
        let (c, s) = (4056i16, 570i16);
        Scene {
            world: 0x800f_0000,
            h: 256,
            near: 16,
            far: 28000,
            eye: [0, -3000 * 4096, 0],
            view: [[4096, 0, 0], [0, c, -s], [0, s, c]],
            centre: [w / 2, h / 2],
            sun: [0, -1500, 3800],
            sun_colour: [3500, 3300, 3000],
            fog: [91, 99, 91],
            x_scale: (1, 1),
            ..Scene::default()
        }
    }

    /// Textures for a post over a `w` x `h` (1x) area at (0, 0) at `scale`.
    struct Rig {
        scale: u32,
        w: u32,
        h: u32,
        vram: wgpu::Texture,
        shadow: wgpu::Texture,
        zorder: wgpu::Texture,
        snapshot: wgpu::Texture,
        scene_buffer: wgpu::Buffer,
    }

    impl Rig {
        fn new(device: &wgpu::Device, scale: u32, w: u32, h: u32) -> Self {
            use wgpu::TextureUsages as U;
            let size = (w * scale, h * scale);
            Self {
                scale,
                w,
                h,
                vram: execute::texture(
                    device,
                    "test vram",
                    size,
                    VRAM_FORMAT,
                    U::RENDER_ATTACHMENT | U::COPY_SRC | U::COPY_DST | U::TEXTURE_BINDING,
                ),
                shadow: execute::texture(
                    device,
                    "test shadow",
                    size,
                    VRAM_FORMAT,
                    U::COPY_DST | U::COPY_SRC | U::TEXTURE_BINDING,
                ),
                zorder: execute::texture(
                    device,
                    "test zorder",
                    size,
                    fx::card::ZORDER_FORMAT,
                    U::TEXTURE_BINDING,
                ),
                snapshot: execute::texture(
                    device,
                    "test snapshot",
                    size,
                    fx::card::ZORDER_FORMAT,
                    U::TEXTURE_BINDING | U::COPY_DST,
                ),
                scene_buffer: execute::buffer(
                    device,
                    "test scene",
                    (fx::scene::UNIFORM_WORDS * 4) as u64,
                    wgpu::BufferUsages::UNIFORM,
                ),
            }
        }

        fn pixels(&self) -> usize {
            (self.w * self.scale * self.h * self.scale) as usize
        }

        /// Upload the picture (both VRAM and its copy) and the snapshot.
        fn load(&self, queue: &wgpu::Queue, picture: &[u16], snapshot: &[[f32; 2]]) {
            let (w, h) = (self.w * self.scale, self.h * self.scale);
            let extent = wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            };
            let bytes: Vec<u8> = picture.iter().flat_map(|p| p.to_le_bytes()).collect();
            for texture in [&self.vram, &self.shadow] {
                queue.write_texture(
                    texture.as_image_copy(),
                    &bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 2),
                        rows_per_image: Some(h),
                    },
                    extent,
                );
            }
            let bytes: Vec<u8> = snapshot
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            queue.write_texture(
                self.snapshot.as_image_copy(),
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 8),
                    rows_per_image: Some(h),
                },
                extent,
            );
        }

        /// Run `run` with a post context over the rig, then read VRAM back.
        fn post(
            &self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            frame: &FxFrame,
            data: Option<&FrameData>,
            run: impl FnOnce(&mut Context<'_>),
        ) -> Vec<u16> {
            let words = frame.scene.uniform(self.scale, [0, 0]);
            let bytes: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
            queue.write_buffer(&self.scene_buffer, 0, &bytes);
            let mut encoder = device.create_command_encoder(&Default::default());
            let vram = self.vram.create_view(&Default::default());
            let shadow = self.shadow.create_view(&Default::default());
            let zorder = self.zorder.create_view(&Default::default());
            let snapshot = self.snapshot.create_view(&Default::default());
            let area = Rect::new(0, 0, self.w as i32, self.h as i32);
            {
                let mut ctx = Context {
                    device,
                    queue,
                    encoder: &mut encoder,
                    scale: self.scale,
                    area,
                    pixels: execute::scaled(area, self.scale),
                    vram: &vram,
                    vram_texture: &self.vram,
                    shadow: &shadow,
                    shadow_texture: &self.shadow,
                    zorder: &zorder,
                    snapshot: Some(&snapshot),
                    scene: &frame.scene,
                    frame,
                    scene_buffer: &self.scene_buffer,
                    debug: fx::DebugView::Off,
                    data: data.map(|d| d as &(dyn Any + Send + Sync)),
                };
                run(&mut ctx);
            }
            queue.submit([encoder.finish()]);
            read_back(device, queue, &self.vram)
        }
    }

    fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u16> {
        let (w, h) = (texture.width(), texture.height());
        let row = (w * 2).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test readback"),
            size: u64::from(row * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let mapped = buffer.get_mapped_range(..).unwrap();
        let mut out = Vec::with_capacity((w * h) as usize);
        for y in 0..h as usize {
            let at = y * row as usize;
            out.extend(
                mapped[at..at + (w * 2) as usize]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]])),
            );
        }
        out
    }

    /// The test frame's draw orders: 1 terrain (slot 2500), 2 an aircraft
    /// (slot 1000), 3 terrain without depth (slot 900), 4 a sprite in the
    /// flare's slot, 5 terrain far away (slot 3400).
    fn test_frame(w: i32, h: i32) -> FxFrame {
        let t = Class::Terrain as u8;
        FxFrame {
            scene: test_scene(w, h),
            world_area: Rect::new(0, 0, w, h),
            classes: vec![t, Class::Polygon as u8, t, Class::Sprite as u8, t],
            slots: vec![2500, 1000, 900, 1, 3400],
            ..FxFrame::default()
        }
    }

    /// The depth case of 1x pixel (x, y): (z, order).
    fn depth_case(x: u32, y: u32) -> [f32; 2] {
        match (y / 4, x / 8) {
            (0, _) => [fx::SKY_Z, 0.0],
            (1, 0) => [800.0 + 400.0 * x as f32, 1.0],
            (1, _) => [26_000.0, 2.0],
            (2, 0) => [0.0, 3.0],
            (2, 1) => [0.0, 0.0],
            (2, 2) => [0.0, 4.0],
            (2, _) => [0.0, 99.0],
            (3, _) => [21_000.0 + 1000.0 * (x % 8) as f32, 5.0],
            _ => [3000.0 + 100.0 * y as f32, 1.0],
        }
    }

    /// The Rust model of a pixel's result (`None`: left alone).
    fn model(
        frame: &FxFrame,
        haze: &Haze,
        data: &FrameData,
        scale: u32,
        p: (u32, u32),
        old: u16,
        [z, order]: [f32; 2],
    ) -> Option<[f32; 3]> {
        let scene = &frame.scene;
        if z >= fx::SKY_Z * 0.5 {
            return None;
        }
        let view = scene_view(scene);
        let ray = pixel_ray(scene, scale, p.0, p.1);
        let world = view.to_world(ray);
        let entry = order_entry(frame, order as u32);
        let terrain = (entry >> 16) as u8 == TERRAIN_CLASS;
        let z = if z <= 0.0 {
            coarse_depth(
                (entry & 0xffff) as u16,
                world[1],
                scene.eye_units()[1],
                Some(data.sea_level),
            )?
        } else {
            z
        };
        let sky = sky_table(frame);
        let input = atmosphere::HazeInput {
            camera_y: scene.eye_units()[1],
            offset: world.map(|c| c * z),
            depth: if terrain { z } else { 0.0 },
        };
        Some(haze.shade(
            atmosphere::from_rgb555(old),
            &input,
            &haze_inputs(scene, &sky),
        ))
    }

    #[test]
    fn the_post_pass_matches_the_model() {
        let Some((device, queue)) = device() else {
            return;
        };
        let (w, h) = (64u32, 24u32);
        let frame = test_frame(w as i32, h as i32);
        let data = FrameData::default();
        for scale in [2u32, 4] {
            let rig = Rig::new(device, scale, w, h);
            let (nw, nh) = (w * scale, h * scale);
            let mut picture = Vec::with_capacity(rig.pixels());
            let mut snapshot = Vec::with_capacity(rig.pixels());
            for py in 0..nh {
                for px in 0..nw {
                    let (x, y) = (px / scale, py / scale);
                    let c = (x * 7 + y * 131) as u16;
                    picture.push((c & 0x7fff) | if x % 3 == 0 { 0x8000 } else { 0 });
                    snapshot.push(depth_case(x, y));
                }
            }
            rig.load(queue, &picture, &snapshot);
            let config = Config {
                on: true,
                density: 3.0,
                edge: true,
            };
            let haze = frame_haze(&config, &frame.scene, &data);
            let mut post = AtmospherePost::new(config);
            let out = rig.post(device, queue, &frame, Some(&data), |ctx| post.post(ctx));
            assert_eq!(post.stats.passes, 1);
            let (mut hazed, mut kept, mut worst) = (0, 0, 0.0f32);
            for py in 0..nh {
                for px in 0..nw {
                    let i = (py * nw + px) as usize;
                    let (old, new) = (picture[i], out[i]);
                    assert_eq!(new & 0x8000, old & 0x8000, "the mask bit at {px},{py}");
                    match model(&frame, &haze, &data, scale, (px, py), old, snapshot[i]) {
                        None => {
                            assert_eq!(new, old, "left alone at {px},{py}: {:?}", snapshot[i]);
                            kept += 1;
                        }
                        Some(rgb) => {
                            let got = atmosphere::from_rgb555(new);
                            for c in 0..3 {
                                // The dither moves a channel by up to one step.
                                let error = (got[c] - rgb[c]).abs() * 31.0;
                                worst = worst.max(error);
                                assert!(
                                    error <= 1.0 + 1e-3,
                                    "{scale}x {px},{py} {:?}: {got:?} vs {rgb:?}",
                                    snapshot[i]
                                );
                            }
                            hazed += 1;
                        }
                    }
                }
            }
            eprintln!("{scale}x: {hazed} hazed, {kept} kept, worst {worst:.3} steps");
            assert!(hazed > 0 && kept > 0);
        }
    }

    #[test]
    fn zero_density_keeps_every_colour() {
        let Some((device, queue)) = device() else {
            return;
        };
        // All 32,768 colours, with and without the mask bit, over terrain
        // at every depth from near to beyond the draw distance (no fade).
        let (w, h) = (256u32, 256u32);
        let scale = 2;
        let mut frame = test_frame(w as i32, h as i32);
        frame.classes = vec![TERRAIN_CLASS; 4];
        frame.slots = vec![2000, 3000, 4000, 4094];
        let rig = Rig::new(device, scale, w, h);
        let (nw, nh) = (w * scale, h * scale);
        let mut picture = Vec::new();
        let mut snapshot = Vec::new();
        for py in 0..nh {
            for px in 0..nw {
                let i = (py / scale) * w + px / scale;
                picture.push(i as u16);
                let order = (i % 4 + 1) as f32;
                let z = if i % 5 == 0 {
                    0.0
                } else {
                    50.0 + (i % 997) as f32 * 40.0
                };
                snapshot.push([z, order]);
            }
        }
        rig.load(queue, &picture, &snapshot);
        let data = FrameData::default();
        let config = Config {
            on: true,
            density: 0.0,
            edge: false,
        };
        let haze = frame_haze(&config, &frame.scene, &data);
        assert_eq!((haze.density, haze.edge), (0.0, None));
        let mut post = AtmospherePost::new(config);
        let out = rig.post(device, queue, &frame, Some(&data), |ctx| {
            post.draw(ctx, &haze, &data)
        });
        assert_eq!(post.stats.passes, 1, "the pass ran");
        let changed = picture.iter().zip(&out).filter(|(a, b)| a != b).count();
        assert_eq!(changed, 0, "density 0 without the fade changes no pixel");
        // And the effect itself skips the pass at density 0.
        let mut post = AtmospherePost::new(config);
        rig.post(device, queue, &frame, Some(&data), |ctx| post.post(ctx));
        assert_eq!((post.stats.passes, post.stats.skipped), (0, 1));
    }

    #[test]
    fn off_or_without_terrain_the_post_draws_nothing() {
        let Some((device, queue)) = device() else {
            return;
        };
        let (w, h) = (32u32, 16u32);
        let frame = test_frame(w as i32, h as i32);
        let rig = Rig::new(device, 2, w, h);
        let picture: Vec<u16> = (0..rig.pixels()).map(|i| (i * 13) as u16).collect();
        let snapshot: Vec<[f32; 2]> = (0..rig.pixels()).map(|_| [20_000.0, 1.0]).collect();
        rig.load(queue, &picture, &snapshot);
        let no_terrain = FrameData {
            haze: false,
            ..FrameData::default()
        };
        for (config, data) in [(Config::default(), None), (on(), Some(&no_terrain))] {
            let mut post = AtmospherePost::new(config);
            let out = rig.post(device, queue, &frame, data, |ctx| post.post(ctx));
            assert_eq!(out, picture, "{config:?} {data:?}");
            assert_eq!(post.stats.passes, 0);
        }
        // On without the recorder half's data: the defaults, a pass.
        let mut post = AtmospherePost::new(on());
        let out = rig.post(device, queue, &frame, None, |ctx| post.post(ctx));
        assert_ne!(out, picture);
    }

    /// A gradient band quad of the sky slot from screen row `y0` to `y1`
    /// (relative to the centre), `top` to `bottom` grey.
    fn sky_band(y0: i32, y1: i32, top: u32, bottom: u32) -> fx::SkyCommand {
        let xy = |x: i32, y: i32| (x as u32 & 0x7ff) | ((y as u32 & 0x7ff) << 16);
        let grey = |v: u32| v | (v << 8) | (v << 16);
        fx::SkyCommand {
            words: vec![
                0x3800_0000 | grey(top),
                xy(-160, y0),
                grey(top),
                xy(160, y0),
                grey(bottom),
                xy(-160, y1),
                grey(bottom),
                xy(160, y1),
            ],
            offset: [160, 120],
        }
    }

    #[test]
    #[ignore = "measures the post pass on the card (timestamp queries); prints the times"]
    fn post_pass_gpu_time() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY.with_env(),
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .expect("an adapter");
        let wanted =
            wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        assert!(adapter.features().contains(wanted), "timestamp queries");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("atmosphere timing"),
            required_features: wanted,
            required_limits:
                wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .unwrap();
        eprintln!("adapter: {}", adapter.get_info().name);
        // JP flight's screen: the ground under a sky, looking 8 degrees down.
        let (w, h) = (320u32, 240u32);
        let mut frame = test_frame(w as i32, h as i32);
        frame.sky = vec![
            sky_band(-120, -20, 0xd0, 0xa0),
            sky_band(-20, 10, 0xa0, 0x60),
            sky_band(10, 120, 0x5b, 0x5b),
        ];
        let data = FrameData::default();
        let config = Config {
            on: true,
            density: 3.0,
            edge: true,
        };
        let view = scene_view(&frame.scene);
        let eye_y = frame.scene.eye_units()[1];
        // As seen (the sky left alone), and every pixel terrain (the sky's
        // rows at 25,000: hazed, faded, with the order table's lookup).
        for (scale, all_terrain) in [(2u32, false), (4, false), (4, true)] {
            let rig = Rig::new(&device, scale, w, h);
            let (nw, nh) = (w * scale, h * scale);
            let mut snapshot = Vec::new();
            let mut orders = Vec::new();
            for py in 0..nh {
                for px in 0..nw {
                    let ray = view.to_world(pixel_ray(&frame.scene, scale, px, py));
                    let z = if ray[1] > 1e-4 {
                        (data.sea_level - eye_y) / ray[1]
                    } else {
                        f32::INFINITY
                    };
                    let z = if all_terrain { z.min(25_000.0) } else { z };
                    if z < 30_000.0 {
                        // A primitive per 8 x 8 block; every 7th without a depth.
                        let order = (py / scale / 8) * (w / 8) + px / scale / 8 + 1;
                        orders.push(order);
                        let depth = if order % 7 == 0 { 0.0 } else { z };
                        snapshot.push([depth, order as f32]);
                    } else {
                        snapshot.push([fx::SKY_Z, 0.0]);
                    }
                }
            }
            let count = orders.iter().max().copied().unwrap_or(0) as usize;
            frame.classes = vec![TERRAIN_CLASS; count];
            frame.slots = (0..count).map(|i| 900 + (i % 2600) as u16).collect();
            let picture: Vec<u16> = (0..nw * nh)
                .map(|i| (i.wrapping_mul(2_654_435_761) >> 17) as u16)
                .collect();
            rig.load(&queue, &picture, &snapshot);
            let words = frame.scene.uniform(scale, [0, 0]);
            let bytes: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
            queue.write_buffer(&rig.scene_buffer, 0, &bytes);
            let runs = 200u32;
            let set = device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("atmosphere timing"),
                ty: wgpu::QueryType::Timestamp,
                count: 2 * runs,
            });
            let resolve = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("atmosphere timing"),
                size: 16 * u64::from(runs),
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let readback = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("atmosphere timing readback"),
                size: 16 * u64::from(runs),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let vram = rig.vram.create_view(&Default::default());
            let shadow = rig.shadow.create_view(&Default::default());
            let zorder = rig.zorder.create_view(&Default::default());
            let snapshot_view = rig.snapshot.create_view(&Default::default());
            let area = Rect::new(0, 0, w as i32, h as i32);
            let mut post = AtmospherePost::new(config);
            let mut cpu = Vec::new();
            for run in 0..runs + 10 {
                let mut encoder = device.create_command_encoder(&Default::default());
                let timed = run >= 10;
                if timed {
                    encoder.write_timestamp(&set, 2 * (run - 10));
                }
                let started = std::time::Instant::now();
                {
                    let mut ctx = Context {
                        device: &device,
                        queue: &queue,
                        encoder: &mut encoder,
                        scale,
                        area,
                        pixels: execute::scaled(area, scale),
                        vram: &vram,
                        vram_texture: &rig.vram,
                        shadow: &shadow,
                        shadow_texture: &rig.shadow,
                        zorder: &zorder,
                        snapshot: Some(&snapshot_view),
                        scene: &frame.scene,
                        frame: &frame,
                        scene_buffer: &rig.scene_buffer,
                        debug: fx::DebugView::Off,
                        data: Some(&data as &(dyn Any + Send + Sync)),
                    };
                    post.post(&mut ctx);
                }
                if timed {
                    cpu.push(started.elapsed().as_secs_f64() * 1e3);
                    encoder.write_timestamp(&set, 2 * (run - 10) + 1);
                }
                if run == runs + 9 {
                    encoder.resolve_query_set(&set, 0..2 * runs, &resolve, 0);
                    encoder.copy_buffer_to_buffer(&resolve, 0, &readback, 0, 16 * u64::from(runs));
                }
                queue.submit([encoder.finish()]);
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            }
            readback.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let period = f64::from(queue.get_timestamp_period());
            let mapped = readback.get_mapped_range(..).unwrap();
            let ticks: Vec<u64> = mapped
                .chunks_exact(8)
                .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
                .collect();
            let mut gpu: Vec<f64> = ticks
                .chunks_exact(2)
                .map(|t| t[1].saturating_sub(t[0]) as f64 * period / 1e6)
                .collect();
            gpu.sort_by(f64::total_cmp);
            cpu.sort_by(f64::total_cmp);
            let sky = snapshot.iter().filter(|s| s[0] >= fx::SKY_Z).count();
            eprintln!(
                "{scale}x ({nw}x{nh}, {:.0}% sky, {count} orders): GPU median {:.3} ms, p90 {:.3} \
                 ms, min {:.3} ms; CPU (sky model, uploads, encoding) median {:.3} ms",
                100.0 * sky as f64 / snapshot.len() as f64,
                gpu[gpu.len() / 2],
                gpu[gpu.len() * 9 / 10],
                gpu[0],
                cpu[cpu.len() / 2],
            );
            assert_eq!(post.stats.passes, u64::from(runs) + 10);
        }
    }

    #[test]
    fn the_sky_model_falls_back_to_tiles_and_the_fog() {
        let mut frame = test_frame(320, 240);
        assert_eq!(sky_table(&frame), SkyTable::uniform(psx_unit([91, 99, 91])));
        frame.sky = vec![
            fx::SkyCommand {
                words: vec![0x6058_6058, 0, 16 | (64 << 16)],
                offset: [320, 256],
            },
            fx::SkyCommand {
                words: vec![0x6040_5060, 0, 320 | (240 << 16)],
                offset: [160, 120],
            },
        ];
        assert_eq!(
            sky_table(&frame),
            SkyTable::uniform(psx_unit([0x60, 0x50, 0x40]))
        );
        // A gradient quad over the screen: binned, on the 248 scale.
        frame.sky.push(fx::SkyCommand {
            words: vec![
                0x3800_0000 | 0xf8_f8f8,
                (-160i32 as u32 & 0x7ff) | ((-120i32 as u32 & 0x7ff) << 16),
                0xf8_f8f8,
                160 | ((-120i32 as u32 & 0x7ff) << 16),
                0x7c_7c7c,
                (-160i32 as u32 & 0x7ff) | (120 << 16),
                0x7c_7c7c,
                160 | (120 << 16),
            ],
            offset: [160, 120],
        });
        let table = sky_table(&frame);
        let sampled: Vec<[f32; 3]> = (0..atmosphere::SKY_BINS)
            .filter(|&b| table.sampled[b])
            .map(|b| table.colors[b])
            .collect();
        assert!(sampled.len() > 4, "{sampled:?}");
        assert!(sampled.iter().all(|c| c[0] > 0.45 && c[0] <= 1.0));
        assert!(sampled.iter().any(|c| c[0] > 0.95), "0xF8 is 31/31");
    }
}
