//! The HDR sun, its lens flare and bloom (Milestone 1 Phase 4 item 7;
//! `docs/EFFECTS_SUN.md`; `docs/RECOMP_ENHANCEMENTS.md` §2.8). Display only.
//!
//! With `[effects] sun` (and `enabled`), in Japanese flight with the GPU
//! renderer above 1x:
//! - **The game's own sun and flare are left out of the picture**
//!   ([`classify`], [`SunRecord`]; the software GPU and the game still draw
//!   them):
//!   - in the sky slot (entry 4095), the sun (`8008BA8C`): its disc, a
//!     semi-transparent textured quad with the palette (352, 316), and the
//!     four quads of its rotating glow, palette (352, 317); and the horizon
//!     glow strip of `80095930`, palette (352, 315); all from the texture
//!     page (576, 256);
//!   - in the flare slot (entry 1), every textured or semi-transparent
//!     primitive: the lens flare `8008B7AC` (page (512, 256)) and the screen
//!     flash (a semi-transparent 320 x 240 tile). Opaque untextured ones
//!     (letterbox clears) stay.
//! - **At the post** ([`SunCard`], `sun.wgsl`): the disc (limb-darkened, far
//!   brighter than white) and its aureole over the sky's pixels of the depth
//!   snapshot, at the sun's direction (world `+0x6C`) through the view
//!   matrix and the projection (widescreen's k included); and the sun's
//!   visibility: the share of its disc showing sky.
//! - **At the late step** (after the deferred smoke and explosions, before
//!   the flare slot and the HUD): bloom from a float mip chain of the world's
//!   picture (bright pixels and the sun's light beyond white), and the lens
//!   flare: the game's own ten flare elements (positions, sizes, thresholds
//!   and the colours of their textures, read from RAM and VRAM), a halo and
//!   a star, scaled by the visibility.
//!
//! Nothing here is game state: the recorder side reads RAM and the software
//! VRAM, the card side draws into the card's VRAM only.
use crate::gpu_renderer::execute::{VRAM_FORMAT, buffer, uint_texture_entry, uniform_entry};
use crate::gpu_renderer::fx::card::FX_COMMON_WGSL;
use crate::gpu_renderer::fx::{CardEffect, Context, Phase, Primitive, RecordEffect, Route, Scene};
use crate::gpu_renderer::{Executor, Recorder};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::sync::Arc;

/// The effect's name (both halves).
pub const NAME: &str = "sun";
/// The shader (after [`FX_COMMON_WGSL`]).
pub const SHADER: &str = include_str!("sun.wgsl");

/// The disc's angular radius (rad), about twice the real sun's 0.00465.
pub const DISC_RADIUS: f32 = 0.009;
/// The disc's radiance at its centre, in multiples of white.
pub const DISC_RADIANCE: f32 = 24.0;
/// Limb darkening: the radiance falls to 1 - LIMB at the edge.
pub const LIMB: f32 = 0.6;
/// The aureole over the sky: two exponentials of the angle from the sun,
/// (strength, width in rad).
pub const AUREOLE: [[f32; 2]; 2] = [[0.8, 0.025], [0.10, 0.06]];
/// The colour a sun at the horizon takes (times the light's tint): the disc
/// half of it, the aureole and halo all of it, fading out by
/// [`WARM_ELEVATION`].
pub const WARM: [f32; 3] = [1.0, 0.82, 0.58];
pub const WARM_ELEVATION: f32 = 0.35;
/// The bloom's bright pass: a pixel's light times a smooth step of its
/// largest channel from this to 1 (white, and the saturated parts of
/// explosions, count fully; the game's picture has no light beyond white).
pub const BLOOM_THRESHOLD: f32 = 0.8;
/// How much of the sun's light beyond white goes into the bloom.
pub const BLOOM_SUN: f32 = 0.3;
/// Levels of the bloom's mip chain, the first at the world area's 1 x size.
pub const BLOOM_LEVELS: u32 = 6;
/// The bloom's light per unit of strength (of the levels' sum).
pub const BLOOM_GAIN: f32 = 0.1;
/// The lens flare: the game's elements, our halo and star.
pub const FLARE_GHOSTS: f32 = 0.8;
pub const FLARE_HALO: f32 = 0.05;
pub const FLARE_STAR: f32 = 0.2;
/// The halo's radius and width, and the star's ray length (1 x pixels).
pub const HALO_RADIUS: f32 = 42.0;
pub const HALO_WIDTH: f32 = 4.0;
pub const STAR_LENGTH: f32 = 16.0;
/// The visibility samples cover this many disc radii.
pub const VISIBILITY_RADIUS: f32 = 1.5;

/// The game's flare elements (`8008B7AC`'s table at 800B440C, JP): kind,
/// brightness threshold, palette, half size, position scale (8 bytes each).
pub const FLARE_TABLE: u32 = 0x800b_440c;
pub const FLARE_ELEMENTS: usize = 10;
/// The flare's texture page (4-bit; `8008B7AC`'s E1 0x38).
pub const FLARE_PAGE: [i32; 2] = [512, 256];
/// The sun's texture page (`8008BA8C` and `80095930`, E1 0x39 or 0x59).
pub const SUN_PAGE: [i32; 2] = [576, 256];
/// The palettes of the horizon strip, the disc and its glow.
pub const STRIP_CLUT: [i32; 2] = [352, 315];
pub const DISC_CLUT: [i32; 2] = [352, 316];
pub const GLOW_CLUT: [i32; 2] = [352, 317];
/// Bins of a flare element's colour profile (centre to edge).
pub const PROFILE: usize = 16;
/// The sun word of the mission parameters (`[world+0x244]+0x6C`): high
/// byte, the disc; low byte, the flash and flare.
pub const SUN_WORD: u32 = 0x6c;

/// `[effects] bloom`: the bloom's strength, 0 (none) to [`Bloom::MAX`];
/// [`Bloom::DEFAULT`] by default (the settings check the range on loading).
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bloom(pub f64);

impl Bloom {
    pub const DEFAULT: f64 = 0.5;
    /// The largest value the settings accept.
    pub const MAX: f64 = 2.0;

    /// Whether loading accepts it (0 to [`Self::MAX`]).
    pub fn valid(self) -> bool {
        (0.0..=Self::MAX).contains(&self.0)
    }

    /// The strength as the bloom uses it: within 0 to [`Self::MAX`] (the
    /// default for NaN).
    pub fn factor(self) -> f32 {
        if self.0.is_nan() {
            Self::DEFAULT as f32
        } else {
            self.0.clamp(0.0, Self::MAX) as f32
        }
    }
}

