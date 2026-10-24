//! The effects infrastructure (Milestone 1 Phase 4 item 4): what flight
//! effects (atmosphere, far terrain, sun and bloom, clouds, forest, ocean,
//! GI) plug into. The integration contract is `docs/EFFECTS.md`.
//!
//! All of it is display-only and, with `[effects] enabled = false` (the
//! default), inert: the recorder records exactly what it did before and the
//! executor runs exactly the same passes.
//!
//! An **effects frame** is a main-loop pass whose DrawOTag walks the flight
//! ordering table while the game is in flight (resident state 2), with the
//! GPU renderer above 1x and the display not a 24-bit movie, effects on
//! ([`Config::enabled`], F6 at run time). In it:
//! 1. **Phase markers** ([`walk`]): the walk passing entries 4095 (sky),
//!    4094 (world begin), 1 (world end) and 0 (HUD) splits the frame into
//!    [`Phase`]s.
//! 2. **Scene constants** ([`Scene`]) are read from machine RAM at the kick.
//! 3. **Depth**: the world's draws also write the `zorder` target (N x,
//!    `Rg32Float`): r the view z (perspective-interpolated from the GTE's
//!    unrounded SZ of their vertices; 0 for primitives without one), g the
//!    primitive's draw order in the world (1..; 0 where nothing of the world
//!    drew). World begin clears it over the world's draw area to
//!    ([`SKY_Z`], 0).
//! 4. **The post pass** at world end ([`Op::FxPost`](super::Op)): the
//!    depths are snapshot, then every [`CardEffect`]'s post runs, then the
//!    debug view ([`DebugView`]).
//! 5. **Deferred transparency**: semi-transparent world primitives
//!    (polygons, rectangles, lines; and VRAM copies onto the world's area)
//!    are queued and drawn after the post, in PlayStation order, each pixel
//!    tested against the snapshot: hidden where a later opaque world
//!    primitive drew ([draw order](EFFECT_ORDER)), or behind effect geometry
//!    by more than [`DEFER_TOLERANCE`]. With a no-op post the picture is the
//!    PlayStation's. Then the **late step** ([`Op::FxLate`](super::Op)):
//!    every [`CardEffect`]'s late runs over the picture with them, before
//!    the flare and the HUD (bloom, lens flares).
//! 6. **Write-back precision**: post results go back into the 15-bit VRAM
//!    through an 8 x 8 ordered dither at N x (`fx_encode` in
//!    `fx_common.wgsl`).
pub mod card;
pub mod scene;
pub mod walk;

use super::tiles::Rect;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::sync::Arc;

pub use card::{CardEffect, Context};
pub use scene::Scene;
pub use walk::{Fire, Marker, Schedule};

/// The view z of the sky: what world begin clears the z target to (nothing
/// with a depth drew there).
pub const SKY_Z: f32 = 1.0e30;
/// The draw order of effect geometry (the far terrain, trees, the ocean):
/// above every game primitive's (which count from 1 and stay below 2^24, so
/// their f32 is exact).
pub const EFFECT_ORDER: u32 = 1 << 24;
/// A deferred fragment behind effect geometry by more than this fraction of
/// its own depth is hidden.
pub const DEFER_TOLERANCE: f32 = 0.015;
/// Words per vertex of [`Frame::fx_vertices`](super::Frame): the view z (f32,
/// 0 for none) and the draw order (u32).
pub const FX_VERTEX_WORDS: usize = 2;

/// A debug view of the effects, drawn into the world's area at the post.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DebugView {
    #[default]
    Off,
    /// The z target: near white to far dark through a colour ramp
    /// (logarithmic), the sky dark blue, no depth magenta.
    Depth,
    /// Which pixels have a view z: green with a depth, red without, blue
    /// sky, over the picture's brightness.
    Coverage,
}

impl DebugView {
    pub const ALL: [Self; 3] = [Self::Off, Self::Depth, Self::Coverage];

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Depth => "depth",
            Self::Coverage => "coverage",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim().to_ascii_lowercase();
        Self::ALL
            .into_iter()
            .find(|view| view.name() == text)
            .ok_or_else(|| format!("unknown effects debug view {text}; use off, depth or coverage"))
    }

    /// The shader's mode number.
    pub fn index(self) -> u32 {
        self as u32
    }
}

/// How effects run; set by the host (`[effects]`, widescreen), changed at
/// run time by F6 (`enabled`). Never game state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// The master switch.
    pub enabled: bool,
    pub debug: DebugView,
    /// Widescreen's X squeeze in flight (`None`: 4:3).
    pub x_scale: Option<(u32, u32)>,
}

