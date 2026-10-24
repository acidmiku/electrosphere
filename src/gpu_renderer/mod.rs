//! The GPU renderer: the game's GPU commands drawn again on the graphics
//! card, at 1x to 8x the PlayStation's resolution, for display only.
//!
//! The software GPU (`ac3_graphics::Gpu`) stays authoritative: everything
//! the game can read (StoreImage, VRAM in save states, GPUSTAT) comes from
//! it, so game logic and save states never depend on this renderer. This
//! renderer is a shadow fed with the same stream (GP0 words from the DMA
//! lists and LoadImage, GP1 writes) and keeps its own VRAM on the card,
//! used only for what is shown. It is never saved; a quick load re-seeds it
//! from the software VRAM ([`Recorder::reseed`]).
//!
//! Two halves:
//! - [`Recorder`] (CPU, no graphics API): decodes the stream exactly as the
//!   software GPU does and records the main-loop pass as a [`Frame`] of
//!   operations and vertices. It is called before the software GPU executes
//!   each word, so the software VRAM it sees is the state the command reads.
//! - [`Executor`] (wgpu): runs a recorded frame on the card and draws the
//!   display area, into the window (the presenter shares its device) or
//!   offscreen (`machine --headless` with the renderer on).
//!
//! ## VRAM on the card
//! - `vram`: the N x VRAM (1024N x 512N, `R16Uint`), the render target. It
//!   holds PlayStation pixels (15-bit colour and the mask bit) at N x, so
//!   copies, fills and render-to-texture work at N x as on the console.
//! - `shadow`: a copy of `vram`, brought up to date only where something
//!   was drawn since the last update and a primitive is about to read it.
//!   Shaders cannot read their own render target, so semi-transparency, the
//!   mask check, 15-bit textures and VRAM copies read `shadow`; the recorder
//!   inserts the copy (an [`Op::Sync`]) exactly when a read overlaps a write
//!   since the last one ([`tiles`]), which keeps the PlayStation's draw order
//!   for blending.
//! - `mirror`: a 1 x copy of the authoritative software VRAM, for palettes
//!   (CLUTs) and 4/8-bit texels. Index data drawn at N x has no meaning, so
//!   indexed textures always decode from the 1 x data, which the recorder
//!   refreshes from the software VRAM (at the moment of use) wherever
//!   anything changed it. 15-bit texels instead sample `shadow` at N x, so a
//!   texture the game drew itself keeps the full resolution.
//!
//! ## Fidelity
//! All blending, modulation, masking and dithering is integer arithmetic in
//! the fragment shader on 5-bit channels, with the reference software GPU's
//! formulas (including its pair/single-pixel differences); there is no
//! hardware blending. Texture coordinates and colours interpolate affinely
//! (`@interpolate(linear)`), as on the PlayStation. At 1 x every polygon
//! and line is rasterized in the shader with the reference's own integer
//! edge walk, gradients and Bresenham steps, so 1 x matches the software
//! renderer pixel for pixel.
//!
//! Above 1 x each 1 x pixel is an N x N block. Rectangles, fills, copies
//! and lines cover whole blocks. Polygons are rasterized with the PlayStation
//! sample point of pixel i at the top-left corner of its block, so an edge
//! on whole pixels covers the same whole blocks as the 1 x top-left rule
//! (the draw area's first rows and columns too: the game never clears its
//! flight frames, so an undrawn sample there would keep an old frame's
//! pixel). Their texture coordinates and colours are read half a 1 x pixel
//! back from each sample, so a block's centre shows the 1 x pixel's texel
//! and shade (`draw.wgsl`). `tests/gpu_renderer_edges.rs` holds the picture
//! edges to the software picture at 1 x to 4 x.
//!
//! ## Perspective-correct textures
//! Above 1 x, with sub-pixel geometry and `[display] perspective`
//! ([`Recorder::perspective`]), textured polygons whose vertices all have a
//! depth from the [`VertexPrecision`] source (the GTE's unrounded SZ) are
//! texture-mapped, and Gouraud-shaded, with perspective instead of
//! affinely, so large ground and aircraft polygons no longer bend their
//! textures. Only their colours change: the shader divides screen-linear
//! uv/w, colour/w and 1/w per pixel, and keeps the clip position (so the
//! coverage) of the affine path, so their pixels, edges and the seams to
//! neighbours are exactly as without it. The texture coordinates are clamped
//! to each triangle's own, so no neighbouring texel (a 4-bit page, a texture
//! window, a replacement's edge) is ever reached. Polygons without depths
//! (sprites, the HUD, 2D effects built from projected points), lines,
//! rectangles and everything at 1 x keep the affine path. The game never
//! sees any of it.
//!
//! ## Texture replacement
//! With a texture pack (`[display] texture_pack`, `docs/ASSET_DUMP.md`),
//! textured primitives draw edited or upscaled PNGs instead of the game's
//! texels, at any scale; only this renderer's picture changes.
//! - [`replace`] (recorder side): the pack's index, which upload each
//!   primitive samples (tracked in stream order, keyed with the asset
//!   dump's functions), the lookup (exact palette, `any`, `nopal`, the
//!   original), and PNG decoding with the alpha rules.
//! - [`atlas`] (card side): the replacements in a layered `R16Uint` atlas
//!   (PlayStation texels, or palette indices drawn through the live
//!   palette), and a slot table the shader reads. A replaced primitive's
//!   vertices name its slot and where its upload starts in the page; the
//!   shader maps the texture coordinates into the replacement and then
//!   draws its texel exactly as an original one (modulation, the colour
//!   0x0000 and the STP bit, blending, masks). The draws stay batched: one
//!   pipeline, one bind group.
//! - Movie frames (the hub's clips, `docs/MOVIES.md` §9) ride on the same
//!   path above 1 x with a movie pack: each frame the game decodes into
//!   VRAM owns its rectangle like a pack texture, and draws from the
//!   pack's image of that frame, in a few movie slots reused as frames come
//!   ([`replace`], "Movie frames").
//!
//! ## Effects
//! [`fx`] is the infrastructure flight effects plug into (`docs/EFFECTS.md`):
//! phase markers from the DMA walk of the flight ordering table, the
//! frame's scene constants from RAM, a z target written beside VRAM by the
//! world's primitives, a post pass at world end, and deferred
//! semi-transparency. Off by default (`[effects] enabled`); then the
//! recorder and executor do exactly what they did before.
pub mod atlas;
pub mod execute;
pub mod fx;
pub mod record;
pub mod replace;
pub mod tiles;