impl Default for Bloom {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

/// `[effects] sun_glare`: the veiling glare's strength, a multiple of the
/// calibrated one ([`GLARE`]: at least the game's own glare); 0 (none) to
/// [`Glare::MAX`]; 1 by default (the settings check the range on loading).
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Glare(pub f64);

impl Glare {
    pub const DEFAULT: f64 = 1.0;
    /// The largest value the settings accept.
    pub const MAX: f64 = 3.0;

    /// Whether loading accepts it (0 to [`Self::MAX`]).
    pub fn valid(self) -> bool {
        (0.0..=Self::MAX).contains(&self.0)
    }

    /// The strength as the glare uses it: within 0 to [`Self::MAX`] (the
    /// default for NaN).
    pub fn factor(self) -> f32 {
        if self.0.is_nan() {
            Self::DEFAULT as f32
        } else {
            self.0.clamp(0.0, Self::MAX) as f32
        }
    }
}

impl Default for Glare {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

/// `[effects] sun_star`: the lens flare's four-ray star; on by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Star(pub bool);

impl Default for Star {
    fn default() -> Self {
        Self(true)
    }
}

/// The veiling glare at strength 1: light added over the whole world
/// picture (under the HUD) while the sun is on screen and visible, as the
/// game's own glare (§1 of `docs/EFFECTS_SUN.md`: the sky brightened toward
/// white, the horizon strip, the disc's glow) costs the player sight.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct GlareShape {
    /// A wide halo around the sun: its strength (times white) and angular
    /// width (rad; the halo falls as `e^(-θ / width)`).
    pub halo: f32,
    pub width: f32,
    /// A uniform veil over the picture (times white).
    pub veil: f32,
    /// Proximity to the view centre, as the game's flash: full at the
    /// centre, falling linearly with the Manhattan distance (1 x pixels, x
    /// unsqueezed) to `floor` at `range`.
    pub range: f32,
    pub floor: f32,
}

/// The calibrated glare (`sun_glare = 1`; `docs/EFFECTS_SUN.md` §6).
pub const GLARE: GlareShape = GlareShape {
    halo: 0.7,
    width: 0.12,
    veil: 0.12,
    range: 280.0,
    floor: 0.3,
};

impl GlareShape {
    /// The proximity factor of a sun at screen offset `offset` (1 x pixels,
    /// x unsqueezed): 1 at the centre to `floor` at `range` and beyond.
    pub fn proximity(&self, offset: [f32; 2]) -> f32 {
        let t = 1.0 - (offset[0].abs() + offset[1].abs()) / self.range.max(1.0);
        self.floor + (1.0 - self.floor) * t.clamp(0.0, 1.0)
    }
}

/// The effect's settings (`[effects] sun`, `bloom`, `sun_glare`,
/// `sun_star`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Settings {
    /// The bloom's strength (0: none).
    pub bloom: f32,
    /// The veiling glare's strength (0: none), times [`Self::shape`].
    pub glare: f32,
    /// Draw the flare's four-ray star.
    pub star: bool,
    pub shape: GlareShape,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            bloom: Bloom::DEFAULT as f32,
            glare: Glare::DEFAULT as f32,
            star: true,
            shape: GLARE,
        }
    }
}

impl Settings {
    /// The settings of `[effects]`, or `None` when `sun` is off.
    pub fn of(effects: &crate::settings::Effects) -> Option<Self> {
        effects.sun.then_some(Self {
            bloom: effects.bloom.factor(),
            glare: effects.sun_glare.factor(),
            star: effects.sun_star.0,
            shape: GLARE,
        })
    }
}

/// Register the recorder half on `recorder` when `[effects] sun` is on
/// (with effects configured: the Japanese release above 1x). Off, nothing
/// is registered: its late step would change the frame's layering.
pub fn register_recorder(recorder: &mut Recorder, effects: &crate::settings::Effects) {
    if recorder.effects_config().is_none() {
        return;
    }
    if let Some(settings) = Settings::of(effects) {
        eprintln!(
            "ac3: effects: sun on: the game's sun and flare replaced, bloom {}x, glare {}x{}",
            settings.bloom,
            settings.glare,
            if settings.star { "" } else { ", no star" }
        );
        recorder.add_effect(Box::new(SunRecord::new(settings)));
    }
}

/// Register the card half on `executor` (above 1x). It is inert in frames
/// without the recorder half's data.
pub fn register_card(executor: &mut Executor) {
    if executor.scale() > 1 {
        executor.add_effect(Box::new(SunCard::default()));
    }
}

// ---- the game's sun ----------------------------------------------------------------------

/// What the game draws for its sun ([`classify`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum GameSun {
    /// The horizon glow strip (`80095930`): sky slot, palette (352, 315).
    Strip,
    /// The sun's disc (`8008BA8C`): sky slot, palette (352, 316).
    Disc,
    /// The disc's rotating glow: sky slot, palette (352, 317).
    Glow,
    /// A textured primitive of the flare slot (the lens flare `8008B7AC`).
    Flare,
    /// A semi-transparent untextured primitive of the flare slot (the
    /// screen flash).
    Flash,
}

impl GameSun {
    pub const ALL: [Self; 5] = [
        Self::Strip,
        Self::Disc,
        Self::Glow,
        Self::Flare,
        Self::Flash,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Strip => "strip",
            Self::Disc => "disc",
            Self::Glow => "glow",
            Self::Flare => "flare",
            Self::Flash => "flash",
        }
    }
}