/// Where the recorder is in an effects frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub enum Phase {
    /// Not in an effects frame, or before its sky.
    #[default]
    Outside,
    /// Entry 4095's packets: the sky.
    Sky,
    /// Entries 4094..2: terrain and objects.
    World,
    /// Entry 1's packets: the flare (and the camera shake's offset).
    Flare,
    /// Entry 0's packets and what follows: the HUD and overlays.
    Hud,
}

/// The pipeline of an effects-frame draw (`Op::FxDraw`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Pipe {
    /// A world primitive: colour, and its view z and draw order.
    World,
    /// The z target's clear at world begin (no colour).
    ZClear,
    /// A deferred primitive, tested against the depth snapshot.
    Deferred,
}

/// What kind of primitive drew a pixel last (per draw order, for the
/// coverage numbers): [`FxFrame::classes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[repr(u8)]
pub enum Class {
    /// A textured polygon with a palette in the terrain block (320..335,
    /// 256..319), which the JP fog animates.
    Terrain = 1,
    /// Any other polygon (aircraft, objects, effects built of polygons).
    Polygon = 2,
    /// A rectangle (sprite).
    Sprite = 3,
    Line = 4,
    /// Fills, VRAM copies, uploads.
    Other = 5,
}

impl Class {
    pub const ALL: [Self; 5] = [
        Self::Terrain,
        Self::Polygon,
        Self::Sprite,
        Self::Line,
        Self::Other,
    ];
    pub fn of(value: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|class| *class as u8 == value)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Terrain => "terrain",
            Self::Polygon => "polygons",
            Self::Sprite => "sprites",
            Self::Line => "lines",
            Self::Other => "fills_copies",
        }
    }
}

/// A textured primitive's texture: depth (0 4-bit, 1 8-bit, 2 15-bit), the
/// page's VRAM position and the palette's (indexed depths only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureUse {
    pub depth: u32,
    pub page: [i32; 2],
    pub clut: [i32; 2],
}

/// A drawing command of an effects frame, as [`RecordEffect::primitive`]
/// sees it: before the card draws it, after the software GPU's checks (it is
/// not culled) and its texture page change.
#[derive(Clone, Copy, Debug)]
pub struct Primitive<'a> {
    pub phase: Phase,
    /// GP0 command byte (0x20..0x3F polygons, 0x40..0x5F lines, 0x60..0x7F
    /// rectangles, 0x80 copies, 0x02 fills).
    pub command: u8,
    /// The command's GP0 words and their RAM addresses (0: not from a
    /// DMA-list packet); vertex words keep the game's coordinates, before
    /// the drawing offset.
    pub words: &'a [u32],
    pub addresses: &'a [u32],
    /// The drawing offset and area (1x VRAM pixels).
    pub offset: [i32; 2],
    pub area: Rect,
    pub semi: bool,
    pub texture: Option<TextureUse>,
}

/// What [`RecordEffect::primitive`] decides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Route {
    /// Draw it (deferred if it is semi-transparent in the world).
    #[default]
    Draw,
    /// Leave it out of the card's picture (the software GPU and the game
    /// still draw it): a game sun, fog strips, sea polygons, backdrop strips.
    Drop,
}

/// The recorder side of an effect: sees the frame's stream and decides about
/// primitives. Every method has a no-op default. Registered with
/// [`Recorder::add_effect`](super::Recorder::add_effect).
pub trait RecordEffect {
    /// Its name; the card side's [`CardEffect::name`] of the same effect
    /// receives what [`Self::take`] returns.
    fn name(&self) -> &'static str;
    /// The frame's kick: its scene, and machine RAM and the software VRAM
    /// as the DrawOTag starts (read only; e.g. the terrain map).
    fn kick(&mut self, _scene: &Scene, _ram: &[u8], _vram: &ac3_graphics::Vram) {}
    /// A marker passed: `phase` begins.
    fn phase(&mut self, _phase: Phase, _scene: &Scene) {}
    /// A drawing command of the frame (any phase).
    fn primitive(&mut self, _primitive: &Primitive<'_>) -> Route {
        Route::Draw
    }
    /// Per-frame data for the card side, taken when the frame is.
    fn take(&mut self) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
    /// A recorded main-loop pass that was no effects frame (left flight,
    /// effects off, a movie): drop per-flight state if needed.
    fn idle(&mut self) {}
    /// Whether its card side works at the late step
    /// ([`CardEffect::late`]); the recorder places the step only when an
    /// effect asks.
    fn late(&self) -> bool {
        false
    }
}