pub use execute::{DisplayArea, Executor};
pub use record::{Frame, Op, Recorder, Shown, Stats};
pub use replace::TexturePack;

/// Sub-pixel position of the vertex word at `address` (the RAM address of a
/// GP0 vertex word inside a DMA-list packet): x, y in the same coordinate
/// space as the integer word (before the GPU's drawing offset), and z (the
/// GTE's unrounded SZ, the depth x and y were projected with: the w of
/// perspective-correct texturing; 0 or less for none).
///
/// Addresses are KSEG0 (`0x8000_0000 | physical`), as the translated code
/// writes them: a packet at physical `0x0010_0000` has its first GP0 word
/// at `0x8010_0004`.
///
/// `word` is the vertex word being drawn; a source answers only for the
/// position it recorded with exactly that word.
pub trait VertexPrecision {
    fn precise_vertex(&self, address: u32, word: u32) -> Option<[f32; 3]>;
}

/// The machine's sub-pixel table ([`ac3_flight::precision`], filled by the
/// translated code's GTE results) as the renderer's source. The machine only
/// borrows the table inside its own loads and stores, never while a device
/// call (and so the recorder) runs.
pub struct TablePrecision(pub ac3_flight::precision::SharedPrecision);

impl VertexPrecision for TablePrecision {
    fn precise_vertex(&self, address: u32, word: u32) -> Option<[f32; 3]> {
        self.0.try_borrow().ok()?.position(address, word)
    }
}

/// The highest internal resolution.
pub const MAX_SCALE: u32 = 8;

/// How the renderer draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Internal resolution, 1..=8 times the PlayStation's.
    pub scale: u32,
    /// Dither shaded surfaces above 1 x as the game asks (scaled with the
    /// picture). At 1 x the game's dithering always applies.
    pub dither: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scale: 1,
            dither: false,
        }
    }
}