/// Whether `primitive` is part of the game's sun or flare (and which), by
/// its phase, command and texture.
pub fn classify(primitive: &Primitive<'_>) -> Option<GameSun> {
    let command = primitive.command;
    match primitive.phase {
        Phase::Sky => {
            // Semi-transparent textured quads (POLY_FT4 0x2E; 0x2C..0x2F),
            // 4-bit, from the sun's page.
            let texture = primitive.texture?;
            if command & 0xfc != 0x2c
                || !primitive.semi
                || texture.depth != 0
                || texture.page != SUN_PAGE
            {
                return None;
            }
            match texture.clut {
                STRIP_CLUT => Some(GameSun::Strip),
                DISC_CLUT => Some(GameSun::Disc),
                GLOW_CLUT => Some(GameSun::Glow),
                _ => None,
            }
        }
        Phase::Flare => {
            // Polygons, lines and rectangles; not fills or copies.
            if !(0x20..0x80).contains(&command) {
                return None;
            }
            if primitive.texture.is_some() {
                Some(GameSun::Flare)
            } else if primitive.semi {
                Some(GameSun::Flash)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The game's sun seen in a frame (before it was dropped): counts by kind,
/// and where the game put it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GameSunSeen {
    /// Primitives by [`GameSun::ALL`] order.
    pub counts: [u32; 5],
    /// The disc's four vertices (screen offsets from the drawing offset).
    pub disc: Option<[[i32; 2]; 4]>,
    /// The glow quads' shared corner: the sun's projected centre (the
    /// game's RTPS of its direction), offset from the drawing offset.
    pub glow_centre: Option<[i32; 2]>,
    /// The sky slot's drawing offset (1 x VRAM pixels).
    pub offset: [i32; 2],
}

impl GameSunSeen {
    fn add(&mut self, kind: GameSun, primitive: &Primitive<'_>) {
        let index = GameSun::ALL
            .iter()
            .position(|k| *k == kind)
            .expect("a kind");
        self.counts[index] += 1;
        let vertex = |word: u32| [i32::from(word as i16), i32::from((word >> 16) as i16)];
        let words = primitive.words;
        if words.len() >= 9 {
            match kind {
                GameSun::Disc => {
                    self.disc = Some([1, 3, 5, 7].map(|i| vertex(words[i])));
                    self.offset = primitive.offset;
                }
                GameSun::Glow => {
                    self.glow_centre = Some(vertex(words[7]));
                    self.offset = primitive.offset;
                }
                _ => {}
            }
        }
    }

    /// The game's sun position (offset from the projection centre, 1 x
    /// pixels): the glow's corner, else the disc's centre.
    pub fn position(&self) -> Option<[f32; 2]> {
        if let Some([x, y]) = self.glow_centre {
            return Some([x as f32, y as f32]);
        }
        self.disc.map(|corners| {
            let sum = corners
                .iter()
                .fold([0.0, 0.0], |s, c| [s[0] + c[0] as f32, s[1] + c[1] as f32]);
            [sum[0] / 4.0, sum[1] / 4.0]
        })
    }
}

// ---- the sun's projection ----------------------------------------------------------------

/// Where the sun is in a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct SunView {
    /// The direction toward the sun in view space (unit).
    pub direction: [f32; 3],
    /// Its projection: the offset from the projection centre in 1 x
    /// pixels (x squeezed by widescreen's k, as drawn); `None` behind the
    /// eye.
    pub screen: Option<[f32; 2]>,
}

/// The sun's direction (world `+0x6C`, toward the sun) through the view
/// matrix (world `+0x4C`) and the projection: screen offset = (k H x / z,
/// H y / z), as the game's RTPS of the sun object's direction (4 times
/// world `+0x6C`) gives it.
pub fn project(scene: &Scene) -> SunView {
    let sun = scene.sun.map(|v| f32::from(v) / 4096.0);
    let view = Scene::unit(&scene.view);
    let mut direction = view.map(|row| row[0] * sun[0] + row[1] * sun[1] + row[2] * sun[2]);
    let length = direction.iter().map(|v| v * v).sum::<f32>().sqrt();
    if length > 0.0 {
        direction = direction.map(|v| v / length);
    }
    let h = scene.h as f32;
    let screen = (direction[2] > 1e-3 && h > 0.0).then(|| {
        [
            scene.k() * h * direction[0] / direction[2],
            h * direction[1] / direction[2],
        ]
    });
    SunView { direction, screen }
}

/// How low the sun is, 1 at the horizon to 0 at [`WARM_ELEVATION`] (rad)
/// and above: its direction's elevation (world y is down).
pub fn warmth(scene: &Scene) -> f32 {
    let sun = scene.sun.map(|v| f32::from(v) / 4096.0);
    let length = sun.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
    let elevation = (-sun[1] / length).clamp(-1.0, 1.0).asin();
    let t = (elevation / WARM_ELEVATION).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// The rotation (rad) of the star at screen offset `offset` (1 x pixels): as
/// the game turns its sun's glow, `4 x + 8 y` in 4096ths of a turn.
pub fn star_angle(offset: [f32; 2]) -> f32 {
    (4.0 * offset[0] + 8.0 * offset[1]) * std::f32::consts::TAU / 4096.0
}

/// The game's flare brightness for a sun at screen offset `offset` (1 x
/// pixels, x unsqueezed), as `8008BA8C` computes it: `(312 - (|x| + |y|))
/// 4096 / 312`, then `(b - 512) / 32 + 24`, at most 240 (without its random
/// 0..7 and without the fold where the game's screen flash takes over).
pub fn flare_level(offset: [f32; 2]) -> f32 {
    let brightness = (312.0 - (offset[0].abs() + offset[1].abs())) * 4096.0 / 312.0;
    ((brightness - 512.0) / 32.0 + 24.0).clamp(0.0, 240.0)
}

// ---- the game's flare elements -----------------------------------------------------------

/// One element of the game's lens flare, and the colours of its texture
/// by radius.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct FlareElement {
    /// 0 and 1: four mirrored quarter quads of a 32 x 32 texture (u 0xC0 +
    /// 32 kind, v 0x20); else one quad of a 16 x 16 one (u 0xE0 + 16 (kind &
    /// 1), v 0).
    pub kind: u8,
    /// Drawn when the flare's brightness exceeds it, with grey (brightness
    /// - threshold).
    pub threshold: u8,
    pub clut: u16,
    /// Half its size (1 x pixels; widescreen squeezes x).
    pub size: i16,
    /// Its position: the sun's screen offset times scale / 4096.
    pub scale: i16,
    /// The texture's mean colour (0..1) in [`PROFILE`] rings from the
    /// centre to the edge.
    pub profile: [[f32; 3]; PROFILE],
}

/// The game's flare elements from RAM (the table) and the software VRAM
/// (their textures and palettes); `None` when the table is not in RAM.
pub fn flare_elements(ram: &[u8], vram: &[u16]) -> Option<[FlareElement; FLARE_ELEMENTS]> {
    use crate::gpu_renderer::fx::scene::word;
    let mut elements = [FlareElement::default(); FLARE_ELEMENTS];
    for (i, element) in elements.iter_mut().enumerate() {
        let at = FLARE_TABLE + i as u32 * 8;
        let first = word(ram, at)?;
        let second = word(ram, at + 4)?;
        element.kind = first as u8;
        element.threshold = (first >> 8) as u8;
        element.clut = (first >> 16) as u16;
        element.size = second as i16;
        element.scale = (second >> 16) as i16;
        element.profile = profile(element.kind, element.clut, vram);
    }
    Some(elements)
}

/// The colour of a 4-bit texel of the flare page through palette `clut`.
fn texel(vram: &[u16], clut: u16, u: i32, v: i32) -> [f32; 3] {
    const WIDTH: usize = 1024;
    let x = FLARE_PAGE[0] + u / 4;
    let y = FLARE_PAGE[1] + v;
    let Some(&word) = vram.get(y as usize * WIDTH + x as usize) else {
        return [0.0; 3];
    };
    let index = (word >> ((u & 3) * 4)) & 0xf;
    let cx = usize::from(clut & 0x3f) * 16;
    let cy = usize::from((clut >> 6) & 0x1ff);
    let colour = vram
        .get(cy * WIDTH + cx + usize::from(index))
        .copied()
        .unwrap_or(0);
    [0, 5, 10].map(|shift| f32::from((colour >> shift) & 31) / 31.0)
}

/// An element's texture as mean colours in [`PROFILE`] rings, centre to
/// edge (radius 1 = its half size). Kinds 0 and 1 are a quarter (the
/// centre at its last texel, mirrored four ways), others a whole 16 x 16
/// image centred on it.
pub fn profile(kind: u8, clut: u16, vram: &[u16]) -> [[f32; 3]; PROFILE] {
    let mut sums = [[0.0f32; 4]; PROFILE];
    let mut add = |r: f32, colour: [f32; 3]| {
        if r < 1.0 {
            let bin = ((r * PROFILE as f32) as usize).min(PROFILE - 1);
            for c in 0..3 {
                sums[bin][c] += colour[c];
            }
            sums[bin][3] += 1.0;
        }
    };
    if kind < 2 {
        let u0 = 0xc0 + 32 * i32::from(kind);
        for y in 0..32 {
            for x in 0..32 {
                let r = (((31 - x) * (31 - x) + (31 - y) * (31 - y)) as f32).sqrt() / 31.0;
                add(r, texel(vram, clut, u0 + x, 0x20 + y));
            }
        }
    } else {
        let u0 = 0xe0 + 16 * i32::from(kind & 1);
        for y in 0..16 {
            for x in 0..16 {
                let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
                add(
                    (dx * dx + dy * dy).sqrt() / 8.0,
                    texel(vram, clut, u0 + x, y),
                );
            }
        }
    }
    let mut profile = [[0.0f32; 3]; PROFILE];
    let filled: Vec<usize> = (0..PROFILE).filter(|&i| sums[i][3] > 0.0).collect();
    for (i, out) in profile.iter_mut().enumerate() {
        // An empty ring takes the nearest filled one.
        let Some(&from) = filled.iter().min_by_key(|&&f| f.abs_diff(i)) else {
            break;
        };
        let [r, g, b, n] = sums[from];
        *out = [r / n, g / n, b / n];
    }
    profile
}

// ---- the recorder side -------------------------------------------------------------------

/// What the recorder side hands the card side each effects frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SunFrame {
    pub settings: Settings,
    pub view: SunView,
    /// The mission's sun word (`[world+0x244]+0x6C`): 0, no sun.
    pub word: u16,
    pub elements: Option<[FlareElement; FLARE_ELEMENTS]>,
    /// The game's sun and flare, as seen (and dropped).
    pub game: GameSunSeen,
}

impl SunFrame {
    /// Whether the mission has a sun disc (the word's high byte).
    pub fn disc(&self) -> bool {
        self.word & 0xff00 != 0
    }
    /// Whether it has a lens flare (the word's low byte).
    pub fn flare(&self) -> bool {
        self.word & 0xff != 0
    }
}

/// The recorder side: drops the game's sun and flare, and hands the frame's
/// sun over ([`SunFrame`]).
pub struct SunRecord {
    settings: Settings,
    frame: Option<SunFrame>,
}

impl SunRecord {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            frame: None,
        }
    }
}