/// The palette redirect of indexed textures ([`Recorder::clut_redirect`](super::Recorder)):
/// `(CLUT half-word, palette entries (16 or 256), phase)` to the half-word
/// to decode with instead, or `None` to keep it. `atmosphere::clut_word_redirect`
/// has this type.
pub type ClutRedirect = fn(u16, u16, crate::effects::atmosphere::Phase) -> Option<u16>;

/// A drawing command of the sky slot (entry 4095) of an effects frame, as
/// the game sent it ([`FxFrame::sky`]): its GP0 words (vertex coordinates
/// relative to `offset`, the projection centre) and the drawing offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkyCommand {
    pub words: Vec<u32>,
    pub offset: [i32; 2],
}

/// The effects part of a recorded [`Frame`](super::Frame): present when the
/// frame was an effects frame.
#[derive(Clone, Default)]
pub struct FxFrame {
    pub scene: Scene,
    /// The world's draw area (1x VRAM pixels): the area the z target is
    /// cleared over, the post works on and the snapshot covers.
    pub world_area: Rect,
    /// The markers fired, with their ordering-table nodes.
    pub markers: Vec<(Marker, u32)>,
    pub debug: DebugView,
    /// Each world draw order's [`Class`] and the ordering-table slot it
    /// hung off (index order - 1). Terrain's slot is about its view z / 8:
    /// a coarse depth where a primitive has none.
    pub classes: Vec<u8>,
    pub slots: Vec<u16>,
    /// The sky slot's drawing commands, in order (the sky model's input).
    pub sky: Vec<SkyCommand>,
    /// What each recorder-side effect handed to its card side.
    pub data: Vec<(&'static str, Arc<dyn Any + Send + Sync>)>,
}

impl std::fmt::Debug for FxFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FxFrame")
            .field("scene", &self.scene)
            .field("world_area", &self.world_area)
            .field("markers", &self.markers)
            .field("debug", &self.debug)
            .field("classes", &self.classes.len())
            .field("sky", &self.sky.len())
            .field(
                "data",
                &self.data.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// What the effects did since the recorder started.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FxStats {
    /// DMA lists walked for markers while effects were on, and the effects
    /// frames among them (all four markers).
    pub lists: u64,
    pub frames: u64,
    /// Frames not armed: outside flight (or no scene), a 24-bit display.
    pub not_flight: u64,
    pub movie: u64,
    /// Markers fired: sky, world begin, world end, HUD.
    pub markers: [u64; 4],
    /// World primitives, and the triangles among them with a view z.
    pub world_primitives: u64,
    pub depth_triangles: u64,
    pub flat_triangles: u64,
    /// Semi-transparent world primitives queued, replayed after the post,
    /// and replayed early (`flushes` times) because a later command read or
    /// wrote what they draw or read; those with mask bits among them.
    pub deferred: u64,
    pub replayed: u64,
    pub replayed_early: u64,
    pub flushes: u64,
    pub deferred_masked: u64,
    /// Primitives effects dropped, palettes they redirected.
    pub dropped: u64,
    pub redirected: u64,
    /// Effects frames whose post ran.
    pub posts: u64,
}

impl FxStats {
    pub(crate) fn count_marker(&mut self, marker: Marker) {
        let index = Marker::ALL
            .iter()
            .position(|m| *m == marker)
            .expect("a marker");
        self.markers[index] += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_views_by_name() {
        for view in DebugView::ALL {
            assert_eq!(DebugView::parse(view.name()).unwrap(), view);
        }
        assert_eq!(DebugView::parse(" Depth ").unwrap(), DebugView::Depth);
        assert!(DebugView::parse("normals").is_err());
        assert_eq!(DebugView::default(), DebugView::Off);
    }

    #[test]
    fn the_shader_constants_match() {
        let source = card::FX_DRAW_SHADER;
        assert!(
            source.contains(&format!("const SKY_Z: f32 = {SKY_Z:e};")),
            "SKY_Z"
        );
        assert!(source.contains(&format!("const EFFECT_ORDER: u32 = {EFFECT_ORDER}u;")));
        assert!(source.contains(&format!("const DEFER_TOLERANCE: f32 = {DEFER_TOLERANCE};")));
        // Draw orders below the effects' are exact in f32.
        assert_eq!((EFFECT_ORDER - 1) as f32 as u32, EFFECT_ORDER - 1);
    }
}