impl RecordEffect for SunRecord {
    fn name(&self) -> &'static str {
        NAME
    }

    fn kick(&mut self, scene: &Scene, ram: &[u8], vram: &ac3_graphics::Vram) {
        let word = crate::gpu_renderer::fx::scene::half(ram, scene.mission + SUN_WORD)
            .map_or(0, |half| half as u16);
        self.frame = Some(SunFrame {
            settings: self.settings,
            view: project(scene),
            word,
            elements: flare_elements(ram, vram.words()),
            game: GameSunSeen::default(),
        });
    }

    fn primitive(&mut self, primitive: &Primitive<'_>) -> Route {
        let Some(kind) = classify(primitive) else {
            return Route::Draw;
        };
        if let Some(frame) = self.frame.as_mut() {
            frame.game.add(kind, primitive);
        }
        Route::Drop
    }

    fn take(&mut self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.frame
            .take()
            .map(|frame| Arc::new(frame) as Arc<dyn Any + Send + Sync>)
    }

    fn idle(&mut self) {
        self.frame = None;
    }

    /// Bloom and the lens flare run at the late step.
    fn late(&self) -> bool {
        true
    }
}

// ---- the card side -----------------------------------------------------------------------

/// `vec4`s of the shader's `Sun` uniform.
pub const UNIFORM_ROWS: usize = 12 + FLARE_ELEMENTS + FLARE_ELEMENTS * PROFILE;

/// The `Sun` uniform of `sun.wgsl` for `frame` over the area `pixels` (N x:
/// x, y, w, h).
pub fn uniform(frame: &SunFrame, scene: &Scene, scale: u32, pixels: [u32; 4]) -> Vec<[f32; 4]> {
    let n = scale as f32;
    let k = scene.k();
    let h = scene.h as f32;
    let centre = [scene.centre[0] as f32, scene.centre[1] as f32];
    let mut rows = vec![[0.0f32; 4]; UNIFORM_ROWS];
    rows[0] = pixels.map(|v| v as f32);
    rows[1] = [centre[0], centre[1], k, n];
    let d = frame.view.direction;
    rows[2] = [d[0], d[1], d[2], h];
    let sun = frame.view.screen.filter(|_| frame.word != 0);
    let at = sun.map(|s| [(centre[0] + s[0]) * n, (centre[1] + s[1]) * n]);
    rows[3] = match at {
        Some([x, y]) => [x, y, 1.0, if frame.disc() { 1.0 } else { 0.0 }],
        None => [0.0; 4],
    };
    // The light's colour (4.12) as a tint: its largest channel 1.
    let colour = scene.sun_colour.map(|v| f32::from(v.max(0)) / 4096.0);
    let top = colour.iter().cloned().fold(0.0f32, f32::max);
    let tint = if top > 0.05 {
        colour.map(|c| c / top)
    } else {
        [1.0; 3]
    };
    let warm = warmth(scene);
    let disc: [f32; 3] =
        std::array::from_fn(|i| (0.5 + 0.5 * tint[i]) * (1.0 + (WARM[i] - 1.0) * 0.5 * warm));
    rows[4] = [disc[0], disc[1], disc[2], DISC_RADIANCE];
    let glow: [f32; 3] = std::array::from_fn(|i| tint[i] * (1.0 + (WARM[i] - 1.0) * warm));
    let radius = (DISC_RADIUS * h * n * VISIBILITY_RADIUS).max(2.0 * n);
    rows[5] = [glow[0], glow[1], glow[2], radius];
    rows[6] = [DISC_RADIUS, LIMB, AUREOLE[0][0], AUREOLE[0][1]];
    rows[7] = [AUREOLE[1][0], AUREOLE[1][1], 0.0, 0.0];
    rows[8] = [
        frame.settings.bloom * BLOOM_GAIN,
        BLOOM_THRESHOLD,
        0.0,
        BLOOM_SUN,
    ];
    let flare = at.is_some() && frame.flare();
    rows[9] = [
        FLARE_GHOSTS,
        FLARE_HALO,
        if frame.settings.star { FLARE_STAR } else { 0.0 },
        if flare { 1.0 } else { 0.0 },
    ];
    rows[10] = [
        HALO_RADIUS * n,
        HALO_WIDTH * n,
        STAR_LENGTH * n,
        star_angle(sun.unwrap_or_default()),
    ];
    // The veiling glare, by the sun's closeness to the view centre (its
    // visibility is applied on the card).
    if let (Some(s), true) = (sun, flare) {
        let shape = frame.settings.shape;
        let strength = frame.settings.glare * shape.proximity([s[0] / k.max(1e-3), s[1]]);
        rows[11] = [
            shape.halo * strength,
            shape.width,
            shape.veil * strength,
            0.0,
        ];
    }
    if let (Some(s), Some(elements), true) = (sun, frame.elements.as_ref(), flare) {
        let level = flare_level([s[0] / k.max(1e-3), s[1]]);
        for (i, element) in elements.iter().enumerate() {
            let scale = f32::from(element.scale) / 4096.0;
            let intensity = (level - f32::from(element.threshold)).max(0.0) / 128.0;
            rows[12 + i] = [
                (centre[0] + s[0] * scale) * n,
                (centre[1] + s[1] * scale) * n,
                f32::from(element.size.max(0)) * n,
                intensity,
            ];
            for (j, c) in element.profile.iter().enumerate() {
                rows[12 + FLARE_ELEMENTS + i * PROFILE + j] = [c[0], c[1], c[2], 0.0];
            }
        }
    }
    rows
}

/// The bloom's mip chain for one area size.
struct Chain {
    size: (u32, u32),
    levels: u32,
    down: Vec<wgpu::TextureView>,
    up: Vec<wgpu::TextureView>,
    /// Down level i (1..) reads level i - 1; up level i reads up i + 1 (or
    /// the last down level) and down level i.
    down_groups: Vec<wgpu::BindGroup>,
    up_groups: Vec<wgpu::BindGroup>,
}

/// The card side's resources, made at the first frame with the sun.
struct Gpu {
    uniform: wgpu::Buffer,
    sampler: wgpu::Sampler,
    visibility: wgpu::TextureView,
    visibility_layout: wgpu::BindGroupLayout,
    sun_layout: wgpu::BindGroupLayout,
    composite_layout: wgpu::BindGroupLayout,
    down_layout: wgpu::BindGroupLayout,
    up_layout: wgpu::BindGroupLayout,
    visibility_pipeline: wgpu::RenderPipeline,
    sun_pipeline: wgpu::RenderPipeline,
    bright_pipeline: wgpu::RenderPipeline,
    down_pipeline: wgpu::RenderPipeline,
    up_pipeline: wgpu::RenderPipeline,
    composite_pipeline: wgpu::RenderPipeline,
    chain: Option<Chain>,
}

/// The float format of the bloom chain and the visibility.
const FLOAT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn texture_entry(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

fn view_entry(binding: u32, view: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
    wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::TextureView(view),
    }
}

impl Gpu {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ac3 fx sun"),
            source: wgpu::ShaderSource::Wgsl(format!("{FX_COMMON_WGSL}\n{SHADER}").into()),
        });
        let layout = |label: &str, entries: &[wgpu::BindGroupLayoutEntry]| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some(label),
                entries,
            })
        };
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let visibility_layout = layout(
            "ac3 fx sun visibility",
            &[uniform_entry(0, fragment), texture_entry(2, false)],
        );
        let sun_layout = layout(
            "ac3 fx sun",
            &[
                uniform_entry(0, fragment),
                uint_texture_entry(1),
                texture_entry(2, false),
            ],
        );
        let composite_layout = layout(
            "ac3 fx sun composite",
            &[
                uniform_entry(0, fragment),
                uint_texture_entry(1),
                texture_entry(3, false),
                texture_entry(4, true),
                sampler_entry(5),
            ],
        );
        let down_layout = layout(
            "ac3 fx sun down",
            &[sampler_entry(5), texture_entry(6, true)],
        );
        let up_layout = layout(
            "ac3 fx sun up",
            &[
                sampler_entry(5),
                texture_entry(6, true),
                texture_entry(7, true),
            ],
        );
        let pipeline = |label: &str,
                        group: &wgpu::BindGroupLayout,
                        entry: &str,
                        format: wgpu::TextureFormat| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(group)],
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_sun"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let visibility_pipeline = pipeline(
            "ac3 fx sun visibility",
            &visibility_layout,
            "fs_visibility",
            FLOAT_FORMAT,
        );
        let sun_pipeline = pipeline("ac3 fx sun disc", &sun_layout, "fs_sun", VRAM_FORMAT);
        let bright_pipeline = pipeline("ac3 fx sun bright", &sun_layout, "fs_bright", FLOAT_FORMAT);
        let down_pipeline = pipeline("ac3 fx sun down", &down_layout, "fs_down", FLOAT_FORMAT);
        let up_pipeline = pipeline("ac3 fx sun up", &up_layout, "fs_up", FLOAT_FORMAT);
        let composite_pipeline = pipeline(
            "ac3 fx sun composite",
            &composite_layout,
            "fs_composite",
            VRAM_FORMAT,
        );
        let visibility = float_texture(device, "ac3 fx sun visibility", (1, 1), 1)
            .create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ac3 fx sun"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            uniform: buffer(
                device,
                "ac3 fx sun",
                (UNIFORM_ROWS * 16) as u64,
                wgpu::BufferUsages::UNIFORM,
            ),
            sampler,
            visibility,
            visibility_layout,
            sun_layout,
            composite_layout,
            down_layout,
            up_layout,
            visibility_pipeline,
            sun_pipeline,
            bright_pipeline,
            down_pipeline,
            up_pipeline,
            composite_pipeline,
            chain: None,
        }
    }

    /// Make the chain for a world area of `size` (1 x pixels), unless it
    /// is there.
    fn ensure_chain(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.chain.as_ref().is_none_or(|chain| chain.size != size) {
            let smallest = size.0.min(size.1).max(1);
            // Every level at least 2 pixels on its short side.
            let levels = BLOOM_LEVELS.min(smallest.ilog2().max(1));
            let down_texture = float_texture(device, "ac3 fx sun bloom down", size, levels);
            let up_texture = float_texture(
                device,
                "ac3 fx sun bloom up",
                size,
                levels.saturating_sub(1).max(1),
            );
            let mip = |texture: &wgpu::Texture, level: u32| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            };
            let down: Vec<_> = (0..levels).map(|l| mip(&down_texture, l)).collect();
            let up: Vec<_> = (0..levels.saturating_sub(1).max(1))
                .map(|l| mip(&up_texture, l))
                .collect();
            let sampler = wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(&self.sampler),
            };
            let down_groups = (1..levels as usize)
                .map(|l| {
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("ac3 fx sun down"),
                        layout: &self.down_layout,
                        entries: &[sampler.clone(), view_entry(6, &down[l - 1])],
                    })
                })
                .collect();
            let up_groups = (0..levels.saturating_sub(1) as usize)
                .map(|l| {
                    let coarser = if l + 2 == levels as usize {
                        &down[l + 1]
                    } else {
                        &up[l + 1]
                    };
                    device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("ac3 fx sun up"),
                        layout: &self.up_layout,
                        entries: &[
                            sampler.clone(),
                            view_entry(6, coarser),
                            view_entry(7, &down[l]),
                        ],
                    })
                })
                .collect();
            self.chain = Some(Chain {
                size,
                levels,
                down,
                up,
                down_groups,
                up_groups,
            });
        }
    }
}

fn float_texture(
    device: &wgpu::Device,
    label: &str,
    (width, height): (u32, u32),
    levels: u32,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: levels.max(1),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FLOAT_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// One full-target pass of `pipeline` with `group` into `target` (scissored
/// to `scissor` when given).
fn pass(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    group: &wgpu::BindGroup,
    scissor: Option<[u32; 4]>,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    if let Some([x, y, w, h]) = scissor {
        pass.set_scissor_rect(x, y, w, h);
    }
    pass.draw(0..3, 0..1);
}

/// What the card side has done.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SunStats {
    /// Posts with the sun's data; discs drawn (the sun in front, the
    /// mission with a disc); late steps; bloom chains run; flares drawn.
    pub posts: u64,
    pub discs: u64,
    pub lates: u64,
    pub blooms: u64,
    pub flares: u64,
}

/// The card side: the disc and visibility at the post, bloom and the lens
/// flare at the late step. Inert in frames without its recorder side's
/// data (`[effects] sun` off).
#[derive(Default)]
pub struct SunCard {
    gpu: Option<Gpu>,
    pub stats: SunStats,
}

impl SunCard {
    fn frame<'a>(ctx: &Context<'a>) -> Option<&'a SunFrame> {
        ctx.data.and_then(|data| data.downcast_ref::<SunFrame>())
    }
}

impl CardEffect for SunCard {
    fn name(&self) -> &'static str {
        NAME
    }

    fn post(&mut self, ctx: &mut Context<'_>) {
        let Some(frame) = Self::frame(ctx) else {
            return;
        };
        let Some(snapshot) = ctx.snapshot else {
            return;
        };
        let [_, _, w, h] = ctx.pixels;
        if w == 0 || h == 0 {
            return;
        }
        let gpu = self.gpu.get_or_insert_with(|| Gpu::new(ctx.device));
        self.stats.posts += 1;
        let rows = uniform(frame, ctx.scene, ctx.scale, ctx.pixels);
        let bytes: Vec<u8> = rows
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        ctx.queue.write_buffer(&gpu.uniform, 0, &bytes);
        // An effect before this one may have written VRAM.
        ctx.sync_area();
        let visibility_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 fx sun visibility"),
            layout: &gpu.visibility_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: gpu.uniform.as_entire_binding(),
                },
                view_entry(2, snapshot),
            ],
        });
        pass(
            ctx.encoder,
            "ac3 fx sun visibility",
            &gpu.visibility,
            &gpu.visibility_pipeline,
            &visibility_group,
            None,
        );
        if rows[3][2] > 0.0 && rows[3][3] > 0.0 {
            self.stats.discs += 1;
            let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ac3 fx sun"),
                layout: &gpu.sun_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: gpu.uniform.as_entire_binding(),
                    },
                    view_entry(1, ctx.shadow),
                    view_entry(2, snapshot),
                ],
            });
            pass(
                ctx.encoder,
                "ac3 fx sun disc",
                ctx.vram,
                &gpu.sun_pipeline,
                &group,
                Some(ctx.pixels),
            );
        }
    }

    fn late(&mut self, ctx: &mut Context<'_>) {
        let Some(frame) = Self::frame(ctx) else {
            return;
        };
        let Some(snapshot) = ctx.snapshot else {
            return;
        };
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };
        let [_, _, w, h] = ctx.pixels;
        if w == 0 || h == 0 {
            return;
        }
        self.stats.lates += 1;
        let bloom = frame.settings.bloom > 0.0;
        let flare = frame.flare() && frame.view.screen.is_some() && frame.word != 0;
        if !bloom && !flare {
            return;
        }
        let size = (ctx.area.w.max(1) as u32, ctx.area.h.max(1) as u32);
        gpu.ensure_chain(ctx.device, size);
        let chain = gpu.chain.as_ref().expect("made above");
        let uniform_entry = wgpu::BindGroupEntry {
            binding: 0,
            resource: gpu.uniform.as_entire_binding(),
        };
        if bloom {
            self.stats.blooms += 1;
            let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("ac3 fx sun bright"),
                layout: &gpu.sun_layout,
                entries: &[
                    uniform_entry.clone(),
                    view_entry(1, ctx.shadow),
                    view_entry(2, snapshot),
                ],
            });
            pass(
                ctx.encoder,
                "ac3 fx sun bright",
                &chain.down[0],
                &gpu.bright_pipeline,
                &group,
                None,
            );
            for (level, group) in chain.down_groups.iter().enumerate() {
                pass(
                    ctx.encoder,
                    "ac3 fx sun down",
                    &chain.down[level + 1],
                    &gpu.down_pipeline,
                    group,
                    None,
                );
            }
            for (level, group) in chain.up_groups.iter().enumerate().rev() {
                pass(
                    ctx.encoder,
                    "ac3 fx sun up",
                    &chain.up[level],
                    &gpu.up_pipeline,
                    group,
                    None,
                );
            }
        }
        if flare {
            self.stats.flares += 1;
        }
        // With one level the bloom is the bright pass itself.
        let bloom_view = if chain.levels > 1 {
            &chain.up[0]
        } else {
            &chain.down[0]
        };
        let group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ac3 fx sun composite"),
            layout: &gpu.composite_layout,
            entries: &[
                uniform_entry,
                view_entry(1, ctx.shadow),
                view_entry(3, &gpu.visibility),
                view_entry(4, bloom_view),
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
            ],
        });
        pass(
            ctx.encoder,
            "ac3 fx sun composite",
            ctx.vram,
            &gpu.composite_pipeline,
            &group,
            Some(ctx.pixels),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_renderer::fx::{TextureUse, scene};
    use crate::gpu_renderer::tiles::Rect;

    fn primitive<'a>(
        phase: Phase,
        words: &'a [u32],
        semi: bool,
        texture: Option<TextureUse>,
    ) -> Primitive<'a> {
        Primitive {
            phase,
            command: (words[0] >> 24) as u8,
            words,
            addresses: &[],
            offset: [160, 120],
            area: Rect::new(0, 0, 320, 240),
            semi,
            texture,
        }
    }

    fn sun_texture(clut: [i32; 2]) -> Option<TextureUse> {
        Some(TextureUse {
            depth: 0,
            page: SUN_PAGE,
            clut,
        })
    }

    /// The game's packets, word for word as `8008BA8C`, `80095930` and
    /// `8008B7AC` build them (vertex words after the command word).
    fn poly_ft4(colour: u32, clut: u16, tpage: u16) -> [u32; 9] {
        [
            0x2e00_0000 | colour,
            0x0010_0020,
            (u32::from(clut) << 16) | 0x2080,
            0x0010_0030,
            (u32::from(tpage) << 16) | 0x209f,
            0x0020_0020,
            0x3f80,
            0x0020_0030,
            0x3f9f,
        ]
    }

    #[test]
    fn the_games_sun_and_flare_are_classified_by_slot_and_texture() {
        let disc = poly_ft4(0x80_8080, 0x4f16, 0x39);
        let glow = poly_ft4(0x30_3030, 0x4f56, 0x39);
        let strip = poly_ft4(0x40_4040, 0x4ed6, 0x59);
        let sky = |words, clut| primitive(Phase::Sky, words, true, sun_texture(clut));
        assert_eq!(classify(&sky(&disc, DISC_CLUT)), Some(GameSun::Disc));
        assert_eq!(classify(&sky(&glow, GLOW_CLUT)), Some(GameSun::Glow));
        assert_eq!(classify(&sky(&strip, STRIP_CLUT)), Some(GameSun::Strip));
        // The same quad in the world, opaque, from another page or palette:
        // not the sun.
        let world = primitive(Phase::World, &disc, true, sun_texture(DISC_CLUT));
        assert_eq!(classify(&world), None);
        assert_eq!(
            classify(&primitive(Phase::Sky, &disc, false, sun_texture(DISC_CLUT))),
            None
        );
        let other_page = Some(TextureUse {
            depth: 0,
            page: [512, 256],
            clut: DISC_CLUT,
        });
        assert_eq!(
            classify(&primitive(Phase::Sky, &disc, true, other_page)),
            None
        );
        assert_eq!(classify(&sky(&disc, [352, 318])), None);
        // The sky's bands (untextured polygons, tiles) stay.
        let band = [0x3800_0000u32, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(classify(&primitive(Phase::Sky, &band, false, None)), None);
        // The flare slot: textured and semi-transparent drawing commands go;
        // opaque untextured ones (letterbox clears) and fills stay.
        let flare = poly_ft4(0x40_4040, 0x4e16, 0x38);
        let texture = Some(TextureUse {
            depth: 0,
            page: FLARE_PAGE,
            clut: [352, 312],
        });
        assert_eq!(
            classify(&primitive(Phase::Flare, &flare, true, texture)),
            Some(GameSun::Flare)
        );
        let flash = [0x6240_4040u32, 0xff88_ff60, 0x00f0_0140];
        assert_eq!(
            classify(&primitive(Phase::Flare, &flash, true, None)),
            Some(GameSun::Flash)
        );
        let letterbox = [0x6000_0000u32, 0xff88_ff60, 0x0020_0140];
        assert_eq!(
            classify(&primitive(Phase::Flare, &letterbox, false, None)),
            None
        );
        let fill = [0x0200_0000u32, 0, 0x00f0_0140];
        assert_eq!(classify(&primitive(Phase::Flare, &fill, false, None)), None);
        // The HUD keeps everything.
        assert_eq!(
            classify(&primitive(Phase::Hud, &flare, true, texture)),
            None
        );
        assert_eq!(classify(&primitive(Phase::Hud, &flash, true, None)), None);
    }

    #[test]
    fn the_seen_sun_keeps_the_glows_centre() {
        let mut seen = GameSunSeen::default();
        let mut glow = poly_ft4(0x30_3030, 0x4f56, 0x39);
        glow[7] = 0xfff6_0012; // (18, -10)
        let p = primitive(Phase::Sky, &glow, true, sun_texture(GLOW_CLUT));
        seen.add(GameSun::Glow, &p);
        assert_eq!(seen.glow_centre, Some([18, -10]));
        assert_eq!(seen.position(), Some([18.0, -10.0]));
        assert_eq!(seen.counts, [0, 0, 1, 0, 0]);
        let mut only_disc = GameSunSeen::default();
        let disc = poly_ft4(0x80_8080, 0x4f16, 0x39);
        only_disc.add(
            GameSun::Disc,
            &primitive(Phase::Sky, &disc, true, sun_texture(DISC_CLUT)),
        );
        // Vertices (32, 16), (48, 16), (32, 32), (48, 32).
        assert_eq!(only_disc.position(), Some([40.0, 24.0]));
    }

    #[test]
    fn the_sun_projects_through_the_view_matrix_and_widescreen() {
        let ram = scene::tests::flight_ram();
        let mut scene = Scene::read(&ram, None).unwrap();
        // Identity view, H 192, the sun ahead, up and to the right.
        let unit = |v: [f32; 3]| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            v.map(|c| (c / l * 4096.0).round() as i16)
        };
        scene.sun = unit([0.1, -0.05, 1.0]);
        let view = project(&scene);
        let [x, y] = view.screen.unwrap();
        let expected = |c: i16| f32::from(c) / f32::from(scene.sun[2]) * 192.0;
        assert!((x - expected(scene.sun[0])).abs() < 1e-3, "{x}");
        assert!((y - expected(scene.sun[1])).abs() < 1e-3, "{y}");
        // Widescreen squeezes x.
        scene.x_scale = (3, 4);
        let wide = project(&scene).screen.unwrap();
        assert!((wide[0] - 0.75 * x).abs() < 1e-3 && (wide[1] - y).abs() < 1e-3);
        // Behind the eye: no projection.
        scene.sun = unit([0.1, -0.05, -1.0]);
        assert_eq!(project(&scene).screen, None);
        // A rotated view: the sun straight ahead after a 90° yaw.
        scene.x_scale = (1, 1);
        scene.view = [[0, 0, -4096], [0, 4096, 0], [4096, 0, 0]];
        scene.sun = [4096, 0, 0];
        let ahead = project(&scene);
        assert_eq!(ahead.screen, Some([0.0, 0.0]));
        assert!((ahead.direction[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_low_sun_is_warm_and_its_star_turns_like_the_games_glow() {
        let mut scene = Scene {
            sun: [0, 0, 4096],
            ..Default::default()
        };
        assert_eq!(warmth(&scene), 1.0, "at the horizon");
        scene.sun = [0, -4096, 0];
        assert_eq!(warmth(&scene), 0.0, "overhead");
        // Mission 1: 2 degrees up, nearly all warm.
        scene.sun = [3705, -144, -1739];
        assert!(warmth(&scene) > 0.95);
        // 1024 of 4096: a quarter turn.
        let quarter = std::f32::consts::FRAC_PI_2;
        assert!((star_angle([256.0, 0.0]) - quarter).abs() < 1e-6);
        assert!((star_angle([0.0, 128.0]) - quarter).abs() < 1e-6);
    }

    #[test]
    fn the_flare_level_follows_the_games_curve() {
        // At the centre, 312 - 0 gives 4096: (4096 - 512) / 32 + 24 = 136.
        assert_eq!(flare_level([0.0, 0.0]), 136.0);
        // 78 pixels out: 3072 -> 104.
        assert_eq!(flare_level([50.0, -28.0]), 104.0);
        // At the screen's corner, dim; far beyond, none.
        assert!(flare_level([160.0, 120.0]) < 25.0);
        assert_eq!(flare_level([400.0, 0.0]), 0.0);
    }

    #[test]
    fn flare_profiles_average_the_texture_by_ring() {
        // A 16 x 16 element (kind 2) at u 0xE0: palette entry 1 inside
        // radius 0.5, entry 2 beyond.
        let mut vram = vec![0u16; 1024 * 512];
        let clut: u16 = (480 << 6) | (16 / 16);
        vram[480 * 1024 + 16 + 1] = 31; // red
        vram[480 * 1024 + 16 + 2] = 31 << 10; // blue
        for y in 0..16 {
            for x in 0..16 {
                let (dx, dy) = (x as f32 - 7.5, y as f32 - 7.5);
                let index: u16 = if (dx * dx + dy * dy).sqrt() / 8.0 < 0.5 {
                    1
                } else {
                    2
                };
                let u = 0xe0 + x;
                let at = (256 + y) * 1024 + 512 + u / 4;
                vram[at] |= index << ((u & 3) * 4);
            }
        }
        let profile = profile(2, clut, &vram);
        assert_eq!(profile[0], [1.0, 0.0, 0.0]);
        assert_eq!(profile[7], [1.0, 0.0, 0.0]);
        assert_eq!(profile[8], [0.0, 0.0, 1.0]);
        assert_eq!(profile[15], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn bloom_strengths_in_range_and_the_settings_of_the_sun() {
        assert!(Bloom(0.0).valid() && Bloom(2.0).valid() && Bloom(0.35).valid());
        assert!(!Bloom(-0.1).valid() && !Bloom(2.5).valid() && !Bloom(f64::NAN).valid());
        assert_eq!(Bloom(f64::NAN).factor(), 0.5);
        assert_eq!(Bloom(9.0).factor(), 2.0);
        assert_eq!(Bloom::default(), Bloom(Bloom::DEFAULT));
        let mut effects = crate::settings::Effects::default();
        assert_eq!(Settings::of(&effects), None, "off by default");
        effects.sun = true;
        effects.bloom = Bloom(0.25);
        effects.sun_glare = Glare(2.0);
        effects.sun_star = Star(false);
        let settings = Settings::of(&effects).unwrap();
        assert_eq!(
            (settings.bloom, settings.glare, settings.star),
            (0.25, 2.0, false)
        );
        assert_eq!(settings.shape, GLARE);
        assert!(Glare(0.0).valid() && Glare(3.0).valid() && !Glare(3.5).valid());
        assert!(!Glare(f64::NAN).valid() && Glare(f64::NAN).factor() == 1.0);
        assert_eq!(Star::default(), Star(true));
        // Proximity: 1 at the centre, the floor from the range on.
        assert_eq!(GLARE.proximity([0.0, 0.0]), 1.0);
        assert_eq!(GLARE.proximity([GLARE.range, 0.0]), GLARE.floor);
        assert_eq!(GLARE.proximity([400.0, 300.0]), GLARE.floor);
        let half = GLARE.proximity([GLARE.range / 4.0, -GLARE.range / 4.0]);
        assert!((half - (GLARE.floor + (1.0 - GLARE.floor) * 0.5)).abs() < 1e-6);
    }

    #[test]
    fn the_uniform_places_the_sun_and_the_flare() {
        let ram = scene::tests::flight_ram();
        let mut scene = Scene::read(&ram, None).unwrap();
        scene.sun = [0, 0, 4096];
        scene.sun_colour = [4096, 3072, 2048];
        let mut elements = [FlareElement::default(); FLARE_ELEMENTS];
        elements[0] = FlareElement {
            kind: 2,
            threshold: 36,
            size: 10,
            scale: -2048,
            profile: [[1.0, 0.5, 0.25]; PROFILE],
            ..Default::default()
        };
        let mut frame = SunFrame {
            settings: Settings::default(),
            view: project(&scene),
            word: 0x0101,
            elements: Some(elements),
            game: GameSunSeen::default(),
        };
        frame.view.screen = Some([20.0, -10.0]);
        let rows = uniform(&frame, &scene, 4, [0, 0, 1280, 960]);
        assert_eq!(rows.len(), UNIFORM_ROWS);
        // The centre (160, 120) plus the offset, at 4 x.
        assert_eq!(rows[3], [720.0, 440.0, 1.0, 1.0]);
        // A sun at the horizon: the tint (1, 0.75, 0.5) warmed all the way.
        let glow: Vec<f32> = rows[5][..3].to_vec();
        let expected = [1.0, 0.75 * WARM[1], 0.5 * WARM[2]];
        assert!(
            glow.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-6),
            "{glow:?}"
        );
        // Element 0 at -0.5 of the offset, 10 pixels: level 136 - 30 / 312
        // of 4096 / 32 ... less its threshold.
        let level = flare_level([20.0, -10.0]);
        assert_eq!(
            rows[12][..3],
            [(160.0 - 10.0) * 4.0, (120.0 + 5.0) * 4.0, 40.0]
        );
        assert!((rows[12][3] - (level - 36.0) / 128.0).abs() < 1e-6);
        assert_eq!(rows[12 + FLARE_ELEMENTS], [1.0, 0.5, 0.25, 0.0]);
        // No sun in the mission: nothing placed.
        frame.word = 0;
        let rows = uniform(&frame, &scene, 4, [0, 0, 1280, 960]);
        assert_eq!(rows[3], [0.0; 4]);
        assert_eq!(rows[12], [0.0; 4]);
    }
}
