//! The recorder: the GPU renderer's command side, on the CPU.
//!
//! [`Recorder`] receives every GP0 word and GP1 write the software GPU gets,
//! each just before the software GPU executes it, and decodes them with the
//! software GPU's own rules (`ac3_graphics::gpu`: command lengths, polyline
//! terminators, the drawing state E1..E6, texture pages set by polygons,
//! culling, offsets and clipping). It records the main-loop pass as a
//! [`Frame`] of [`Op`]s and the vertices they draw, which the
//! [`Executor`](super::Executor) runs on the card when the pass ends.
//!
//! Vertices are in 1 x VRAM coordinates; the shader scales them to N x. At
//! 1 x (`exact`), polygons and lines are drawn as bounding boxes and the
//! fragment shader decides coverage and every attribute with the software
//! GPU's integer arithmetic (the data for that rides in the flat vertex
//! words). Above 1 x they are real triangles and parallelograms with
//! affinely interpolated attributes.
//!
//! ## Perspective-correct textures
//! Above 1 x, with a precision source and [`Recorder::perspective`], a
//! textured polygon whose every vertex has a sub-pixel position with a
//! depth (the GTE's unrounded SZ, above 0) carries those depths as its
//! vertices' w and [`mode::PERSPECTIVE`]: the shader interpolates its
//! texture coordinates and colours with perspective. A quad decides for
//! both of its triangles together, so the halves agree along their shared
//! edge. Every other primitive keeps w = 1 and the affine path, and its
//! vertices are word for word what they are with perspective off.
//!
//! ## Layers
//! A primitive that reads the card's VRAM (semi-transparency and the mask
//! check under itself, 15-bit texels, VRAM copies) reads `shadow`, a copy
//! that only a render-pass break can update. So the frame is recorded as
//! layers, each one render pass, with `shadow` brought up to date between
//! them. Each primitive goes into the first layer that keeps the
//! PlayStation's result:
//! - after every earlier layer that drew pixels it reads;
//! - not before any earlier primitive that touched its own pixels (it then
//!   draws after it, in the same layer or a later one);
//! - for the 1 x mirror, which uploads and refreshes write between render
//!   passes (at the start of a layer), after the write it reads and before
//!   any later write over what it reads.
//!
//! Overlap is tested on exact rectangles (bounding boxes), so primitives
//! that do not touch do not wait for each other, and within a layer the
//! command order is kept. Independent semi-transparent sprites, the common
//! case, share one layer.
//!
//! ## The shown field
//! The screen shows the field after the latest VBlank: the display area as
//! that VBlank left it (`machine_player`'s scanout). The host marks the
//! point in the stream, the first VRAM write after a VBlank
//! ([`Recorder::show`]). It is a read of the display area placed like any
//! other: after the layers that drew it, and before every later write into
//! it, which then goes to its layer or a later one. When such a write comes
//! in the same frame, the frame copies the area there ([`Op::Show`]) and
//! the executor shows the copy ([`Frame::shown`]); otherwise the area as
//! the frame leaves it is the field.
//!
//! ## Texture replacement
//! With a texture pack ([`Recorder::set_texture_pack`]), every upload and
//! every write goes to the [`Replacer`](super::replace::Replacer) in stream
//! order, and each textured primitive asks it for a replacement: its
//! vertices then name the replacement's slot ([`mode::SLOT_SHIFT`]) and,
//! in the window word, where its upload starts in the page. The images
//! decoded for the pass ride in [`Frame::replacements`]. Movie frames the
//! host hands over ([`Recorder::movie_frame`], with
//! [`Recorder::set_movie_images`]) take the same path, with or without a
//! texture pack.
//!
//! ## Effects frames
//! With effects configured ([`Recorder::set_effects`], [`super::fx`]) and on,
//! the host hands every DMA list to [`Recorder::list_begin`] before the
//! device walks it, with machine RAM. A list that walks the flight ordering
//! table in flight arms the frame's markers ([`super::fx::walk`]); each fires
//! just before the first GP0 word after its table entry:
//! - **sky** (entry 4095) and **HUD** (entry 0) only change the [`Phase`];
//! - **world begin** (4094): the z target is cleared over the draw area (a
//!   [`Pipe::ZClear`] draw placed as a write of the area, so after the sky
//!   and before every world primitive), then [`Op::FxBegin`]. Every world
//!   primitive gets a draw order (1, 2, ...) and draws with
//!   [`Pipe::World`], writing its view z and order beside its colour (the
//!   two words per vertex of [`Frame::fx_vertices`]);
//! - **world end** (1): [`Op::FxPost`], placed as a read and write of the
//!   draw area (after every world primitive, `shadow` up to date), then the
//!   deferred primitives, then (when an effect asks for it,
//!   [`fx::RecordEffect::late`]) [`Op::FxLate`], placed the same way (after
//!   them, before the flare and the HUD).
//!
//! **Deferral.** A semi-transparent world primitive (polygon, rectangle,
//! line) or a VRAM copy into the world's draw area is
//! not drawn when it comes: its words, addresses, drawing state and draw
//! order are queued, after its state changes (a polygon's texture page) are
//! made. At world end each is drawn again through the normal path with that
//! state ([`Pipe::Deferred`]), in queue order: the PlayStation's order among
//! them, and the order test against the depth snapshot puts them under the
//! later opaque primitives exactly. What would break that is drawn early
//! ("a flush", [`Op::FxSnapshot`] first): a later command that reads on the
//! card what a queued one draws (blending, mask checks, 15-bit texels, copy
//! sources), one that writes where a queued one reads (its texels or
//! palette, in either VRAM), and uploads or captures over what a queued one
//! draws (they carry the software VRAM's pixels, which already hold it).
//! The software VRAM is not deferred, so indexed texels and palettes decode
//! from it as before.
use super::fx::{self, Class, Marker, Phase, Pipe, Route};
use super::replace::{MovieFrame, MovieImages, Replacer, Sample, TexturePack};
use super::tiles::{HEIGHT, Rect, TILE, Tiles, WIDTH};
use super::{Options, VertexPrecision};
use ac3_graphics::{Gpu, Vram};
use std::rc::Rc;

/// 32-bit words per vertex in [`Frame::vertices`]: position (2 f32), texture
/// coordinates (2 f32), colour (RGB bytes), mode, 16 flat words (`q0..q3`),
/// whose meaning depends on the mode's kind, then w (f32): the vertex's
/// depth for perspective-correct texturing ([`mode::PERSPECTIVE`]), else 1.
pub const VERTEX_WORDS: usize = 23;

/// The mode word of a vertex (shared with `draw.wgsl`).
pub mod mode {
    /// Bits 0..2: what the shader draws.
    pub const KIND_POLY: u32 = 0;
    pub const KIND_RECT: u32 = 1;
    pub const KIND_LINE: u32 = 2;
    /// GP0(02): a plain colour, no mask.
    pub const KIND_FILL: u32 = 3;
    /// The 1 x mirror's pixels, replicated (uploads, captures, re-seeding).
    pub const KIND_MIRROR: u32 = 4;
    /// GP0(80): VRAM to VRAM, from `shadow`.
    pub const KIND_COPY: u32 = 5;
    pub const TEXTURED: u32 = 1 << 3;
    pub const RAW: u32 = 1 << 4;
    pub const GOURAUD: u32 = 1 << 5;
    pub const SEMI: u32 = 1 << 6;
    /// Bits 7..8: the semi-transparency function (0 average .. 3 add a quarter).
    pub const ABR_SHIFT: u32 = 7;
    pub const DITHER: u32 = 1 << 9;
    pub const CHECK: u32 = 1 << 10;
    pub const SET: u32 = 1 << 11;
    /// 1 x: coverage and attributes by the software GPU's integer rules.
    pub const EXACT: u32 = 1 << 12;
    /// Single-pixel arithmetic (Gouraud-textured slow path).
    pub const SCALAR: u32 = 1 << 13;
    /// Gouraud-textured fast path: pixel pairs share the first one's shade.
    pub const SHARE: u32 = 1 << 14;
    /// Above 1 x: texture coordinates and colours interpolate with
    /// perspective, by the vertices' w (a textured polygon whose vertices
    /// all have a depth).
    pub const PERSPECTIVE: u32 = 1 << 15;
    /// Bits 16..31: a texture replacement's slot + 1 (0: none).
    pub const SLOT_SHIFT: u32 = 16;
}

/// One operation of a frame, in execution order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Draw `count` vertices (a triangle list) from vertex `first`, clipped
    /// to `scissor` (1 x pixels).
    Draw {
        first: u32,
        count: u32,
        scissor: Rect,
    },
    /// Copy these rectangles of `vram` into `shadow` (a render-pass break).
    Sync(Vec<Rect>),
    /// Write 1 x pixels into the mirror at `rect`: `data[offset..]`, rows
    /// `pitch` halfwords apart (`offset` and `pitch` are multiples of 128,
    /// the card's row alignment).
    Mirror {
        rect: Rect,
        offset: usize,
        pitch: usize,
    },
    /// Copy `rect` of `vram` (the display area) as the shown field
    /// ([`Recorder::show`]).
    Show(Rect),
    /// An effects frame's draw ([`Op::Draw`] with the effects pipeline
    /// `pipe`; its vertices' view z and draw order in
    /// [`Frame::fx_vertices`]).
    FxDraw {
        first: u32,
        count: u32,
        scissor: Rect,
        pipe: Pipe,
    },
    /// World begin: the effects' work over the world's draw area `Rect`,
    /// after the z target's clear (the far terrain).
    FxBegin(Rect),
    /// World end: snapshot the z target over the area, then the post pass.
    FxPost(Rect),
    /// After world end's deferred primitives, before the flare and the
    /// HUD: the effects' late step over the area (bloom, lens flares).
    FxLate(Rect),
    /// Snapshot the z target over the area (before deferred primitives
    /// drawn early).
    FxSnapshot(Rect),
}

/// What the screen shows after a frame ([`Recorder::show`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Shown {
    /// As after the frame before (no VBlank in the pass).
    #[default]
    Kept,
    /// The display area of `vram` as the frame leaves it.
    Live,
    /// The copy of the frame's [`Op::Show`].
    Copy,
}

/// A recorded main-loop pass.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub ops: Vec<Op>,
    /// [`VERTEX_WORDS`] words per vertex.
    pub vertices: Vec<u32>,
    /// Pixel data of the [`Op::Mirror`] writes.
    pub data: Vec<u16>,
    /// Texture replacements decoded for this pass, for the atlas before
    /// its draws; `replacements_reset`: every slot starts over first (the
    /// pack was reloaded).
    pub replacements: Vec<super::replace::Image>,
    pub replacements_reset: bool,
    /// What the screen shows after the frame.
    pub shown: Shown,
    /// With effects configured, [`fx::FX_VERTEX_WORDS`] words per vertex of
    /// `vertices` (its view z and draw order); empty when the frame has no
    /// effects draw.
    pub fx_vertices: Vec<u32>,
    /// The effects frame's scene and markers.
    pub fx: Option<Box<fx::FxFrame>>,
}

impl Frame {
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty() && self.replacements.is_empty() && !self.replacements_reset
    }
    pub fn vertex_count(&self) -> usize {
        self.vertices.len() / VERTEX_WORDS
    }
}

/// What the recorder has done since it was created.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Stats {
    pub frames: u64,
    pub triangles: u64,
    /// Triangles drawn with perspective-correct texturing.
    pub perspective_triangles: u64,
    pub lines: u64,
    pub rectangles: u64,
    pub fills: u64,
    pub copies: u64,
    pub uploads: u64,
    /// Layers (render passes) in all frames, and the most in one frame.
    pub layers: u64,
    pub most_layers: u64,
    /// `shadow` updates (between layers) and their rectangles.
    pub syncs: u64,
    pub sync_rects: u64,
    /// Mirror writes from the software VRAM and their pixels (uploads not
    /// counted).
    pub refreshes: u64,
    pub refreshed_pixels: u64,
    /// Regions taken from the software VRAM after a command the card does
    /// not reproduce (overlapping VRAM copies).
    pub captures: u64,
    /// Shown fields copied before a later write into them ([`Op::Show`]).
    pub shows: u64,
    pub draws: u64,
    pub vertices: u64,
    pub reseeds: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Area {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// The software GPU's drawing state (`ac3_graphics::gpu::DrawState`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct State {
    area: Area,
    offset: [i32; 2],
    tex_base: [i32; 2],
    /// 0: 4-bit, 1: 8-bit, 2: 15-bit.
    depth: u32,
    abr: u32,
    dither: bool,
    status_dither: bool,
    window_mask: [i32; 2],
    window_offset: [i32; 2],
    set_mask: bool,
    check_mask: bool,
}

struct Upload {
    rect: Rect,
    pixels: Vec<u16>,
}

fn sign_extend_11(value: u32) -> i32 {
    ((value << 21) as i32) >> 21
}

fn over_limit(a: (i32, i32), b: (i32, i32)) -> bool {
    (a.0 - b.0).abs() >= 1024 || (a.1 - b.1).abs() >= 512
}

fn terminator(word: u32) -> bool {
    word & 0xf000_f000 == 0x5000_5000
}

/// 24-bit command colour to RGB555 without a mask bit.
pub fn command_color(rgb: u32) -> u32 {
    ((rgb >> 3) & 0x1f) | ((rgb >> 6) & 0x3e0) | ((rgb >> 9) & 0x7c00)
}

fn pack_xy(x: i32, y: i32) -> u32 {
    u32::from(x as u16) | (u32::from(y as u16) << 16)
}

fn align(value: usize, to: usize) -> usize {
    value.div_ceil(to) * to
}

/// The smallest rectangle holding all of `rects` (empty for none).
fn union(rects: &[Rect]) -> Rect {
    let mut rects = rects.iter().filter(|r| !r.is_empty());
    let Some(first) = rects.next() else {
        return Rect::default();
    };
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.right(), first.bottom());
    for r in rects {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.right());
        y1 = y1.max(r.bottom());
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

// ---- layering ---------------------------------------------------------------------------------

/// One access to a resource in the frame being recorded.
#[derive(Clone, Copy, Debug)]
struct Access {
    rect: Rect,
    layer: usize,
    write: bool,
}

/// Which accesses a query looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Any,
    Writes,
    Reads,
}

/// Most accesses kept per tile; more are merged into two covering the tile
/// (the latest write and the latest read).
const TILE_ACCESSES: usize = 24;

/// The accesses to one resource (the card's VRAM, or the mirror) in the
/// frame being recorded, by 16 x 16 tile.
struct Accesses {
    tiles: Vec<Vec<Access>>,
}

impl Accesses {
    const COLUMNS: usize = (WIDTH / TILE) as usize;

    fn new() -> Self {
        Self {
            tiles: vec![Vec::new(); Self::COLUMNS * (HEIGHT / TILE) as usize],
        }
    }

    fn clear(&mut self) {
        for list in &mut self.tiles {
            list.clear();
        }
    }

    /// The tiles `rect` touches, with their rectangles.
    fn tiles_of(rect: &Rect) -> impl Iterator<Item = (usize, Rect)> + use<> {
        let rect = rect.clipped();
        let (columns, rows) = if rect.is_empty() {
            (0..0, 0..0)
        } else {
            (
                rect.x / TILE..(rect.right() - 1) / TILE + 1,
                rect.y / TILE..(rect.bottom() - 1) / TILE + 1,
            )
        };
        rows.flat_map(move |row| {
            columns.clone().map(move |column| {
                (
                    row as usize * Self::COLUMNS + column as usize,
                    Rect::new(column * TILE, row * TILE, TILE, TILE),
                )
            })
        })
    }

    /// The highest layer of an earlier access of `kind` overlapping `rect`.
    fn latest(&self, rect: &Rect, kind: Kind) -> Option<usize> {
        let mut latest = None;
        for (index, _) in Self::tiles_of(rect) {
            for access in &self.tiles[index] {
                let wanted = match kind {
                    Kind::Any => true,
                    Kind::Writes => access.write,
                    Kind::Reads => !access.write,
                };
                if wanted && access.rect.overlaps(rect) {
                    latest = latest.max(Some(access.layer));
                }
            }
        }
        latest
    }

    /// Note an access. A write covering a whole tile replaces the tile's
    /// earlier accesses: its layer is at least theirs.
    fn add(&mut self, rect: &Rect, layer: usize, write: bool) {
        for (index, tile) in Self::tiles_of(rect) {
            let list = &mut self.tiles[index];
            let part = rect.intersect(&tile);
            if write && part == tile {
                list.clear();
            }
            list.push(Access {
                rect: part,
                layer,
                write,
            });
            if list.len() > TILE_ACCESSES {
                let latest = |write: bool| {
                    list.iter()
                        .filter(|a| a.write == write)
                        .map(|a| a.layer)
                        .max()
                };
                let merged: Vec<Access> = [true, false]
                    .into_iter()
                    .filter_map(|write| {
                        latest(write).map(|layer| Access {
                            rect: tile,
                            layer,
                            write,
                        })
                    })
                    .collect();
                *list = merged;
            }
        }
    }
}

/// One draw or effects step of a layer, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// First vertex (in the layer's `vertices`), count and scissor; the
    /// effects pipeline (`None`: the renderer's own).
    Draw {
        first: u32,
        count: u32,
        scissor: Rect,
        pipe: Option<Pipe>,
    },
    /// [`Op::FxBegin`], [`Op::FxPost`], [`Op::FxLate`] or [`Op::FxSnapshot`].
    Fx(FxStep),
}

/// The effects steps of [`Item::Fx`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FxStep {
    Begin(Rect),
    Post(Rect),
    Late(Rect),
    Snapshot(Rect),
}

/// One render pass of a frame.
#[derive(Default)]
struct Layer {
    /// Mirror writes before the pass.
    mirror: Vec<Op>,
    vertices: Vec<u32>,
    /// With effects configured: [`fx::FX_VERTEX_WORDS`] per vertex.
    fx: Vec<u32>,
    /// Its draws and effects steps.
    items: Vec<Item>,
    /// The card VRAM tiles the pass draws.
    written: Tiles,
    /// Whether a primitive of the pass reads `shadow`.
    reads: bool,
}

/// A triangle vertex: integer position (offset applied), its drawing
/// position (sub-pixel with a precision source above 1 x) and depth (the
/// source's z; 0 without one), texture coordinates and colour.
#[derive(Clone, Copy, Debug, Default)]
struct Point {
    x: i32,
    y: i32,
    fx: f32,
    fy: f32,
    z: f32,
    u: i32,
    v: i32,
    rgb: u32,
}

/// The software GPU's triangle set-up (`draw_triangle`): vertices sorted by
/// Y, the long edge's side and the horizontal gradients.
#[derive(Clone, Copy, Debug)]
struct Setup {
    p: [Point; 3],
    long_left: bool,
    du: i32,
    dv: i32,
    dc: [i32; 3],
}

fn shl10idiv(x: i32, y: i32) -> i32 {
    ((i64::from(x) << 10) / i64::from(y)) as i32
}

impl Setup {
    fn new(vertices: [Point; 3], interpolated: bool) -> Option<Self> {
        let mut p = vertices;
        if p[0].y > p[1].y {
            p.swap(0, 1);
        }
        if p[0].y > p[2].y {
            p.swap(0, 2);
        }
        if p[1].y > p[2].y {
            p.swap(1, 2);
        }
        let height = p[2].y - p[0].y;
        if height == 0 {
            return None;
        }
        let x = |i: usize| p[i].x << 16;
        let temp = ((p[1].y - p[0].y) << 16) / height;
        let mut longest = temp
            .wrapping_mul((x(2) - x(0)) >> 16)
            .wrapping_add(x(0) - x(1));
        if longest == 0 {
            return None;
        }
        let long_left = longest < 0;
        if interpolated {
            longest = if longest < 0 {
                longest.min(-0x1000)
            } else {
                longest.max(0x1000)
            };
        }
        let gradient = |a: [i32; 3]| {
            shl10idiv(
                temp.wrapping_mul((a[2].wrapping_sub(a[0])) >> 10)
                    .wrapping_add((a[0].wrapping_sub(a[1])) << 6),
                longest,
            )
        };
        let channel = |i: usize, c: usize| (((p[i].rgb >> (c * 8)) & 0xff) << 16) as i32;
        Some(Self {
            p,
            long_left,
            du: gradient([p[0].u << 16, p[1].u << 16, p[2].u << 16]),
            dv: gradient([p[0].v << 16, p[1].v << 16, p[2].v << 16]),
            dc: std::array::from_fn(|c| gradient([channel(0, c), channel(1, c), channel(2, c)])),
        })
    }
}

/// Whether a quad's triangles (1, 3, 2) and (0, 1, 2) are apart: vertices
/// 0 and 3 strictly on opposite sides of the shared edge 1-2 (the fill rule
/// keeps the edge's pixels to one of them).
fn halves_disjoint(corners: &[Point; 4]) -> bool {
    let side = |p: &Point| {
        let (ex, ey) = (
            f64::from(corners[2].fx - corners[1].fx),
            f64::from(corners[2].fy - corners[1].fy),
        );
        let (px, py) = (
            f64::from(p.fx - corners[1].fx),
            f64::from(p.fy - corners[1].fy),
        );
        ex * py - ey * px
    };
    side(&corners[0]) * side(&corners[3]) < 0.0
}

/// Whether a textured polygon with these vertices (all of a triangle, or
/// all four of a quad) is drawn with perspective: every vertex has a depth
/// above 0 (a finite one), and not all are equal (equal depths are the
/// affine mapping exactly, so the exact affine path draws them).
fn perspective(corners: &[Point]) -> bool {
    let finite = |p: &Point| p.z > 0.0 && p.z.is_finite();
    corners.iter().all(finite)
        && corners
            .iter()
            .any(|p| p.z.to_bits() != corners[0].z.to_bits())
}

/// A triangle ready to draw: its vertices, set-up and the pixels it may
/// reach.
struct Triangle {
    points: [Point; 3],
    setup: Setup,
    dest: Rect,
}

/// What a textured primitive samples.
#[derive(Clone, Copy, Debug)]
struct Texture {
    depth: u32,
    base: [i32; 2],
    /// Palette position in VRAM pixels.
    clut: [i32; 2],
    /// Texture coordinates used, inclusive, before the window.
    u: (i32, i32),
    v: (i32, i32),
}

/// Whether the palette word `clut` names a palette of the JP terrain block
/// (320..335, 256..319), whose palettes the fog `800943A8` rewrites.
fn terrain_palette(clut: u32) -> bool {
    let (x, y) = ((clut & 0x3f) * 16, (clut >> 6) & 0x1ff);
    x == 320 && (256..320).contains(&y)
}

/// The 8-bit coordinate ranges (inclusive) reachable through a texture
/// window from the coordinates `lo..=hi`.
pub(super) fn axis_ranges(
    lo: i32,
    hi: i32,
    window_mask: i32,
    window_offset: i32,
) -> Vec<(i32, i32)> {
    if window_mask != 0 {
        let keep = i32::from(!(window_mask as u8));
        return vec![(window_offset, window_offset | keep)];
    }
    if hi - lo >= 255 {
        return vec![(0, 255)];
    }
    let (lo, hi) = (lo & 255, hi & 255);
    if lo <= hi {
        vec![(lo, hi)]
    } else {
        vec![(0, hi), (lo, 255)]
    }
}

/// VRAM rows `y0..=y1` of halfwords `x0..=x1` in linear addressing: a span
/// past the right edge continues on the next row.
fn linear_rects(x0: i32, x1: i32, y0: i32, y1: i32, out: &mut Vec<Rect>) {
    let (y0, y1) = (y0.min(HEIGHT - 1), y1.min(HEIGHT - 1));
    if x0 < WIDTH {
        out.push(Rect::inclusive(x0, y0, x1.min(WIDTH - 1), y1));
    }
    if x1 >= WIDTH {
        let rows = Rect::inclusive(
            (x0 - WIDTH).max(0),
            (y0 + 1).min(HEIGHT - 1),
            x1 - WIDTH,
            (y1 + 1).min(HEIGHT - 1),
        );
        out.push(rows);
    }
}

impl Texture {
    /// The VRAM rectangles of the texels and of the palette.
    fn rects(&self, window_mask: [i32; 2], window_offset: [i32; 2]) -> (Vec<Rect>, Vec<Rect>) {
        let mut texels = Vec::new();
        for &(u0, u1) in &axis_ranges(self.u.0, self.u.1, window_mask[0], window_offset[0]) {
            for &(v0, v1) in &axis_ranges(self.v.0, self.v.1, window_mask[1], window_offset[1]) {
                let shift = 2 - self.depth.min(2) as i32;
                linear_rects(
                    self.base[0] + (u0 >> shift),
                    self.base[0] + (u1 >> shift),
                    self.base[1] + v0,
                    self.base[1] + v1,
                    &mut texels,
                );
            }
        }
        let mut clut = Vec::new();
        if self.depth < 2 {
            let entries = if self.depth == 0 { 16 } else { 256 };
            let [x, y] = self.clut;
            linear_rects(x, x + entries - 1, y, y, &mut clut);
        }
        (texels, clut)
    }
}

/// How a primitive writes.
#[derive(Clone, Copy, Debug, Default)]
struct Style {
    gouraud: bool,
    textured: bool,
    semi: bool,
    raw: bool,
    dither: bool,
    /// Perspective-correct texturing (textured polygons above 1 x).
    perspective: bool,
}

/// A shown field not yet in a frame ([`Recorder::show`]).
#[derive(Clone, Copy, Debug)]
struct Show {
    /// The display area.
    rect: Rect,
    /// The layer before whose draws it is copied.
    layer: usize,
    /// A later write went into the area, so the frame copies it.
    overwritten: bool,
}

/// A deferred primitive ([`Recorder`], "Effects frames"): its command, the
/// drawing state before it, and its draw order.
struct Deferred {
    words: Vec<u32>,
    addresses: Vec<u32>,
    state: State,
    order: u32,
    /// The ordering-table slot it hung off.
    slot: Option<u16>,
}

/// The effects frame being recorded.
struct FxFrameState {
    scene: fx::Scene,
    phase: Phase,
    /// The draw area at world begin.
    world_area: Rect,
    /// The last draw order given.
    order: u32,
    classes: Vec<u8>,
    slots: Vec<u16>,
    markers: Vec<(Marker, u32)>,
    /// The sky slot's drawing commands.
    sky: Vec<fx::SkyCommand>,
    queue: Vec<Deferred>,
    /// Drawing the queue (no deferral, no flushes, orders from the queue).
    replaying: bool,
    /// The world-begin and post steps were placed.
    begun: bool,
    ended: bool,
}

/// The recorder's effects ([`Recorder::set_effects`]).
struct Effects {
    config: fx::Config,
    effects: Vec<Box<dyn fx::RecordEffect>>,
    stats: fx::FxStats,
    /// The markers of the DMA list being walked, and the next to fire.
    fires: Vec<fx::Fire>,
    next: usize,
    /// Where the walk's ordering-table slot changes (first GP0 word, slot),
    /// the next change, and the slot of the words being recorded (None:
    /// not in the flight table); while the queue is drawn, its slot.
    slots: Vec<(u32, u16)>,
    slot_next: usize,
    slot: Option<u16>,
    replay_slot: Option<Option<u16>>,
    frame: Option<FxFrameState>,
    /// What the queued primitives draw, and what they read (on the card or
    /// from the software VRAM); apart, as a write covering a tile drops the
    /// reads before it in one set.
    queued_dests: Accesses,
    queued_reads: Accesses,
}

impl Effects {
    /// The address of the GP0 word before which the next marker fires.
    fn due(&self) -> Option<u32> {
        self.fires.get(self.next).and_then(|fire| fire.word)
    }
    fn world(&self) -> Option<&FxFrameState> {
        self.frame
            .as_ref()
            .filter(|frame| frame.phase == Phase::World && !frame.replaying)
    }
}

/// The GPU renderer's recorder; see the module documentation.
pub struct Recorder {
    options: Options,
    exact: bool,
    /// Optional sub-pixel vertex positions, used above 1 x.
    pub precision: Option<Rc<dyn VertexPrecision>>,
    /// Perspective-correct texturing above 1 x, from the depths of
    /// [`precision`](Self::precision) (`[display] perspective`).
    pub perspective: bool,
    state: State,
    fifo: Vec<u32>,
    addresses: Vec<u32>,
    upload: Option<Upload>,
    /// Where the card's 1 x mirror may differ from the software VRAM.
    mirror_dirty: Tiles,
    /// The frame's accesses to the card's VRAM and to the mirror.
    vram_access: Accesses,
    mirror_access: Accesses,
    /// The frame's layers, and the one taking vertices now.
    layers: Vec<Layer>,
    current: usize,
    /// Tiles the last layer of earlier frames drew, not yet in `shadow`.
    pending: Tiles,
    /// Bring all of `shadow` up to date at the end of the frame (after a
    /// re-seed).
    seal: bool,
    /// The frame's mirror pixel data.
    data: Vec<u16>,
    /// Regions to take from the software VRAM once the command that
    /// changed them has run.
    captures: Vec<Rect>,
    /// The shown field not yet in a frame ([`Self::show`]).
    show: Option<Show>,
    /// A VBlank (or a re-seed) since the last frame: the field is the live
    /// display area unless a [`Self::show`] follows.
    live: bool,
    /// Texture replacement, with a texture pack.
    replace: Option<Box<Replacer>>,
    /// Effects, when configured ([`Self::set_effects`]).
    fx: Option<Box<Effects>>,
    /// The effects' palette redirect (the atmosphere's
    /// `clut_word_redirect`), applied to every indexed texture of an
    /// effects frame with the phase of the ordering-table slot it hangs off
    /// (`Phase::Other` outside the flight table). `None`: none.
    pub clut_redirect: Option<fx::ClutRedirect>,
    /// The draw order of the primitive being drawn, and the effects
    /// pipeline its draws take (`None`: the renderer's own).
    order: u32,
    pipe: Option<Pipe>,
    stats: Stats,
}

impl Recorder {
    /// A recorder whose card VRAM starts as `gpu`'s (a [`reseed`](Self::reseed)).
    pub fn new(options: Options, gpu: &Gpu) -> Self {
        let scale = options.scale.clamp(1, super::MAX_SCALE);
        let mut recorder = Self {
            options: Options { scale, ..options },
            exact: scale == 1,
            precision: None,
            perspective: false,
            state: State::default(),
            fifo: Vec::new(),
            addresses: Vec::new(),
            upload: None,
            mirror_dirty: Tiles::full(),
            vram_access: Accesses::new(),
            mirror_access: Accesses::new(),
            layers: Vec::new(),
            current: 0,
            pending: Tiles::empty(),
            seal: false,
            data: Vec::new(),
            captures: Vec::new(),
            show: None,
            live: false,
            replace: None,
            fx: None,
            clut_redirect: None,
            order: 0,
            pipe: None,
            stats: Stats::default(),
        };
        recorder.reseed(gpu);
        recorder
    }

    // ---- effects --------------------------------------------------------------------------

    /// Configure effects (`None`: none, the default). Only above 1 x; at 1 x
    /// the recorder stays exact and this does nothing.
    pub fn set_effects(&mut self, config: Option<fx::Config>) {
        // Vertices recorded so far get their (empty) effects words.
        for layer in &mut self.layers {
            layer.fx = vec![0; layer.vertices.len() / VERTEX_WORDS * fx::FX_VERTEX_WORDS];
        }
        self.fx = config.filter(|_| !self.exact).map(|config| {
            Box::new(Effects {
                config,
                effects: Vec::new(),
                stats: fx::FxStats::default(),
                fires: Vec::new(),
                next: 0,
                slots: Vec::new(),
                slot_next: 0,
                slot: None,
                replay_slot: None,
                frame: None,
                queued_dests: Accesses::new(),
                queued_reads: Accesses::new(),
            })
        });
    }

    /// The effects' configuration, when configured (F6 flips `enabled`;
    /// it applies from the next DMA list).
    pub fn effects_config_mut(&mut self) -> Option<&mut fx::Config> {
        self.fx.as_deref_mut().map(|fx| &mut fx.config)
    }
    pub fn effects_config(&self) -> Option<fx::Config> {
        self.fx.as_deref().map(|fx| fx.config)
    }

    /// Register the recorder side of an effect (needs [`Self::set_effects`]).
    pub fn add_effect(&mut self, effect: Box<dyn fx::RecordEffect>) {
        if let Some(fx) = self.fx.as_deref_mut() {
            fx.effects.push(effect);
        }
    }

    pub fn effects_stats(&self) -> Option<&fx::FxStats> {
        self.fx.as_deref().map(|fx| &fx.stats)
    }

    /// A DMA list `start` is about to be walked (the host, at each GP0 list
    /// hand-over: DrawOTag, PutDrawEnv, MoveImage), with machine RAM. With
    /// effects on, in flight, above 1 x and not on a 24-bit display
    /// (`movie`), a list through the flight ordering table arms the frame's
    /// markers.
    pub fn list_begin(&mut self, ram: &[u8], start: u32, vram: &Vram, movie: bool) {
        let Some(fx) = self.fx.as_deref_mut() else {
            return;
        };
        fx.fires.clear();
        fx.next = 0;
        fx.slots.clear();
        fx.slot_next = 0;
        fx.slot = None;
        if !fx.config.enabled || fx.frame.is_some() {
            return;
        }
        fx.stats.lists += 1;
        let Some(scene) = fx::Scene::read(ram, fx.config.x_scale) else {
            fx.stats.not_flight += 1;
            return;
        };
        let Some(schedule) = fx::walk::schedule(ram, start, &scene.ots) else {
            return;
        };
        if movie {
            fx.stats.movie += 1;
            return;
        }
        fx.stats.frames += 1;
        fx.fires = schedule.fires;
        fx.slots = schedule.slots;
        for effect in &mut fx.effects {
            effect.kick(&scene, ram, vram);
        }
        fx.frame = Some(FxFrameState {
            scene,
            phase: Phase::Outside,
            world_area: Rect::default(),
            order: 0,
            classes: Vec::new(),
            slots: Vec::new(),
            markers: Vec::new(),
            sky: Vec::new(),
            queue: Vec::new(),
            replaying: false,
            begun: false,
            ended: false,
        });
    }

    /// The DMA list walked: the markers no packet followed fire now.
    pub fn list_end(&mut self, vram: &Vram) {
        let Some(fx) = self.fx.as_deref() else {
            return;
        };
        if fx.next < fx.fires.len() {
            self.fire_markers(None, vram);
        }
        if let Some(fx) = self.fx.as_deref_mut() {
            fx.fires.clear();
            fx.next = 0;
            fx.slots.clear();
            fx.slot_next = 0;
            fx.slot = None;
        }
        // A world that began ends with its list.
        if self
            .fx
            .as_deref()
            .and_then(|fx| fx.frame.as_ref())
            .is_some_and(|frame| frame.begun && !frame.ended)
        {
            self.world_end(vram);
        }
        // What follows is outside the flight table.
        if let Some(frame) = self.fx_frame_mut() {
            frame.phase = Phase::Outside;
        }
    }

    /// Fire the markers due before the word at `address` (`None`: all that
    /// are left, at the list's end).
    fn fire_markers(&mut self, address: Option<u32>, vram: &Vram) {
        loop {
            let Some(fx) = self.fx.as_deref_mut() else {
                return;
            };
            let Some(fire) = fx.fires.get(fx.next).copied() else {
                return;
            };
            if address.is_some() && fire.word != address {
                return;
            }
            fx.next += 1;
            fx.stats.count_marker(fire.marker);
            let Some(frame) = fx.frame.as_mut() else {
                continue;
            };
            frame.markers.push((fire.marker, fire.node));
            let phase = match fire.marker {
                Marker::Sky => Phase::Sky,
                Marker::WorldBegin => Phase::World,
                Marker::WorldEnd => Phase::Flare,
                Marker::Hud => Phase::Hud,
            };
            let scene = frame.scene;
            for effect in &mut fx.effects {
                effect.phase(phase, &scene);
            }
            match fire.marker {
                Marker::WorldBegin => self.world_begin(),
                Marker::WorldEnd => self.world_end(vram),
                _ => {
                    if let Some(frame) = self.fx_frame_mut() {
                        frame.phase = phase;
                    }
                }
            }
        }
    }

    fn fx_frame_mut(&mut self) -> Option<&mut FxFrameState> {
        self.fx.as_deref_mut()?.frame.as_mut()
    }

    /// World begin: clear the z target over the draw area to the sky (as a
    /// write of the area: after the sky, before the world), then the
    /// effects' world-begin step.
    fn world_begin(&mut self) {
        let area = self.area_rect();
        let Some(frame) = self.fx_frame_mut() else {
            return;
        };
        if frame.begun || area.is_empty() {
            return;
        }
        frame.begun = true;
        frame.world_area = area;
        self.place(&[area], &[], &[]);
        self.pipe = Some(Pipe::ZClear);
        self.order = 0;
        self.quad(area, [0.0; 4], 0, mode::KIND_FILL, &[0; 16], area);
        self.pipe = None;
        self.layers[self.current]
            .items
            .push(Item::Fx(FxStep::Begin(area)));
        if let Some(frame) = self.fx_frame_mut() {
            frame.phase = Phase::World;
        }
    }

    /// World end: the post (after every world primitive, as a read and a
    /// write of the world's area), then the deferred primitives, then the
    /// late step when an effect asks for it (placed the same way: after
    /// them, before the flare and the HUD).
    fn world_end(&mut self, vram: &Vram) {
        let Some(frame) = self.fx_frame_mut() else {
            return;
        };
        frame.phase = Phase::Flare;
        if !frame.begun || frame.ended {
            return;
        }
        frame.ended = true;
        let area = frame.world_area;
        self.place(&[area], &[area], &[]);
        self.layers[self.current]
            .items
            .push(Item::Fx(FxStep::Post(area)));
        if let Some(fx) = self.fx.as_deref_mut() {
            fx.stats.posts += 1;
        }
        self.replay(false, vram);
        // Only when asked: the step's place changes the layering after it
        // (above 1x a line's ends then fall differently, docs/EFFECTS.md
        // §5).
        if self
            .fx
            .as_deref()
            .is_some_and(|fx| fx.effects.iter().any(|effect| effect.late()))
        {
            self.place(&[area], &[area], &[]);
            self.layers[self.current]
                .items
                .push(Item::Fx(FxStep::Late(area)));
        }
    }

    /// Draw the queued primitives (`early`: before world end, after a z
    /// snapshot), in queue order, with their drawing state and draw order.
    fn replay(&mut self, early: bool, vram: &Vram) {
        let Some(fx) = self.fx.as_deref_mut() else {
            return;
        };
        let Some(frame) = fx.frame.as_mut() else {
            return;
        };
        if frame.queue.is_empty() {
            return;
        }
        let queue = std::mem::take(&mut frame.queue);
        frame.replaying = true;
        let area = frame.world_area;
        fx.queued_dests.clear();
        fx.queued_reads.clear();
        fx.stats.replayed += queue.len() as u64;
        if early {
            fx.stats.flushes += 1;
            fx.stats.replayed_early += queue.len() as u64;
            self.place(&[], &[area], &[]);
            self.layers[self.current]
                .items
                .push(Item::Fx(FxStep::Snapshot(area)));
        }
        let live = self.state;
        for item in &queue {
            self.state = item.state;
            self.order = item.order;
            if let Some(fx) = self.fx.as_deref_mut() {
                fx.replay_slot = Some(item.slot);
            }
            self.execute(&item.words, &item.addresses, vram);
        }
        self.state = live;
        self.order = 0;
        if let Some(fx) = self.fx.as_deref_mut() {
            fx.replay_slot = None;
        }
        if let Some(frame) = self.fx_frame_mut() {
            frame.replaying = false;
        }
    }

    /// The effects frame recorded, for [`Frame::fx`] (the world ended
    /// first if its list did not end it).
    fn take_fx(&mut self, vram: &Vram) -> Option<Box<fx::FxFrame>> {
        if self
            .fx
            .as_deref()
            .and_then(|fx| fx.frame.as_ref())
            .is_some_and(|frame| frame.begun && !frame.ended)
        {
            self.world_end(vram);
        }
        let fx = self.fx.as_deref_mut()?;
        let Some(frame) = fx.frame.take() else {
            for effect in &mut fx.effects {
                effect.idle();
            }
            return None;
        };
        fx.fires.clear();
        fx.next = 0;
        fx.queued_dests.clear();
        fx.queued_reads.clear();
        let data = fx
            .effects
            .iter_mut()
            .filter_map(|effect| effect.take().map(|data| (effect.name(), data)))
            .collect();
        frame.begun.then(|| {
            Box::new(fx::FxFrame {
                scene: frame.scene,
                world_area: frame.world_area,
                markers: frame.markers,
                debug: fx.config.debug,
                classes: frame.classes,
                slots: frame.slots,
                sky: frame.sky,
                data,
            })
        })
    }

    /// Whether the recorder is in an effects frame's world (not replaying).
    fn in_world(&self) -> bool {
        self.fx.as_deref().is_some_and(|fx| fx.world().is_some())
    }

    /// The effects' decision about a drawing command of an effects frame
    /// (`Route::Draw` outside one), and whether a drawn one is deferred
    /// (`deferrable`: semi-transparent, or a copy; into the world's area).
    fn route(
        &mut self,
        words: &[u32],
        addresses: &[u32],
        semi: bool,
        texture: Option<fx::TextureUse>,
        deferrable: bool,
    ) -> (Route, bool) {
        let area = self.area_rect();
        let state = self.state;
        let Some(fx) = self.fx.as_deref_mut() else {
            return (Route::Draw, false);
        };
        let Some(frame) = fx.frame.as_ref() else {
            return (Route::Draw, false);
        };
        if frame.replaying {
            return (Route::Draw, false);
        }
        let primitive = fx::Primitive {
            phase: frame.phase,
            command: (words[0] >> 24) as u8,
            words,
            addresses,
            offset: state.offset,
            area,
            semi,
            texture,
        };
        if frame.phase == Phase::Sky
            && let Some(frame) = fx.frame.as_mut()
        {
            frame.sky.push(fx::SkyCommand {
                words: words.to_vec(),
                offset: state.offset,
            });
        }
        let Some(frame) = fx.frame.as_ref() else {
            return (Route::Draw, false);
        };
        let world = frame.phase == Phase::World && area == frame.world_area;
        let mut route = Route::Draw;
        for effect in &mut fx.effects {
            if effect.primitive(&primitive) == Route::Drop {
                route = Route::Drop;
            }
        }
        if route == Route::Drop {
            fx.stats.dropped += 1;
            return (route, false);
        }
        if !(world && deferrable) {
            return (route, false);
        }
        // Mask bits defer too: a later mask check reads what it draws (an
        // early replay), and the replay draws with its own mask state.
        if state.set_mask || state.check_mask {
            fx.stats.deferred_masked += 1;
        }
        (route, true)
    }

    /// Queue a deferred command of `class` (its state changes made): it
    /// draws `dests` (a bound, 1 x pixels) and reads `reads` (on the card or
    /// from the software VRAM). When it draws where a queued one reads, the
    /// queue is drawn first (the software VRAM, which the queued one's
    /// palettes and indexed texels decode from, now has its pixels).
    #[allow(clippy::too_many_arguments)]
    fn defer(
        &mut self,
        words: &[u32],
        addresses: &[u32],
        state: State,
        dests: Rect,
        reads: &[Rect],
        class: Class,
        vram: &Vram,
    ) {
        if self
            .fx
            .as_deref()
            .is_some_and(|fx| fx.queued_reads.latest(&dests, Kind::Any).is_some())
        {
            self.replay(true, vram);
        }
        let order = self.next_order(class);
        let Some(fx) = self.fx.as_deref_mut() else {
            return;
        };
        let slot = fx.slot;
        fx.stats.deferred += 1;
        fx.queued_dests.add(&dests, 0, true);
        for read in reads {
            fx.queued_reads.add(read, 0, false);
        }
        if let Some(frame) = fx.frame.as_mut() {
            frame.queue.push(Deferred {
                words: words.to_vec(),
                addresses: addresses.to_vec(),
                state,
                order,
                slot,
            });
        }
        // The software VRAM has its pixels now: indexed reads of them
        // decode from it.
        self.mirror_dirty.mark(&dests);
    }

    /// The next draw order of a world primitive of `class`.
    fn next_order(&mut self, class: Class) -> u32 {
        let slot = self.slot().unwrap_or(0);
        let Some(frame) = self.fx_frame_mut() else {
            return 0;
        };
        frame.order += 1;
        frame.classes.push(class as u8);
        frame.slots.push(slot);
        frame.order
    }

    /// Before a world primitive that writes `dests` and reads `reads` on
    /// the card: [`Self::fx_flush`], then its draw order.
    fn fx_before(
        &mut self,
        dests: &[Rect],
        reads: &[Rect],
        software: bool,
        class: Class,
        vram: &Vram,
    ) {
        if self.fx.is_none() {
            return;
        }
        self.fx_flush(dests, reads, software, vram);
        self.order = if self.in_world() {
            self.next_order(class)
        } else if self
            .fx
            .as_deref()
            .and_then(|fx| fx.frame.as_ref())
            .is_some_and(|frame| frame.replaying)
        {
            self.order
        } else {
            0
        };
        if self.in_world()
            && let Some(fx) = self.fx.as_deref_mut()
        {
            fx.stats.world_primitives += 1;
        }
    }

    /// Draw the queue now when a world command that writes `dests` and
    /// reads `reads` on the card would see a queued primitive's change
    /// missing (it reads what one draws), or would change what one reads
    /// (its texels or palette, in either VRAM). `software`: the command
    /// writes the software VRAM's own pixels (uploads, captures), which
    /// already hold the queued primitives: also where one draws. Called
    /// before the software GPU runs the command when that writes VRAM by
    /// itself (uploads, captured copies), so the queue decodes the
    /// palettes it had.
    fn fx_flush(&mut self, dests: &[Rect], reads: &[Rect], software: bool, vram: &Vram) {
        let flush = match self.fx.as_deref() {
            Some(fx) if fx.world().is_some_and(|frame| !frame.queue.is_empty()) => {
                let drawn = |rect: &Rect| fx.queued_dests.latest(rect, Kind::Any).is_some();
                let read = |rect: &Rect| fx.queued_reads.latest(rect, Kind::Any).is_some();
                reads.iter().any(drawn)
                    || dests.iter().any(read)
                    || (software && dests.iter().any(drawn))
            }
            _ => false,
        };
        if flush {
            self.replay(true, vram);
        }
    }

    /// The effects pipeline of the draws being recorded.
    fn draw_pipe(&self) -> Option<Pipe> {
        if self.pipe.is_some() {
            return self.pipe;
        }
        let frame = self.fx.as_deref()?.frame.as_ref()?;
        if frame.replaying {
            Some(Pipe::Deferred)
        } else if frame.phase == Phase::World {
            Some(Pipe::World)
        } else {
            None
        }
    }

    /// The ordering-table slot of the words being recorded (the queued
    /// command's own while the queue is drawn); `None` outside the flight
    /// table of an effects frame.
    pub fn slot(&self) -> Option<u16> {
        let fx = self.fx.as_deref()?;
        fx.replay_slot.unwrap_or(fx.slot)
    }

    /// An indexed texture's palette half-word `clut` as the effects'
    /// redirect ([`Self::clut_redirect`]) has it, in the phase of the
    /// current slot; unchanged without a redirect or for 15-bit texels. Once
    /// per primitive, before its texture's use, replacement and words.
    fn effective_clut(&mut self, clut: u32) -> u32 {
        let Some(redirect) = self.clut_redirect else {
            return clut;
        };
        let entries = match self.state.depth {
            0 => 16,
            1 => 256,
            _ => return clut,
        };
        let phase = crate::effects::atmosphere::phase_of_slot(self.slot());
        match redirect(clut as u16, entries, phase) {
            Some(word) => {
                if let Some(fx) = self.fx.as_deref_mut() {
                    fx.stats.redirected += 1;
                }
                u32::from(word)
            }
            None => clut,
        }
    }

    pub fn options(&self) -> Options {
        self.options
    }
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Draw `pack`'s textures instead of the game's from now on (`None`:
    /// none); textures already in `vram` are found by the uploads seen.
    /// Movie frames ([`Self::set_movie_images`]) stay on.
    pub fn set_texture_pack(&mut self, pack: Option<TexturePack>, vram: &Vram) {
        let movies = self
            .replace
            .as_deref_mut()
            .and_then(Replacer::take_movie_images);
        self.replace = match (pack, movies) {
            (None, None) => None,
            (pack, movies) => {
                let mut replacer = Box::new(Replacer::new(pack.unwrap_or_else(TexturePack::none)));
                replacer.set_movie_images(movies);
                replacer.reseed(vram);
                Some(replacer)
            }
        };
    }

    /// Draw movie frames ([`Self::movie_frame`]) from `images` from now on
    /// (`None`: never), with or without a texture pack.
    pub fn set_movie_images(&mut self, images: Option<Box<dyn MovieImages>>, vram: &Vram) {
        match self.replace.as_deref_mut() {
            Some(replacer) => replacer.set_movie_images(images),
            None if images.is_some() => {
                let mut replacer = Box::new(Replacer::new(TexturePack::none()));
                replacer.set_movie_images(images);
                replacer.reseed(vram);
                self.replace = Some(replacer);
            }
            None => {}
        }
    }

    /// A movie frame is complete in VRAM (after the upload of its last
    /// slice): its texels are drawn from the movie pack's image while they
    /// are its own ([`Replacer::movie_frame`]).
    pub fn movie_frame(&mut self, frame: MovieFrame) {
        if let Some(replacer) = self.replace.as_deref_mut() {
            replacer.movie_frame(frame);
        }
    }

    /// Texture replacement, when a texture pack or movie frames are in use.
    pub fn replacer(&self) -> Option<&Replacer> {
        self.replace.as_deref()
    }

    /// Read the texture pack's folder again; `None` without a pack. The
    /// replacements are decoded again as they are drawn.
    pub fn reload_texture_pack(&mut self, vram: &Vram) -> Option<Result<String, String>> {
        self.replace
            .as_deref_mut()
            .filter(|replacer| !replacer.pack().is_none())
            .map(|replacer| replacer.reload(vram))
    }

    /// Note a write (anything but an upload) over `rect`.
    fn wrote(&mut self, rect: &Rect) {
        self.mirror_dirty.mark(rect);
        if let Some(replacer) = self.replace.as_deref_mut() {
            replacer.written(rect);
        }
    }

    /// The replacement (slot, upload origin) of a primitive sampling the
    /// current texture page with palette word `clut` at texture
    /// coordinates `u` x `v` (inclusive, as given).
    fn replacement(
        &mut self,
        clut: u32,
        u: (i32, i32),
        v: (i32, i32),
        vram: &Vram,
    ) -> Option<(u32, [i32; 2])> {
        let replacer = self.replace.as_deref_mut()?;
        if self.state.window_mask != [0, 0] {
            replacer.stats.windowed += 1;
            return None;
        }
        let sample = Sample {
            depth: self.state.depth,
            base: self.state.tex_base,
            clut: [((clut & 0x3f) * 16) as i32, ((clut >> 6) & 0x1ff) as i32],
            u,
            v,
        };
        let found = replacer.lookup(&sample, vram);
        if found.is_none() {
            replacer.stats.originals += 1;
        }
        found
    }

    /// Whether the game's dithering applies (always at 1 x).
    fn dithering(&self) -> bool {
        self.exact || self.options.dither
    }

    /// Start over from the software GPU: its whole VRAM, drawing state and
    /// partial command (after a quick load, or when the card side starts).
    /// Drops what was recorded since the last frame.
    pub fn reseed(&mut self, gpu: &Gpu) {
        self.layers.clear();
        self.data.clear();
        if let Some(fx) = self.fx.as_deref_mut() {
            fx.frame = None;
            fx.fires.clear();
            fx.next = 0;
            fx.queued_dests.clear();
            fx.queued_reads.clear();
        }
        self.order = 0;
        self.pipe = None;
        self.vram_access.clear();
        self.mirror_access.clear();
        self.pending = Tiles::empty();
        self.captures.clear();
        // The screen shows the new VRAM's display area.
        self.show = None;
        self.live = true;
        match mirror::of(gpu) {
            Ok(saved) => {
                let dithering = self.dithering();
                self.state = State {
                    dither: saved.state.dither && dithering,
                    status_dither: saved.state.status_dither && dithering,
                    ..saved.state
                };
                self.addresses = vec![0; saved.fifo.len()];
                self.fifo = saved.fifo;
                self.upload = saved.upload;
            }
            Err(error) => {
                eprintln!("ac3: gpu renderer: drawing state not readable ({error}); reset");
                self.state = State::default();
                self.fifo.clear();
                self.addresses.clear();
                self.upload = None;
            }
        }
        self.mirror_dirty = Tiles::empty();
        self.mirror_write(Rect::VRAM, gpu.vram().words(), WIDTH as usize);
        self.replicate(Rect::VRAM);
        // `shadow` too, at the end of the frame.
        self.seal = true;
        self.stats.reseeds += 1;
        if let Some(replacer) = self.replace.as_deref_mut() {
            replacer.reseed(gpu.vram());
        }
    }

    /// A VBlank: the screen shows the display area as it is now, until the
    /// next [`show`](Self::show).
    pub fn vblank(&mut self) {
        self.show = None;
        self.live = true;
    }

    /// The first VRAM write after a VBlank comes next: the screen shows
    /// `area` (the display area) as VRAM is now, until the next VBlank;
    /// `None` when the software picture is shown (a 24-bit display). See
    /// the module documentation, "The shown field".
    pub fn show(&mut self, area: Option<super::DisplayArea>, vram: &Vram) {
        self.resolve_captures(vram);
        let Some(area) = area else {
            self.vblank();
            return;
        };
        let rect = Rect::new(
            area.x as i32,
            area.y as i32,
            area.width as i32,
            area.height as i32,
        );
        let layer = self
            .vram_access
            .latest(&rect, Kind::Writes)
            .map_or(0, |layer| layer + 1);
        self.vram_access.add(&rect, layer, false);
        self.show = Some(Show {
            rect,
            layer,
            overwritten: false,
        });
    }

    /// The recorded pass, leaving an empty one.
    pub fn take_frame(&mut self, vram: &Vram) -> Frame {
        self.resolve_captures(vram);
        let fx = self.take_fx(vram);
        self.stats.frames += 1;
        let layers = std::mem::take(&mut self.layers);
        self.stats.layers += layers.len() as u64;
        self.stats.most_layers = self.stats.most_layers.max(layers.len() as u64);
        let mut frame = Frame {
            data: std::mem::take(&mut self.data),
            ..Frame::default()
        };
        if let Some(replacer) = self.replace.as_deref_mut() {
            (frame.replacements_reset, frame.replacements) = replacer.take();
        }
        // The shown field: copied before the draws of its layer when a
        // later write went into the area (that write's layer is the field's
        // or a later one). Else the area as the frame leaves it is the
        // field, and the point moves to the start of the next frame (no
        // VBlank came after it yet).
        let copy = self.show.filter(|show| show.overwritten);
        debug_assert!(copy.is_none_or(|show| show.layer < layers.len()));
        frame.shown = if copy.is_some() {
            Shown::Copy
        } else if self.show.is_some() || self.live {
            Shown::Live
        } else {
            Shown::Kept
        };
        self.live = false;
        self.show = match (copy, self.show) {
            (None, Some(show)) => Some(Show { layer: 0, ..show }),
            _ => None,
        };
        // What is drawn and not yet in `shadow`; copied before a layer that
        // reads (the first layer reads none of it, by construction).
        let mut unsynced = std::mem::take(&mut self.pending);
        // The effects' vertex words, when the frame has an effects draw.
        let effects = layers.iter().any(|layer| {
            layer
                .items
                .iter()
                .any(|item| !matches!(item, Item::Draw { pipe: None, .. }))
        });
        for (index, mut layer) in layers.into_iter().enumerate() {
            if let Some(show) = copy
                && show.layer == index
            {
                self.stats.shows += 1;
                frame.ops.push(Op::Show(show.rect));
            }
            frame.ops.append(&mut layer.mirror);
            if index > 0 && layer.reads {
                let rects = unsynced.take_all();
                self.sync(&mut frame.ops, rects);
            }
            let base = frame.vertex_count() as u32;
            frame.vertices.append(&mut layer.vertices);
            if effects {
                frame.fx_vertices.append(&mut layer.fx);
            }
            for item in layer.items {
                match item {
                    Item::Draw {
                        first,
                        count,
                        scissor,
                        pipe,
                    } => self.push_draw(&mut frame.ops, base + first, count, scissor, pipe),
                    Item::Fx(step) => frame.ops.push(match step {
                        FxStep::Begin(rect) => Op::FxBegin(rect),
                        FxStep::Post(rect) => Op::FxPost(rect),
                        FxStep::Late(rect) => Op::FxLate(rect),
                        FxStep::Snapshot(rect) => Op::FxSnapshot(rect),
                    }),
                }
            }
            unsynced.add(&layer.written);
        }
        frame.fx = fx;
        self.pending = unsynced;
        if std::mem::take(&mut self.seal) {
            let rects = self.pending.take_all();
            self.sync(&mut frame.ops, rects);
        }
        // The next frame starts after this one: what is not in `shadow` yet
        // counts as drawn in its first layer.
        self.vram_access.clear();
        self.mirror_access.clear();
        for rect in self.pending.rects(&Rect::VRAM) {
            self.vram_access.add(&rect, 0, true);
        }
        if let Some(show) = self.show {
            self.vram_access.add(&show.rect, 0, false);
        }
        frame
    }

    /// A draw into `ops`: merged into the last one when it continues it
    /// (same pipeline and scissor).
    fn push_draw(
        &mut self,
        ops: &mut Vec<Op>,
        first: u32,
        count: u32,
        scissor: Rect,
        pipe: Option<Pipe>,
    ) {
        match (ops.last_mut(), pipe) {
            (
                Some(Op::Draw {
                    first: last_first,
                    count: last_count,
                    scissor: last_scissor,
                }),
                None,
            ) if *last_scissor == scissor && *last_first + *last_count == first => {
                *last_count += count;
                return;
            }
            (
                Some(Op::FxDraw {
                    first: last_first,
                    count: last_count,
                    scissor: last_scissor,
                    pipe: last_pipe,
                }),
                Some(pipe),
            ) if *last_pipe == pipe
                && *last_scissor == scissor
                && *last_first + *last_count == first =>
            {
                *last_count += count;
                return;
            }
            _ => {}
        }
        self.stats.draws += 1;
        ops.push(match pipe {
            None => Op::Draw {
                first,
                count,
                scissor,
            },
            Some(pipe) => Op::FxDraw {
                first,
                count,
                scissor,
                pipe,
            },
        });
    }

    fn sync(&mut self, ops: &mut Vec<Op>, rects: Vec<Rect>) {
        if rects.is_empty() {
            return;
        }
        self.stats.syncs += 1;
        self.stats.sync_rects += rects.len() as u64;
        ops.push(Op::Sync(rects));
    }

    /// A GP1 control word, before the software GPU executes it.
    pub fn gp1(&mut self, word: u32, vram: &Vram) {
        self.resolve_captures(vram);
        if word >> 24 == 0 {
            self.fifo.clear();
            self.addresses.clear();
            self.upload = None;
            self.state = State::default();
        }
    }

    /// One GP0 word, before the software GPU executes it; `vram` is the
    /// software VRAM at that moment. `address` is the word's RAM address in
    /// a DMA-list packet (KSEG0), or 0 (LoadImage).
    pub fn gp0(&mut self, word: u32, address: u32, vram: &Vram) {
        self.resolve_captures(vram);
        if let Some(fx) = self.fx.as_deref_mut() {
            while let Some(&(word, slot)) = fx.slots.get(fx.slot_next) {
                if word != address {
                    break;
                }
                fx.slot = Some(slot);
                fx.slot_next += 1;
            }
            if fx.due() == Some(address) {
                self.fire_markers(Some(address), vram);
            }
        }
        if let Some(upload) = &mut self.upload {
            upload.pixels.push(word as u16);
            upload.pixels.push((word >> 16) as u16);
            if upload.pixels.len() >= (upload.rect.w * upload.rect.h) as usize {
                let upload = self.upload.take().expect("upload in progress");
                self.finish_upload(upload, vram);
            }
            return;
        }
        self.fifo.push(word);
        self.addresses.push(address);
        match self.command_complete() {
            Some(false) => {}
            Some(true) => {
                let words = std::mem::take(&mut self.fifo);
                let addresses = std::mem::take(&mut self.addresses);
                self.execute(&words, &addresses, vram);
                self.fifo = words;
                self.fifo.clear();
                self.addresses = addresses;
                self.addresses.clear();
            }
            None => {
                // The software GPU fails here (an unterminated polyline).
                self.fifo.clear();
                self.addresses.clear();
            }
        }
    }

    /// `ac3_graphics::gpu::Gpu::command_complete`; `None` for its error.
    fn command_complete(&self) -> Option<bool> {
        let words = &self.fifo;
        let op = (words[0] >> 24) as u8;
        let needed = match op {
            0x02 => 3,
            0x20..=0x3f => {
                let vertices = if op & 8 != 0 { 4 } else { 3 };
                let textured = usize::from(op & 4 != 0);
                let gouraud = op & 0x10 != 0;
                1 + vertices * (1 + textured) + if gouraud { vertices - 1 } else { 0 }
            }
            0x40..=0x5f if op & 8 == 0 => {
                if op & 0x10 != 0 {
                    4
                } else {
                    3
                }
            }
            0x40..=0x5f => {
                let first_two = 1 + if op & 0x10 != 0 { 3 } else { 2 };
                if words.len() > first_two && terminator(*words.last().expect("nonempty")) {
                    return Some(true);
                }
                if words.len() > 0x10000 {
                    return None;
                }
                usize::MAX
            }
            0x60..=0x7f => {
                let textured = usize::from(op & 4 != 0);
                2 + textured + usize::from(op & 0x18 == 0)
            }
            0x80..=0x9f => 4,
            0xa0..=0xdf => 3,
            _ => 1,
        };
        Some(words.len() >= needed)
    }

    fn execute(&mut self, words: &[u32], addresses: &[u32], vram: &Vram) {
        let command = words[0];
        match (command >> 24) as u8 {
            0x02 => self.fill(words, addresses, vram),
            0x20..=0x3f => self.polygon(words, addresses, vram),
            0x40..=0x5f => self.line(words, addresses, vram),
            0x60..=0x7f => self.rectangle(words, addresses, vram),
            0x80..=0x9f => self.copy(words, addresses, vram),
            0xa0..=0xbf => self.begin_upload(words, vram),
            0xe1 => {
                self.status_texture_page(command & 0xff_ffff);
                self.texture_page(command & 0xff_ffff);
            }
            0xe2 => {
                let field = |shift: u32| ((command >> shift) & 0x1f) as i32;
                self.state.window_mask = [field(0) << 3, field(5) << 3];
                self.state.window_offset = [
                    (field(10) << 3) & self.state.window_mask[0],
                    (field(15) << 3) & self.state.window_mask[1],
                ];
            }
            0xe3 => {
                self.state.area.left = (command & 0x3ff) as i32;
                self.state.area.top = ((command >> 10) & 0x3ff) as i32;
            }
            0xe4 => {
                self.state.area.right = (command & 0x3ff) as i32;
                self.state.area.bottom = ((command >> 10) & 0x3ff) as i32;
            }
            0xe5 => {
                self.state.offset = [sign_extend_11(command), sign_extend_11(command >> 11)];
            }
            0xe6 => {
                self.state.set_mask = command & 1 != 0;
                self.state.check_mask = command & 2 != 0;
            }
            _ => {}
        }
    }

    fn status_texture_page(&mut self, raw: u32) {
        self.state.status_dither = self.dithering() && raw & 0x200 != 0;
    }

    fn texture_page(&mut self, raw: u32) {
        self.state.tex_base = [((raw & 0xf) << 6) as i32, (((raw >> 4) & 1) << 8) as i32];
        self.state.abr = (raw >> 5) & 3;
        self.state.depth = ((raw >> 7) & 3).min(2);
        self.state.dither = self.dithering() && raw & 0x200 != 0;
    }

    fn offset_point(&self, xy: (i32, i32)) -> (i32, i32) {
        (
            sign_extend_11((xy.0 + self.state.offset[0]) as u32),
            sign_extend_11((xy.1 + self.state.offset[1]) as u32),
        )
    }

    /// The inclusive draw area as a rectangle (in VRAM).
    fn area_rect(&self) -> Rect {
        let area = self.state.area;
        Rect::inclusive(area.left, area.top, area.right, area.bottom).clipped()
    }

    /// The mode bits shared by a primitive's kinds.
    fn mode_bits(&self, kind: u32, style: Style) -> u32 {
        let mut bits = kind;
        if style.textured {
            bits |= mode::TEXTURED;
        }
        if style.raw {
            bits |= mode::RAW;
        }
        if style.gouraud {
            bits |= mode::GOURAUD;
        }
        if style.semi {
            bits |= mode::SEMI | (self.state.abr << mode::ABR_SHIFT);
        }
        if style.dither {
            bits |= mode::DITHER;
        }
        if self.state.check_mask {
            bits |= mode::CHECK;
        }
        if self.state.set_mask {
            bits |= mode::SET;
        }
        if self.exact {
            bits |= mode::EXACT;
        }
        if style.perspective {
            bits |= mode::PERSPECTIVE;
        }
        bits
    }

    /// The texture page and palette word of the flat data.
    fn tex_word(&self, clut: u32) -> u32 {
        let state = &self.state;
        (state.tex_base[0] as u32 / 64)
            | ((state.tex_base[1] as u32 / 256) << 4)
            | (state.depth << 5)
            | ((clut & 0x3f) << 7)
            | (((clut >> 6) & 0x1ff) << 13)
    }

    /// The texture window word: kept bits and offset, per axis.
    fn window_word(&self) -> u32 {
        let state = &self.state;
        let keep = |axis: usize| u32::from(!(state.window_mask[axis] as u8));
        keep(0)
            | (keep(1) << 8)
            | ((state.window_offset[0] as u32) << 16)
            | ((state.window_offset[1] as u32) << 24)
    }

    /// What an effect sees of the texture of a primitive with palette word
    /// `clut` in the current texture page.
    fn texture_info(&self, clut: u32) -> fx::TextureUse {
        fx::TextureUse {
            depth: self.state.depth,
            page: self.state.tex_base,
            clut: [((clut & 0x3f) * 16) as i32, ((clut >> 6) & 0x1ff) as i32],
        }
    }

    fn texture_use(&self, clut: u32, u: (i32, i32), v: (i32, i32)) -> Texture {
        Texture {
            depth: self.state.depth,
            base: self.state.tex_base,
            clut: [((clut & 0x3f) * 16) as i32, ((clut >> 6) & 0x3ff) as i32],
            u,
            v,
        }
    }

    // ---- bookkeeping ----------------------------------------------------------------------

    /// Before a primitive drawing `dests`: bring the mirror up to date where
    /// it decodes indexed texels and palettes, then place the primitive (see
    /// [`place`](Self::place)), with its 15-bit texels among its reads.
    fn prepare(
        &mut self,
        dests: &[Rect],
        mut reads: Vec<Rect>,
        texture: Option<Texture>,
        class: Class,
        vram: &Vram,
    ) {
        let mut mirror_reads = Vec::new();
        if let Some(texture) = texture {
            let (texels, clut) = texture.rects(self.state.window_mask, self.state.window_offset);
            if texture.depth < 2 {
                mirror_reads = texels;
                mirror_reads.extend(clut);
            } else {
                reads.extend(texels);
            }
        }
        self.fx_before(dests, &reads, false, class, vram);
        for rect in &mirror_reads {
            self.refresh(rect, vram);
        }
        self.place(dests, &reads, &mirror_reads);
    }

    /// Choose the layer of a primitive drawing `dests` that reads the card's
    /// VRAM at `reads` (through `shadow`) and the mirror at `mirror_reads`
    /// (see the module documentation); its vertices go there.
    fn place(&mut self, dests: &[Rect], reads: &[Rect], mirror_reads: &[Rect]) {
        let mut layer = 0;
        for dest in dests {
            layer = layer.max(self.vram_access.latest(dest, Kind::Any).unwrap_or(0));
        }
        for read in reads {
            if let Some(written) = self.vram_access.latest(read, Kind::Writes) {
                layer = layer.max(written + 1);
            }
        }
        for read in mirror_reads {
            if let Some(written) = self.mirror_access.latest(read, Kind::Writes) {
                layer = layer.max(written);
            }
        }
        for read in reads {
            self.vram_access.add(read, layer, false);
        }
        for read in mirror_reads {
            self.mirror_access.add(read, layer, false);
        }
        for dest in dests {
            self.vram_access.add(dest, layer, true);
        }
        if let Some(show) = &mut self.show
            && dests.iter().any(|dest| dest.overlaps(&show.rect))
        {
            show.overwritten = true;
        }
        self.open(layer);
        for dest in dests {
            self.layers[layer].written.mark(dest);
        }
        self.layers[layer].reads |= !reads.is_empty();
        self.current = layer;
    }

    /// Make sure layer `index` exists.
    fn open(&mut self, index: usize) {
        while self.layers.len() <= index {
            self.layers.push(Layer::default());
        }
    }

    /// Write the mirror from the software VRAM where it is out of date in `rect`.
    fn refresh(&mut self, rect: &Rect, vram: &Vram) {
        for tiles in self.mirror_dirty.take(rect) {
            self.stats.refreshes += 1;
            self.stats.refreshed_pixels += (tiles.w * tiles.h) as u64;
            self.mirror_write(tiles, vram.words(), WIDTH as usize);
        }
    }

    /// Record a mirror write of `rect` from `pixels` (rows `stride` apart,
    /// starting at the rectangle's position in VRAM when `stride` is the
    /// VRAM width, else at 0): before the first layer that may read it, and
    /// after every earlier read of what it replaces.
    fn mirror_write(&mut self, rect: Rect, pixels: &[u16], stride: usize) {
        let offset = align(self.data.len(), 128);
        let pitch = align(rect.w as usize, 128);
        self.data.resize(offset + pitch * rect.h as usize, 0);
        let origin = if stride == WIDTH as usize {
            rect.y as usize * stride + rect.x as usize
        } else {
            0
        };
        for row in 0..rect.h as usize {
            let from = origin + row * stride;
            let to = offset + row * pitch;
            self.data[to..to + rect.w as usize]
                .copy_from_slice(&pixels[from..from + rect.w as usize]);
        }
        let mut layer = self.mirror_access.latest(&rect, Kind::Writes).unwrap_or(0);
        if let Some(read) = self.mirror_access.latest(&rect, Kind::Reads) {
            layer = layer.max(read + 1);
        }
        self.mirror_access.add(&rect, layer, true);
        self.open(layer);
        self.layers[layer].mirror.push(Op::Mirror {
            rect,
            offset,
            pitch,
        });
    }

    /// Draw the mirror's `rect` into the card's VRAM (replicated at N x).
    fn replicate(&mut self, rect: Rect) {
        self.place(&[rect], &[], &[rect]);
        self.quad(rect, [0.0; 4], 0, mode::KIND_MIRROR, &[0; 16], Rect::VRAM);
    }

    fn resolve_captures(&mut self, vram: &Vram) {
        if self.captures.is_empty() {
            return;
        }
        for rect in std::mem::take(&mut self.captures) {
            self.stats.captures += 1;
            // The software VRAM's pixels: draw the queue first where they
            // cover what it draws or reads (before the mirror changes).
            self.fx_before(&[rect], &[], true, Class::Other, vram);
            self.mirror_write(rect, vram.words(), WIDTH as usize);
            self.mirror_dirty.clear_inside(&rect);
            self.replicate(rect);
            if let Some(replacer) = self.replace.as_deref_mut() {
                replacer.written(&rect);
            }
        }
    }

    // ---- vertices -------------------------------------------------------------------------

    /// One vertex at `[x, y, w]` (w 1 but for perspective-correct polygons),
    /// at view z `depth` (0: none; effects frames only).
    #[allow(clippy::too_many_arguments)]
    fn vertex(
        &mut self,
        [x, y, w]: [f32; 3],
        depth: f32,
        u: f32,
        v: f32,
        rgb: u32,
        bits: u32,
        flat: &[u32; 16],
    ) {
        let layer = &mut self.layers[self.current];
        let vertices = &mut layer.vertices;
        vertices.extend_from_slice(&[
            x.to_bits(),
            y.to_bits(),
            u.to_bits(),
            v.to_bits(),
            rgb & 0xff_ffff,
            bits,
        ]);
        vertices.extend_from_slice(flat);
        vertices.push(w.to_bits());
        if self.fx.is_some() {
            layer.fx.extend_from_slice(&[depth.to_bits(), self.order]);
        }
    }

    /// Note `count` vertices just added to the current layer, clipped to
    /// `scissor`.
    fn draw(&mut self, count: u32, scissor: Rect) {
        let pipe = self.draw_pipe();
        let layer = &mut self.layers[self.current];
        let end = (layer.vertices.len() / VERTEX_WORDS) as u32;
        let first = end - count;
        self.stats.vertices += u64::from(count);
        match layer.items.last_mut() {
            Some(Item::Draw {
                first: last_first,
                count: last_count,
                scissor: last_scissor,
                pipe: last_pipe,
            }) if *last_scissor == scissor
                && *last_pipe == pipe
                && *last_first + *last_count == first =>
            {
                *last_count += count;
            }
            _ => layer.items.push(Item::Draw {
                first,
                count,
                scissor,
                pipe,
            }),
        }
    }

    /// A rectangle of whole pixels (`rect`, sample points at its pixels),
    /// with texture coordinates `[u0, v0, u1, v1]` at its outer corners.
    fn quad(
        &mut self,
        rect: Rect,
        uv: [f32; 4],
        rgb: u32,
        bits: u32,
        flat: &[u32; 16],
        scissor: Rect,
    ) {
        let (x0, y0) = (rect.x as f32 - 0.5, rect.y as f32 - 0.5);
        let (x1, y1) = (rect.right() as f32 - 0.5, rect.bottom() as f32 - 0.5);
        let [u0, v0, u1, v1] = uv;
        for (x, y, u, v) in [
            (x0, y0, u0, v0),
            (x1, y0, u1, v0),
            (x0, y1, u0, v1),
            (x1, y0, u1, v0),
            (x1, y1, u1, v1),
            (x0, y1, u0, v1),
        ] {
            self.vertex([x, y, 1.0], 0.0, u, v, rgb, bits, flat);
        }
        self.draw(6, scissor);
    }

    // ---- primitives -----------------------------------------------------------------------

    fn polygon(&mut self, words: &[u32], addresses: &[u32], vram: &Vram) {
        let before = self.state;
        let op = (words[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let quad = op & 8 != 0;
        let textured = op & 4 != 0;
        let semi = op & 2 != 0;
        let raw = op & 1 != 0;
        let count = if quad { 4 } else { 3 };
        let mut points = [(0, 0); 4];
        let mut vertex_addresses = [0u32; 4];
        let mut vertex_words = [0u32; 4];
        let mut uv = [(0, 0); 4];
        let mut colors = [words[0] & 0xff_ffff; 4];
        let (mut clut, mut page) = (0, 0);
        let mut at = 1;
        for i in 0..count {
            if gouraud && i > 0 {
                colors[i] = words[at] & 0xff_ffff;
                at += 1;
            }
            if gouraud && textured && raw {
                colors[i] = 0x80_8080;
            }
            let xy = words[at];
            vertex_addresses[i] = addresses[at];
            vertex_words[i] = xy;
            at += 1;
            points[i] = (sign_extend_11(xy), sign_extend_11(xy >> 16));
            if textured {
                let word = words[at];
                at += 1;
                uv[i] = ((word & 0xff) as i32, ((word >> 8) & 0xff) as i32);
                match i {
                    0 => clut = word >> 16,
                    1 => page = (word >> 16) & 0x9ff,
                    _ => {}
                }
            }
        }
        let word_points = points;
        let points = points.map(|p| self.offset_point(p));
        let culled = if quad {
            [(0, 1), (1, 3), (3, 2), (2, 0), (1, 2)]
                .iter()
                .any(|&(a, b)| over_limit(points[a], points[b]))
        } else {
            [(0, 1), (1, 2), (2, 0)]
                .iter()
                .any(|&(a, b)| over_limit(points[a], points[b]))
        };
        if culled {
            return;
        }
        if textured {
            let page = if self.state.status_dither {
                page | 0x200
            } else {
                page
            };
            self.status_texture_page(page);
            self.texture_page(page);
        }
        let class = if textured && self.state.depth < 2 && terrain_palette(clut) {
            Class::Terrain
        } else {
            Class::Polygon
        };
        if self.fx.is_some() {
            let texture = textured.then(|| self.texture_info(clut));
            let (route, deferred) = self.route(words, addresses, semi, texture, semi);
            if route == Route::Drop {
                return;
            }
            if textured {
                clut = self.effective_clut(clut);
            }
            if deferred {
                let x = points[..count].iter().map(|p| p.0);
                let y = points[..count].iter().map(|p| p.1);
                let dests = Rect::inclusive(
                    x.clone().min().unwrap_or(0) - 1,
                    y.clone().min().unwrap_or(0) - 1,
                    x.max().unwrap_or(0) + 1,
                    y.max().unwrap_or(0) + 1,
                )
                .intersect(&self.area_rect());
                let reads = if textured {
                    let u = uv[..count].iter().map(|t| t.0);
                    let v = uv[..count].iter().map(|t| t.1);
                    let texture = self.texture_use(
                        clut,
                        (u.clone().min().unwrap_or(0) - 1, u.max().unwrap_or(0) + 1),
                        (v.clone().min().unwrap_or(0) - 1, v.max().unwrap_or(0) + 1),
                    );
                    let (mut texels, palette) =
                        texture.rects(self.state.window_mask, self.state.window_offset);
                    texels.extend(palette);
                    texels
                } else {
                    Vec::new()
                };
                self.defer(words, addresses, before, dests, &reads, class, vram);
                return;
            }
        }
        let mut style = Style {
            gouraud,
            textured,
            semi,
            raw,
            dither: gouraud && self.state.dither,
            perspective: false,
        };
        // Flat polygons modulate by the command colour.
        let rgb = |i: usize| {
            if gouraud {
                colors[i]
            } else {
                words[0] & 0xff_ffff
            }
        };
        let precision = if self.exact {
            None
        } else {
            self.precision.clone()
        };
        let mut corners = [Point::default(); 4];
        for i in 0..count {
            let (x, y) = points[i];
            let (fx, fy, z) = match precision
                .as_ref()
                .and_then(|source| source.precise_vertex(vertex_addresses[i], vertex_words[i]))
            {
                Some([px, py, pz]) => (
                    x as f32 + (px - word_points[i].0 as f32),
                    y as f32 + (py - word_points[i].1 as f32),
                    pz,
                ),
                None => (x as f32, y as f32, 0.0),
            };
            corners[i] = Point {
                x,
                y,
                fx,
                fy,
                z,
                u: uv[i].0,
                v: uv[i].1,
                rgb: rgb(i),
            };
        }
        style.perspective = textured && self.perspective && perspective(&corners[..count]);
        let mut triangles = Vec::with_capacity(2);
        if quad {
            triangles.extend(self.triangle([corners[1], corners[3], corners[2]], style));
        }
        triangles.extend(self.triangle([corners[0], corners[1], corners[2]], style));
        // A convex quad's two triangles never share a pixel, so they are
        // placed as one primitive; a folded one draws the second over the
        // first.
        if quad && !halves_disjoint(&corners) {
            for triangle in triangles {
                self.polygon_part(&[triangle], style, clut, class, vram);
            }
        } else {
            self.polygon_part(&triangles, style, clut, class, vram);
        }
    }

    /// Place and draw `triangles` (one polygon, or one triangle of a folded
    /// quad) as one primitive.
    fn polygon_part(
        &mut self,
        triangles: &[Triangle],
        style: Style,
        clut: u32,
        class: Class,
        vram: &Vram,
    ) {
        if triangles.is_empty() {
            return;
        }
        let dests: Vec<Rect> = triangles.iter().map(|t| t.dest).collect();
        let all = || triangles.iter().flat_map(|t| t.points.iter());
        let u = all().map(|p| p.u);
        let v = all().map(|p| p.v);
        let (u_lo, u_hi) = (u.clone().min().unwrap_or(0), u.max().unwrap_or(0));
        let (v_lo, v_hi) = (v.clone().min().unwrap_or(0), v.max().unwrap_or(0));
        let texture = style
            .textured
            .then(|| self.texture_use(clut, (u_lo - 1, u_hi + 1), (v_lo - 1, v_hi + 1)));
        let reads = if style.semi || self.state.check_mask {
            vec![union(&dests)]
        } else {
            Vec::new()
        };
        self.prepare(&dests, reads, texture, class, vram);
        // The texels used are the vertices' range (the margin above only
        // keeps the mirror up to date around it), without the right and
        // bottom ends a polygon names but never reads, as the asset dump
        // attributes polygons (`asset_dump::key::sampled_hi`).
        let replaced = if style.textured && self.replace.is_some() {
            let vertices = |axis: fn(&Point) -> i32| -> Vec<(f32, f32, i32)> {
                all().map(|p| (p.fx, p.fy, axis(p))).collect()
            };
            let u_read = crate::asset_dump::key::sampled_hi(&vertices(|p| p.u));
            let v_read = crate::asset_dump::key::sampled_hi(&vertices(|p| p.v));
            self.replacement(clut, (u_lo, u_read), (v_lo, v_read), vram)
        } else {
            None
        };
        for triangle in triangles {
            self.emit_triangle(triangle, style, clut, replaced);
        }
        for dest in &dests {
            self.wrote(dest);
        }
    }

    /// The software GPU's checks and set-up of one triangle; `None` when it
    /// draws nothing.
    fn triangle(&self, v: [Point; 3], style: Style) -> Option<Triangle> {
        let area = self.state.area;
        if area.top >= area.bottom || area.left >= area.right {
            return None;
        }
        if v.iter().all(|p| p.x > area.right)
            || v.iter().all(|p| p.y > area.bottom)
            || v.iter().all(|p| p.x < area.left)
            || v.iter().all(|p| p.y < area.top)
        {
            return None;
        }
        let setup = Setup::new(v, style.gouraud || style.textured)?;
        let min_x = v.iter().map(|p| p.x).min().expect("three");
        let max_x = v.iter().map(|p| p.x).max().expect("three");
        let (min_y, max_y) = (setup.p[0].y, setup.p[2].y);
        let area_rect = self.area_rect();
        let dest = if self.exact {
            // The pixels the software GPU can reach: rows up to the last
            // vertex row (exclusive), columns of the vertices.
            Rect::inclusive(min_x, min_y, max_x, max_y - 1).intersect(&area_rect)
        } else {
            let lo_x = v.iter().map(|p| p.fx).fold(f32::INFINITY, f32::min).floor() as i32;
            let hi_x = v
                .iter()
                .map(|p| p.fx)
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil() as i32;
            let lo_y = v.iter().map(|p| p.fy).fold(f32::INFINITY, f32::min).floor() as i32;
            let hi_y = v
                .iter()
                .map(|p| p.fy)
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil() as i32;
            Rect::inclusive(lo_x - 1, lo_y - 1, hi_x + 1, hi_y + 1).intersect(&area_rect)
        };
        (!dest.is_empty()).then_some(Triangle {
            points: v,
            setup,
            dest,
        })
    }

    /// The mode bits and window word of a primitive with or without a
    /// replacement (slot, upload origin).
    fn replaced_bits(&self, bits: u32, replaced: Option<(u32, [i32; 2])>) -> (u32, u32) {
        match replaced {
            Some((slot, [x, y])) => (bits | ((slot + 1) << mode::SLOT_SHIFT), pack_xy(x, y)),
            None => (bits, self.window_word()),
        }
    }

    fn emit_triangle(
        &mut self,
        triangle: &Triangle,
        style: Style,
        clut: u32,
        replaced: Option<(u32, [i32; 2])>,
    ) {
        self.stats.triangles += 1;
        let area = self.state.area;
        let mut bits = self.mode_bits(mode::KIND_POLY, style);
        if style.textured && style.gouraud && !style.dither {
            bits |= if style.semi || self.state.check_mask {
                mode::SCALAR
            } else {
                mode::SHARE
            };
        }
        let (bits, window) = self.replaced_bits(bits, replaced);
        let setup = &triangle.setup;
        let p = &setup.p;
        let uv = |i: usize| (p[i].u as u32) | ((p[i].v as u32) << 8);
        let flags = u32::from(setup.long_left)
            | ((area.left as u32 & 0x3ff) << 1)
            | ((area.right as u32 & 0x3ff) << 11);
        let flat = [
            self.tex_word(clut),
            window,
            flags,
            setup.dc[2] as u32,
            pack_xy(p[0].x, p[0].y),
            pack_xy(p[1].x, p[1].y),
            pack_xy(p[2].x, p[2].y),
            uv(0) | (uv(1) << 16),
            uv(2),
            p[0].rgb,
            p[1].rgb,
            p[2].rgb,
            setup.du as u32,
            setup.dv as u32,
            setup.dc[0] as u32,
            setup.dc[1] as u32,
        ];
        let scissor = self.area_rect();
        if self.exact {
            // The flat polygons' colour rides in the vertex colour.
            self.quad(
                triangle.dest,
                [0.0; 4],
                triangle.points[0].rgb,
                bits,
                &flat,
                scissor,
            );
        } else {
            if style.perspective {
                self.stats.perspective_triangles += 1;
            }
            // The view z of an effects frame's triangle, when all three
            // vertices have one.
            let depth =
                self.fx.is_some() && triangle.points.iter().all(|p| p.z > 0.0 && p.z.is_finite());
            if self.draw_pipe() == Some(Pipe::World)
                && let Some(fx) = self.fx.as_deref_mut()
            {
                if depth {
                    fx.stats.depth_triangles += 1;
                } else {
                    fx.stats.flat_triangles += 1;
                }
            }
            for point in triangle.points {
                let w = if style.perspective { point.z } else { 1.0 };
                self.vertex(
                    [point.fx, point.fy, w],
                    if depth { point.z } else { 0.0 },
                    point.u as f32,
                    point.v as f32,
                    point.rgb,
                    bits,
                    &flat,
                );
            }
            self.draw(3, scissor);
        }
    }

    fn line(&mut self, words: &[u32], addresses: &[u32], vram: &Vram) {
        let op = (words[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let poly = op & 8 != 0;
        let semi = op & 2 != 0;
        let point = |xy: u32| (sign_extend_11(xy), sign_extend_11(xy >> 16));
        let mut colors = vec![words[0] & 0xff_ffff];
        let mut points = vec![point(words[1])];
        let end = if poly { words.len() - 1 } else { words.len() };
        let mut at = 2;
        while at < end {
            if gouraud {
                colors.push(words[at] & 0xff_ffff);
                at += 1;
                if at >= end {
                    break;
                }
            } else {
                colors.push(colors[0]);
            }
            points.push(point(words[at]));
            at += 1;
        }
        if colors.len() < 2 || colors.len() != points.len() {
            return;
        }
        if self.fx.is_some() {
            let (route, deferred) = self.route(words, addresses, semi, None, semi);
            if route == Route::Drop {
                return;
            }
            if deferred {
                let ends: Vec<(i32, i32)> = points.iter().map(|&p| self.offset_point(p)).collect();
                let x = ends.iter().map(|p| p.0);
                let y = ends.iter().map(|p| p.1);
                // Above 1 x a line reaches into the pixels beside its ends.
                let dests = Rect::inclusive(
                    x.clone().min().unwrap_or(0) - 1,
                    y.clone().min().unwrap_or(0) - 1,
                    x.max().unwrap_or(0) + 1,
                    y.max().unwrap_or(0) + 1,
                )
                .intersect(&self.area_rect());
                let state = self.state;
                self.defer(words, addresses, state, dests, &[], Class::Line, vram);
                return;
            }
        }
        for i in 1..points.len() {
            let a = self.offset_point(points[i - 1]);
            let b = self.offset_point(points[i]);
            if over_limit(a, b) {
                continue;
            }
            self.segment(a, b, [colors[i - 1], colors[i]], gouraud, semi, vram);
        }
    }

    fn segment(
        &mut self,
        a: (i32, i32),
        b: (i32, i32),
        rgb: [u32; 2],
        gouraud: bool,
        semi: bool,
        vram: &Vram,
    ) {
        let area = self.state.area;
        if (a.0 > area.right && b.0 > area.right)
            || (a.1 > area.bottom && b.1 > area.bottom)
            || (a.0 < area.left && b.0 < area.left)
            || (a.1 < area.top && b.1 < area.top)
            || area.top >= area.bottom
            || area.left >= area.right
        {
            return;
        }
        let area_rect = self.area_rect();
        let dest = Rect::inclusive(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
            .intersect(&area_rect);
        if dest.is_empty() {
            return;
        }
        let reads = if semi || self.state.check_mask {
            vec![dest]
        } else {
            Vec::new()
        };
        if self.fx.is_some() {
            // Above 1 x a line reaches into the pixels beside its ends.
            let reach = Rect::inclusive(dest.x - 1, dest.y - 1, dest.right(), dest.bottom())
                .intersect(&area_rect);
            let read = if reads.is_empty() {
                Vec::new()
            } else {
                vec![reach]
            };
            self.fx_before(&[reach], &read, false, Class::Line, vram);
        }
        self.place(&[dest], &reads, &[]);
        self.stats.lines += 1;
        let style = Style {
            gouraud,
            semi,
            ..Style::default()
        };
        let bits = self.mode_bits(mode::KIND_LINE, style);
        let flat = [
            rgb[0],
            rgb[1],
            pack_xy(area.left, area.top),
            pack_xy(area.right, area.bottom),
            pack_xy(a.0, a.1),
            pack_xy(b.0, b.1),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ];
        if self.exact {
            self.quad(dest, [0.0; 4], rgb[0], bits, &flat, area_rect);
        } else {
            self.parallelogram(a, b, rgb, bits, &flat, area_rect);
        }
        self.wrote(&dest);
    }

    /// A line one pixel wide around the segment from `a` to `b` (sample
    /// points on it), half a pixel longer at both ends. The software GPU
    /// walks lines from their left end (top end when vertical) with the first
    /// vertex's colour there, whichever vertex that is.
    fn parallelogram(
        &mut self,
        a: (i32, i32),
        b: (i32, i32),
        rgb: [u32; 2],
        bits: u32,
        flat: &[u32; 16],
        scissor: Rect,
    ) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (start, end) = if dx < 0 || (dx == 0 && dy < 0) {
            (b, a)
        } else {
            (a, b)
        };
        let (sx, sy) = (start.0 as f32, start.1 as f32);
        let (ex, ey) = (end.0 as f32, end.1 as f32);
        let corners: [(f32, f32, u32); 4] = if dx.abs() >= dy.abs() {
            let slope = if dx == 0 { 0.0 } else { (ey - sy) / (ex - sx) };
            let (ly, ry) = (sy - 0.5 * slope, ey + 0.5 * slope);
            [
                (sx - 0.5, ly - 0.5, rgb[0]),
                (ex + 0.5, ry - 0.5, rgb[1]),
                (ex + 0.5, ry + 0.5, rgb[1]),
                (sx - 0.5, ly + 0.5, rgb[0]),
            ]
        } else {
            let step = (ey - sy).signum();
            let slope = (ex - sx) / (ey - sy);
            let (tx, bx) = (sx - 0.5 * step * slope, ex + 0.5 * step * slope);
            let (ty, by) = (sy - 0.5 * step, ey + 0.5 * step);
            [
                (tx - 0.5, ty, rgb[0]),
                (tx + 0.5, ty, rgb[0]),
                (bx + 0.5, by, rgb[1]),
                (bx - 0.5, by, rgb[1]),
            ]
        };
        for i in [0, 1, 2, 0, 2, 3] {
            let (x, y, color) = corners[i];
            self.vertex([x, y, 1.0], 0.0, 0.0, 0.0, color, bits, flat);
        }
        self.draw(6, scissor);
    }

    fn rectangle(&mut self, words: &[u32], addresses: &[u32], vram: &Vram) {
        let op = (words[0] >> 24) as u8;
        let textured = op & 4 != 0;
        let semi = op & 2 != 0;
        let raw = op & 1 != 0;
        let x = sign_extend_11(words[1]);
        let y = sign_extend_11(words[1] >> 16);
        let (w, h) = match (op >> 3) & 3 {
            0 => {
                let size = words[2 + usize::from(textured)];
                ((size & 0x3ff) as i32, ((size >> 16) & 0x1ff) as i32)
            }
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        if w == 0 || h == 0 {
            return;
        }
        let color = words[0] & 0xff_ffff;
        let (x0, y0) = (x + self.state.offset[0], y + self.state.offset[1]);
        let area_rect = self.area_rect();
        let rect = Rect::new(x0, y0, w, h).intersect(&area_rect);
        if rect.is_empty() {
            return;
        }
        let (u0, v0, mut clut) = if textured {
            let word = words[2];
            (
                (word & 0xff) as i32 + rect.x - x0,
                ((word >> 8) & 0xff) as i32 + rect.y - y0,
                word >> 16,
            )
        } else {
            (0, 0, 0)
        };
        if self.fx.is_some() {
            let texture = textured.then(|| self.texture_info(clut));
            let (route, deferred) = self.route(words, addresses, semi, texture, semi);
            if route == Route::Drop {
                return;
            }
            if textured {
                clut = self.effective_clut(clut);
            }
            if deferred {
                let reads = if textured {
                    let texture =
                        self.texture_use(clut, (u0, u0 + rect.w - 1), (v0, v0 + rect.h - 1));
                    let (mut texels, palette) =
                        texture.rects(self.state.window_mask, self.state.window_offset);
                    texels.extend(palette);
                    texels
                } else {
                    Vec::new()
                };
                let state = self.state;
                self.defer(words, addresses, state, rect, &reads, Class::Sprite, vram);
                return;
            }
        }
        let texture =
            textured.then(|| self.texture_use(clut, (u0, u0 + rect.w - 1), (v0, v0 + rect.h - 1)));
        let reads = if semi || self.state.check_mask {
            vec![rect]
        } else {
            Vec::new()
        };
        self.prepare(&[rect], reads, texture, Class::Sprite, vram);
        self.stats.rectangles += 1;
        let style = Style {
            textured,
            semi,
            raw,
            ..Style::default()
        };
        let replaced = if textured {
            self.replacement(clut, (u0, u0 + rect.w - 1), (v0, v0 + rect.h - 1), vram)
        } else {
            None
        };
        let (bits, window) = self.replaced_bits(self.mode_bits(mode::KIND_RECT, style), replaced);
        let mut flat = [0; 16];
        flat[0] = self.tex_word(clut);
        flat[1] = window;
        flat[2] = (rect.x as u32) | ((rect.right() as u32) << 16);
        let uv = [
            u0 as f32,
            v0 as f32,
            (u0 + rect.w) as f32,
            (v0 + rect.h) as f32,
        ];
        self.quad(rect, uv, color, bits, &flat, area_rect);
        self.wrote(&rect);
    }

    fn fill(&mut self, words: &[u32], addresses: &[u32], vram: &Vram) {
        let x = (words[1] & 0x3f0) as i32;
        let y = (words[1] >> 16) as i32;
        let w = ((((words[2] & 0x3ff) + 0xf) & !0xf) as i32).min(1024);
        let h = ((words[2] >> 16) & 0x1ff) as i32;
        if w == 0 || h == 0 || y >= HEIGHT || x + w > WIDTH || y + h > HEIGHT {
            return;
        }
        if self.fx.is_some() && self.route(words, addresses, false, None, false).0 == Route::Drop {
            return;
        }
        let rect = Rect::new(x, y, w, h);
        self.stats.fills += 1;
        self.fx_before(&[rect], &[], false, Class::Other, vram);
        self.place(&[rect], &[], &[]);
        let mut flat = [0; 16];
        flat[0] = command_color(words[0] & 0xff_ffff);
        self.quad(rect, [0.0; 4], 0, mode::KIND_FILL, &flat, Rect::VRAM);
        self.wrote(&rect);
    }

    /// The parts of a `w` x `h` VRAM rectangle at (`x`, `y`) that wraps at
    /// the VRAM edges, with their offsets inside it.
    fn wrapped(x: i32, y: i32, w: i32, h: i32) -> Vec<(Rect, i32, i32)> {
        let columns: Vec<(i32, i32, i32)> = if x + w > WIDTH {
            vec![(x, WIDTH - x, 0), (0, x + w - WIDTH, WIDTH - x)]
        } else {
            vec![(x, w, 0)]
        };
        let rows: Vec<(i32, i32, i32)> = if y + h > HEIGHT {
            vec![(y, HEIGHT - y, 0), (0, y + h - HEIGHT, HEIGHT - y)]
        } else {
            vec![(y, h, 0)]
        };
        let mut parts = Vec::new();
        for &(py, ph, oy) in &rows {
            for &(px, pw, ox) in &columns {
                parts.push((Rect::new(px, py, pw, ph), ox, oy));
            }
        }
        parts
    }

    fn copy(&mut self, words: &[u32], addresses: &[u32], vram: &Vram) {
        let coordinate = |word: u32| (sign_extend_11(word), sign_extend_11(word >> 16));
        let (sx, sy) = coordinate(words[1]);
        let (dx, dy) = coordinate(words[2]);
        let w = (((words[3] & 0xffff).wrapping_sub(1) & 0x3ff) + 1) as i32;
        let h = (((words[3] >> 16).wrapping_sub(1) & 0x1ff) + 1) as i32;
        let inside = |x: i32, y: i32| (0..WIDTH).contains(&x) && (0..HEIGHT).contains(&y);
        if !inside(sx, sy) || !inside(dx, dy) {
            return;
        }
        let (set, check) = (self.state.set_mask, self.state.check_mask);
        if sx == dx && sy == dy && !set {
            return;
        }
        self.stats.copies += 1;
        let sources = Self::wrapped(sx, sy, w, h);
        let targets = Self::wrapped(dx, dy, w, h);
        let overlap = sources
            .iter()
            .any(|(s, _, _)| targets.iter().any(|(t, _, _)| s.overlaps(t)));
        // The software GPU copies forward, row by row: a target before its
        // source reads only pixels not yet copied over, as a copy from
        // `shadow` does. Other overlaps (and overlaps across the edges)
        // smear; take their result from the software VRAM.
        let ahead = dy < sy || (dy == sy && dx < sx);
        if overlap && (!ahead || sources.len() > 1 || targets.len() > 1) {
            let rects: Vec<Rect> = targets.iter().map(|(rect, _, _)| *rect).collect();
            // The software GPU is about to write them: the queue first.
            self.fx_flush(&rects, &[], true, vram);
            self.captures.extend(rects);
            return;
        }
        let dests: Vec<Rect> = targets.iter().map(|(rect, _, _)| *rect).collect();
        let mut reads: Vec<Rect> = sources.iter().map(|(rect, _, _)| *rect).collect();
        if check {
            reads.extend(dests.iter().copied());
        }
        if self.fx.is_some() {
            // A copy into the world's area is deferred, so it copies the
            // finished (post-processed) pixels.
            let onto = self
                .fx
                .as_deref()
                .and_then(|fx| fx.world())
                .is_some_and(|frame| {
                    dests
                        .iter()
                        .all(|dest| dest.intersect(&frame.world_area) == *dest)
                });
            let (route, deferred) = self.route(words, addresses, false, None, onto);
            if route == Route::Drop {
                return;
            }
            if deferred {
                let state = self.state;
                self.defer(
                    words,
                    addresses,
                    state,
                    union(&dests),
                    &reads,
                    Class::Other,
                    vram,
                );
                return;
            }
        }
        self.fx_before(&dests, &reads, false, Class::Other, vram);
        self.place(&dests, &reads, &[]);
        for (rect, ox, oy) in targets {
            let mut flat = [0; 16];
            flat[0] = pack_xy((sx + ox) & (WIDTH - 1), (sy + oy) & (HEIGHT - 1));
            flat[1] = pack_xy(rect.x, rect.y);
            flat[2] = u32::from(set) | (u32::from(check) << 1);
            self.quad(rect, [0.0; 4], 0, mode::KIND_COPY, &flat, Rect::VRAM);
            self.wrote(&rect);
        }
    }

    fn begin_upload(&mut self, words: &[u32], vram: &Vram) {
        let x = (words[1] & 0xffff) as i32 % WIDTH;
        let y = (words[1] >> 16) as i32 % HEIGHT;
        let width = ((words[2] & 0xffff).wrapping_sub(1) & 0x3ff) as i32 + 1;
        let height = ((words[2] >> 16).wrapping_sub(1) & 0x1ff) as i32 + 1;
        if x + width > WIDTH || y + height > HEIGHT {
            return;
        }
        // The software GPU writes its pixels as they come: the queue first.
        let rect = Rect::new(x, y, width, height);
        if self.fx.is_some() {
            self.fx_flush(&[rect], &[], true, vram);
        }
        self.upload = Some(Upload {
            rect,
            pixels: Vec::with_capacity((width * height) as usize + 1),
        });
    }

    /// Uploads ignore the mask bits.
    fn finish_upload(&mut self, upload: Upload, vram: &Vram) {
        let rect = upload.rect;
        self.stats.uploads += 1;
        self.fx_before(&[rect], &[], true, Class::Other, vram);
        self.mirror_write(rect, &upload.pixels, rect.w as usize);
        self.mirror_dirty.clear_inside(&rect);
        self.replicate(rect);
        if let Some(replacer) = self.replace.as_deref_mut() {
            replacer.upload(rect, &upload.pixels);
        }
    }
}

/// The software GPU's drawing state, partial command and transfer, read
/// from its serialized form (its fields are private). Postcard writes
/// fields in declaration order, so these types mirror the beginning of
/// `ac3_graphics::Gpu` field for field; `recorder_reads_the_software_state`
/// fails if that layout changes.
mod mirror {
    use super::{Area, Rect, State, Upload};
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Vram {
        #[allow(dead_code)]
        words: Vec<u16>,
    }
    #[derive(Deserialize)]
    struct DrawArea {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    #[derive(Deserialize)]
    enum Depth {
        Clut4,
        Clut8,
        Direct15,
    }
    #[derive(Deserialize)]
    struct DrawState {
        area: DrawArea,
        offset: [i32; 2],
        tex_base: [i32; 2],
        depth: Depth,
        abr: ac3_graphics::BlendMode,
        dither: bool,
        status_dither: bool,
        window_mask: [i32; 2],
        window_offset: [i32; 2],
        set_mask: u16,
        check_mask: bool,
        #[allow(dead_code)]
        cheat: u16,
    }
    #[derive(Deserialize)]
    struct Transfer {
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        pixels: Vec<u16>,
    }
    #[derive(Deserialize)]
    struct GpuStart {
        #[allow(dead_code)]
        vram: Vram,
        state: DrawState,
        #[allow(dead_code)]
        dithering: ac3_graphics::Dithering,
        fifo: Vec<u32>,
        upload: Option<Transfer>,
    }

    pub(super) struct Saved {
        pub state: State,
        pub fifo: Vec<u32>,
        pub upload: Option<Upload>,
    }

    pub(super) fn of(gpu: &ac3_graphics::Gpu) -> Result<Saved, postcard::Error> {
        let bytes = postcard::to_allocvec(gpu)?;
        let (start, _): (GpuStart, _) = postcard::take_from_bytes(&bytes)?;
        let s = start.state;
        Ok(Saved {
            state: State {
                area: Area {
                    left: s.area.left,
                    top: s.area.top,
                    right: s.area.right,
                    bottom: s.area.bottom,
                },
                offset: s.offset,
                tex_base: s.tex_base,
                depth: match s.depth {
                    Depth::Clut4 => 0,
                    Depth::Clut8 => 1,
                    Depth::Direct15 => 2,
                },
                abr: match s.abr {
                    ac3_graphics::BlendMode::Average => 0,
                    ac3_graphics::BlendMode::Add => 1,
                    ac3_graphics::BlendMode::Subtract => 2,
                    ac3_graphics::BlendMode::AddQuarter => 3,
                },
                dither: s.dither,
                status_dither: s.status_dither,
                window_mask: s.window_mask,
                window_offset: s.window_offset,
                set_mask: s.set_mask != 0,
                check_mask: s.check_mask,
            },
            fifo: start.fifo,
            upload: start.upload.map(|t| Upload {
                rect: Rect::new(t.x as i32, t.y as i32, t.width as i32, t.height as i32),
                pixels: t.pixels,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recorder at `scale` and its software GPU, the re-seed dropped.
    fn pair(scale: u32) -> (Recorder, Gpu) {
        let gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(
            Options {
                scale,
                dither: false,
            },
            &gpu,
        );
        recorder.take_frame(gpu.vram());
        (recorder, gpu)
    }

    /// Feed `words` to both (the recorder first, as the device does).
    fn feed(recorder: &mut Recorder, gpu: &mut Gpu, words: &[u32]) {
        for (i, &word) in words.iter().enumerate() {
            recorder.gp0(word, 0x8010_0004 + 4 * i as u32, gpu.vram());
            gpu.write_gp0(word).unwrap();
        }
    }

    const FULL_AREA: [u32; 2] = [0xe300_0000, 0xe407_ffff];

    fn vertex(frame: &Frame, index: usize) -> &[u32] {
        &frame.vertices[index * VERTEX_WORDS..(index + 1) * VERTEX_WORDS]
    }

    fn position(frame: &Frame, index: usize) -> (f32, f32) {
        let v = vertex(frame, index);
        (f32::from_bits(v[0]), f32::from_bits(v[1]))
    }

    fn kinds(frame: &Frame) -> Vec<&'static str> {
        frame
            .ops
            .iter()
            .map(|op| match op {
                Op::Draw { .. } => "draw",
                Op::Sync(_) => "sync",
                Op::Mirror { .. } => "mirror",
                Op::Show(_) => "show",
                Op::FxDraw {
                    pipe: Pipe::World, ..
                } => "world",
                Op::FxDraw {
                    pipe: Pipe::ZClear, ..
                } => "zclear",
                Op::FxDraw {
                    pipe: Pipe::Deferred,
                    ..
                } => "deferred",
                Op::FxBegin(_) => "begin",
                Op::FxPost(_) => "post",
                Op::FxLate(_) => "late",
                Op::FxSnapshot(_) => "snapshot",
            })
            .collect()
    }

    #[test]
    fn the_shown_field_is_copied_before_a_later_write_into_it() {
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        let area = super::super::DisplayArea {
            x: 0,
            y: 0,
            width: 320,
            height: 480,
        };
        let display = Rect::new(0, 0, 320, 480);
        // The JP credits: drawn, the VBlank, then frame end's clear. The
        // copy goes between the two.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x0200_00ff, 0x0000_0000, 0x01e0_0140],
        );
        recorder.vblank();
        recorder.show(Some(area), gpu.vram());
        feed(
            &mut recorder,
            &mut gpu,
            &[0x0200_0000, 0x0000_0000, 0x01e0_0140],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["draw", "show", "draw"]);
        assert_eq!(frame.ops[1], Op::Show(display));
        assert_eq!(frame.shown, Shown::Copy);
        assert_eq!(recorder.stats().shows, 1);
        // A VBlank and no write: the area as it is.
        recorder.vblank();
        let frame = recorder.take_frame(gpu.vram());
        assert!(frame.ops.is_empty());
        assert_eq!(frame.shown, Shown::Live);
        // A pass with neither: as before.
        assert_eq!(recorder.take_frame(gpu.vram()).shown, Shown::Kept);
        // Double buffering: the writes after the VBlank draw elsewhere (in
        // two layers, past the field's), so the area as the frame leaves
        // it is the field; no copy.
        recorder.vblank();
        recorder.show(Some(area), gpu.vram());
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6000_00f8, 0x0000_0200, 0x0010_0010],
        );
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6200_f800, 0x0004_0204, 0x0010_0010],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["draw", "sync", "draw"]);
        assert_eq!(frame.shown, Shown::Live);
        // A pass with no VBlank that draws into the area: the field is
        // copied first.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x0200_ff00, 0x0000_0000, 0x0010_0010],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["show", "draw"]);
        assert_eq!(frame.shown, Shown::Copy);
        // A 24-bit display shows the software picture: no copy.
        recorder.vblank();
        recorder.show(None, gpu.vram());
        feed(
            &mut recorder,
            &mut gpu,
            &[0x0200_00ff, 0x0000_0000, 0x01e0_0140],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["draw"]);
        assert_eq!(frame.shown, Shown::Live);
        assert_eq!(recorder.stats().shows, 2);
    }

    #[test]
    fn a_re_seed_fills_vram_and_shadow() {
        let gpu = Gpu::new(Vram::new());
        let mut recorder = Recorder::new(Options::default(), &gpu);
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["mirror", "draw", "sync"]);
        assert_eq!(frame.ops[2], Op::Sync(vec![Rect::VRAM]));
        assert_eq!(frame.data.len(), 1024 * 512);
        // Nothing is left for the next frame.
        assert!(recorder.take_frame(gpu.vram()).is_empty());
    }

    #[test]
    fn tile_records_one_quad_of_whole_pixels() {
        let (mut recorder, mut gpu) = pair(1);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // TILE, red 16x8 at (4, 2).
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6000_00f8, 0x0002_0004, 0x0008_0010],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(
            frame.ops,
            vec![Op::Draw {
                first: 0,
                count: 6,
                scissor: Rect::VRAM,
            }]
        );
        // Corners half a pixel outside the sample points 4..19, 2..9.
        assert_eq!(position(&frame, 0), (3.5, 1.5));
        assert_eq!(position(&frame, 4), (19.5, 9.5));
        let v = vertex(&frame, 0);
        assert_eq!(v[4], 0xf8);
        assert_eq!(v[5] & 7, mode::KIND_RECT);
        assert_eq!(v[5] & mode::EXACT, mode::EXACT);
        assert_eq!(recorder.stats().rectangles, 1);
    }

    #[test]
    fn draw_offset_and_area_clip() {
        let (mut recorder, mut gpu) = pair(2);
        // Area (10, 10)..(19, 19), offset (100, 0): a 16x16 tile at (-95, 5)
        // lands at (5, 5) and is cut to (10, 10)..(20, 20).
        feed(
            &mut recorder,
            &mut gpu,
            &[0xe300_280a, 0xe400_4c13, 0xe500_0064],
        );
        feed(&mut recorder, &mut gpu, &[0x7800_ffff, 0x0005_ffa1]);
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(
            frame.ops,
            vec![Op::Draw {
                first: 0,
                count: 6,
                scissor: Rect::new(10, 10, 10, 10),
            }]
        );
        assert_eq!(position(&frame, 0), (9.5, 9.5));
        assert_eq!(position(&frame, 4), (19.5, 19.5));
    }

    #[test]
    fn semi_transparency_over_new_pixels_waits_for_the_next_layer() {
        let (mut recorder, mut gpu) = pair(4);
        let syncs = recorder.stats().syncs;
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // Opaque tile A, then a semi-transparent tile B over it: B needs
        // `shadow` to hold A, so it goes into the next layer.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6000_00f8, 0x0000_0000, 0x0010_0010],
        );
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6200_f800, 0x0004_0004, 0x0010_0010],
        );
        // C, semi-transparent elsewhere (in A's tile row but not touching
        // A or B), joins A's layer; D, opaque over B, follows B.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6200_f800, 0x0000_0020, 0x0010_0010],
        );
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6000_001f, 0x0008_0008, 0x0004_0004],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["draw", "sync", "draw"]);
        // Layer 0: A and C (12 vertices); the update copies their tiles.
        assert_eq!(
            frame.ops[0],
            Op::Draw {
                first: 0,
                count: 12,
                scissor: Rect::VRAM,
            }
        );
        assert_eq!(
            frame.ops[1],
            Op::Sync(vec![Rect::new(0, 0, 16, 16), Rect::new(32, 0, 16, 16)])
        );
        assert_eq!(
            frame.ops[2],
            Op::Draw {
                first: 12,
                count: 12,
                scissor: Rect::VRAM,
            }
        );
        // B then D, in command order.
        assert_eq!(position(&frame, 12), (3.5, 3.5));
        assert_eq!(position(&frame, 18), (7.5, 7.5));
        assert_eq!(recorder.stats().syncs, syncs + 1);
        assert_eq!(recorder.stats().most_layers, 2);
    }

    #[test]
    fn writes_never_move_before_reads_of_their_pixels() {
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // A (layer 0), B semi over A (layer 1), then C opaque over B's area
        // but not A's: C must not go before B reads.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6000_00f8, 0x0000_0000, 0x0004_0004],
        );
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6200_f800, 0x0002_0002, 0x0008_0008],
        );
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6000_001f, 0x0006_0006, 0x0004_0004],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["draw", "sync", "draw"]);
        let Op::Draw { first, count, .. } = frame.ops[2] else {
            panic!("{:?}", frame.ops);
        };
        assert_eq!((first, count), (6, 12), "B and C after the update");
        // The next frame starts with B and C not in `shadow` yet: a read
        // there needs an update first.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x6200_f800, 0x0007_0007, 0x0002_0002],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["sync", "draw"]);
    }

    #[test]
    fn indexed_textures_refresh_the_mirror_where_vram_changed() {
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // Fill the 4-bit texture area of page 5 (x 320) with a fill; then
        // draw a 4-bit sprite from it, palette at (0, 480).
        feed(
            &mut recorder,
            &mut gpu,
            &[0x0200_00ff, 0x0000_0140, 0x0010_0010],
        );
        feed(&mut recorder, &mut gpu, &[0xe100_0005]);
        // SPRT16 at (0, 300), uv (0, 0), clut (0, 480) = 480 << 6.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x7c80_8080, 0x012c_0000, 0x7800_0000],
        );
        let frame = recorder.take_frame(gpu.vram());
        let mirrors: Vec<(&Rect, usize)> = frame
            .ops
            .iter()
            .filter_map(|op| match op {
                Op::Mirror { rect, offset, .. } => Some((rect, *offset)),
                _ => None,
            })
            .collect();
        // The filled texels (x 320..324 in 4-bit: 16 texels = 4 halfwords,
        // in the tile at 320) are refreshed; the palette was never changed.
        assert_eq!(mirrors.len(), 1);
        assert_eq!(mirrors[0].0, &Rect::new(320, 0, 16, 16));
        assert_eq!(recorder.stats().refreshes, 1);
        // The refresh holds the software VRAM's fill colour.
        assert_eq!(frame.data[mirrors[0].1], 0x1f);
    }

    #[test]
    fn uploads_go_through_the_mirror() {
        let (mut recorder, mut gpu) = pair(3);
        // A0: 3x1 at (8, 4).
        feed(
            &mut recorder,
            &mut gpu,
            &[
                0xa000_0000,
                0x0004_0008,
                0x0001_0003,
                0x0567_1234,
                0x0000_7fff,
            ],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(
            frame.ops[0],
            Op::Mirror {
                rect: Rect::new(8, 4, 3, 1),
                offset: 0,
                pitch: 128,
            }
        );
        assert_eq!(&frame.data[..3], &[0x1234, 0x0567, 0x7fff]);
        let Op::Draw { count: 6, .. } = frame.ops[1] else {
            panic!("{:?}", frame.ops);
        };
        assert_eq!(vertex(&frame, 0)[5] & 7, mode::KIND_MIRROR);
        assert_eq!(recorder.stats().uploads, 1);
    }

    #[test]
    fn mirror_writes_wait_for_earlier_readers() {
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // Upload a 4-bit texture, draw a sprite from it, upload over it and
        // draw again: the second upload goes after the first sprite (into a
        // second layer, which reads no `shadow`: no update).
        feed(&mut recorder, &mut gpu, &[0xe100_0005]);
        let upload = [
            0xa000_0000,
            0x0000_0140,
            0x0001_0004,
            0x1111_1111,
            0x1111_1111,
        ];
        let sprite = [0x7c80_8080, 0x0000_0000, 0x7800_0000];
        feed(&mut recorder, &mut gpu, &upload);
        feed(&mut recorder, &mut gpu, &sprite);
        feed(
            &mut recorder,
            &mut gpu,
            &[
                0xa000_0000,
                0x0000_0140,
                0x0001_0004,
                0x2222_2222,
                0x2222_2222,
            ],
        );
        feed(
            &mut recorder,
            &mut gpu,
            &[0x7c80_8080, 0x0000_0020, 0x7800_0000],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(
            kinds(&frame),
            ["mirror", "draw", "mirror", "draw"],
            "{:?}",
            frame.ops
        );
    }

    #[test]
    fn overlapping_copies_are_taken_from_the_software_vram() {
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // A copy to the right of its source smears; one to the left does not.
        feed(
            &mut recorder,
            &mut gpu,
            &[0x8000_0000, 0x0000_0000, 0x0000_0004, 0x0001_0008],
        );
        assert_eq!(recorder.stats().captures, 0, "taken at the next word");
        feed(
            &mut recorder,
            &mut gpu,
            &[0x8000_0000, 0x0000_0004, 0x0000_0000, 0x0001_0008],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(recorder.stats().captures, 1);
        assert_eq!(recorder.stats().copies, 2);
        assert!(frame.ops.contains(&Op::Mirror {
            rect: Rect::new(4, 0, 8, 1),
            offset: 0,
            pitch: 128,
        }));
        let kinds: Vec<u32> = (0..frame.vertex_count())
            .step_by(6)
            .map(|v| vertex(&frame, v)[5] & 7)
            .collect();
        assert_eq!(kinds, [mode::KIND_MIRROR, mode::KIND_COPY]);
    }

    #[test]
    fn exact_polygons_carry_the_software_set_up() {
        let (mut recorder, mut gpu) = pair(1);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // Gouraud triangle (0,0) red, (10,0) green, (0,10) blue.
        let triangle = [
            0x3000_00ff,
            0x0000_0000,
            0x0000_ff00,
            0x0000_000a,
            0x00ff_0000,
            0x000a_0000,
        ];
        feed(&mut recorder, &mut gpu, &triangle);
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(frame.vertex_count(), 6, "a bounding box at 1x");
        assert_eq!(position(&frame, 0), (-0.5, -0.5));
        assert_eq!(position(&frame, 4), (10.5, 9.5));
        let v = vertex(&frame, 0);
        assert_eq!(
            v[5] & (7 | mode::GOURAUD | mode::EXACT),
            mode::GOURAUD | mode::EXACT
        );
        // Sorted vertices and the long edge on the left.
        assert_eq!(&v[10..13], &[0, 10, 10 << 16]);
        assert_eq!(v[8] & 1, 1);
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        feed(&mut recorder, &mut gpu, &triangle);
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(frame.vertex_count(), 3, "the triangle itself above 1x");
        assert_eq!(position(&frame, 1), (10.0, 0.0));
        assert_eq!(vertex(&frame, 1)[4], 0xff00);
    }

    #[test]
    fn a_folded_quad_draws_its_second_half_over_the_first() {
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // Vertices 0 and 3 on the same side of 1-2: the halves overlap.
        feed(
            &mut recorder,
            &mut gpu,
            &[
                0x2a00_8080,
                0x0000_0000,
                0x0000_0020,
                0x0020_0000,
                0x0008_0008,
            ],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["draw", "sync", "draw"]);
    }

    #[test]
    fn a_quad_is_one_primitive() {
        let (mut recorder, mut gpu) = pair(2);
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        // A semi-transparent flat quad: its two triangles share a layer.
        feed(
            &mut recorder,
            &mut gpu,
            &[
                0x2a00_8080,
                0x0000_0000,
                0x0000_0020,
                0x0020_0000,
                0x0020_0020,
            ],
        );
        let frame = recorder.take_frame(gpu.vram());
        assert_eq!(kinds(&frame), ["draw"]);
        assert_eq!(frame.vertex_count(), 6);
        assert_eq!(recorder.stats().triangles, 2);
    }

    struct Offset(f32);
    impl VertexPrecision for Offset {
        fn precise_vertex(&self, address: u32, _word: u32) -> Option<[f32; 3]> {
            // Only the vertex words of the packet at 0x80100004 (the second
            // and third words of a flat triangle) are known.
            (0x8010_0008..0x8010_0010).contains(&address).then_some([
                self.0 + (address - 0x8010_0008) as f32 * 2.5,
                self.0,
                1.0,
            ])
        }
    }

    #[test]
    fn precision_hook_moves_vertices_above_1x() {
        for scale in [1, 2] {
            let (mut recorder, mut gpu) = pair(scale);
            recorder.precision = Some(Rc::new(Offset(0.25)));
            feed(
                &mut recorder,
                &mut gpu,
                &[0xe300_0000, 0xe407_ffff, 0xe500_1005],
            );
            // Flat triangle: words 1..3 at 0x80100008.. (after the command).
            for (i, word) in [0x2000_00ff, 0x0000_0000, 0x0000_000a, 0x000a_0000]
                .into_iter()
                .enumerate()
            {
                recorder.gp0(word, 0x8010_0004 + 4 * i as u32, gpu.vram());
                gpu.write_gp0(word).unwrap();
            }
            let frame = recorder.take_frame(gpu.vram());
            if scale == 1 {
                // 1x draws the software GPU's integer triangle.
                assert_eq!(position(&frame, 0), (4.5, 1.5));
                continue;
            }
            // Offset (5, 2); the word at 0x80100008 holds (0, 0): precise
            // (0.25, 0.25); 0x8010000C holds (10, 0): precise x 0.25 + 10.
            assert_eq!(position(&frame, 0), (5.25, 2.25));
            assert_eq!(position(&frame, 1), (5.0 + 0.25 + 10.0, 2.25));
            // The third vertex (0x80100010) has no precise position.
            assert_eq!(position(&frame, 2), (5.0, 12.0));
        }
    }

    /// Positions as the words give them, with the depth `.0(x, y)` (no
    /// position at all where it is negative).
    struct Depth(fn(i32, i32) -> f32);
    impl VertexPrecision for Depth {
        fn precise_vertex(&self, _address: u32, word: u32) -> Option<[f32; 3]> {
            let (x, y) = (sign_extend_11(word), sign_extend_11(word >> 16));
            let z = (self.0)(x, y);
            (z >= 0.0).then_some([x as f32, y as f32, z])
        }
    }

    /// One polygon `words` recorded at `scale` with `depth` and perspective
    /// `on`: its vertices' modes and w.
    fn perspective_run(scale: u32, depth: Depth, on: bool, words: &[u32]) -> (Frame, Stats) {
        let (mut recorder, mut gpu) = pair(scale);
        recorder.precision = Some(Rc::new(depth));
        recorder.perspective = on;
        feed(&mut recorder, &mut gpu, &FULL_AREA);
        feed(&mut recorder, &mut gpu, words);
        let frame = recorder.take_frame(gpu.vram());
        (frame, recorder.stats().clone())
    }

    fn w(frame: &Frame, index: usize) -> f32 {
        f32::from_bits(vertex(frame, index)[VERTEX_WORDS - 1])
    }

    /// A raw-textured triangle (POLY_FT3) at (0, 0), (32, 0), (0, 32).
    const TEXTURED_TRIANGLE: [u32; 7] = [
        0x2580_8080,
        0x0000_0000,
        0x0000_0000,
        0x0000_0020,
        0x0005_001f,
        0x0020_0000,
        0x1f00,
    ];

    #[test]
    fn perspective_needs_a_depth_at_every_vertex() {
        let receding = Depth(|_, y| 100.0 + y as f32);
        let (on, stats) = perspective_run(2, receding, true, &TEXTURED_TRIANGLE);
        assert_eq!(on.vertex_count(), 3);
        assert_eq!(stats.perspective_triangles, 1);
        for (index, z) in [100.0, 100.0, 132.0].into_iter().enumerate() {
            assert_ne!(vertex(&on, index)[5] & mode::PERSPECTIVE, 0);
            assert_eq!(w(&on, index), z, "vertex {index}");
        }
        // Off: the same vertices, word for word, but the bit and w.
        let (off, stats) =
            perspective_run(2, Depth(|_, y| 100.0 + y as f32), false, &TEXTURED_TRIANGLE);
        assert_eq!(stats.perspective_triangles, 0);
        for index in 0..3 {
            let (a, b) = (vertex(&on, index), vertex(&off, index));
            assert_eq!(a[5] & !mode::PERSPECTIVE, b[5]);
            assert_eq!(a[..5], b[..5]);
            assert_eq!(a[6..VERTEX_WORDS - 1], b[6..VERTEX_WORDS - 1]);
            assert_eq!(w(&off, index), 1.0);
        }
        // Not without a depth at every vertex, nor with equal depths (the
        // affine mapping exactly), nor at 1x, nor untextured.
        let cases: [(u32, Depth, &[u32], &str); 6] = [
            (
                2,
                Depth(|x, y| if x == 32 { 0.0 } else { 100.0 + y as f32 }),
                &TEXTURED_TRIANGLE,
                "a zero depth",
            ),
            (
                2,
                Depth(|x, y| if x == 32 { -1.0 } else { 100.0 + y as f32 }),
                &TEXTURED_TRIANGLE,
                "no position",
            ),
            (2, Depth(|_, _| 500.0), &TEXTURED_TRIANGLE, "equal depths"),
            (
                2,
                Depth(|_, y| if y == 32 { f32::INFINITY } else { 1.0 }),
                &TEXTURED_TRIANGLE,
                "infinite",
            ),
            (1, Depth(|_, y| 100.0 + y as f32), &TEXTURED_TRIANGLE, "1x"),
            (
                2,
                Depth(|_, y| 100.0 + y as f32),
                &[0x2000_00ff, 0, 0x20, 0x0020_0000],
                "untextured",
            ),
        ];
        for (scale, depth, words, case) in cases {
            let (frame, stats) = perspective_run(scale, depth, true, words);
            assert_eq!(stats.perspective_triangles, 0, "{case}");
            for index in 0..frame.vertex_count() {
                assert_eq!(vertex(&frame, index)[5] & mode::PERSPECTIVE, 0, "{case}");
                assert_eq!(w(&frame, index), 1.0, "{case}");
            }
        }
    }

    #[test]
    fn a_quad_decides_perspective_for_both_halves() {
        // POLY_FT4 (raw) at (0, 0), (32, 0), (0, 32), (32, 32).
        let quad = [
            0x2d80_8080,
            0x0000_0000,
            0x0000_0000,
            0x0000_0020,
            0x0005_001f,
            0x0020_0000,
            0x1f00,
            0x0020_0020,
            0x1f1f,
        ];
        let (frame, stats) =
            perspective_run(4, Depth(|x, y| 50.0 + (x + 2 * y) as f32), true, &quad);
        assert_eq!((frame.vertex_count(), stats.perspective_triangles), (6, 2));
        // Triangles (1, 3, 2) and (0, 1, 2): the shared vertices 1 and 2 have
        // the same w in both.
        let ws: Vec<f32> = (0..6).map(|index| w(&frame, index)).collect();
        assert_eq!(ws, [82.0, 146.0, 114.0, 50.0, 82.0, 114.0]);
        // Three depths of four: neither half (no seam along the diagonal).
        let (frame, stats) = perspective_run(
            4,
            Depth(|x, y| {
                if (x, y) == (32, 32) {
                    0.0
                } else {
                    50.0 + (x + 2 * y) as f32
                }
            }),
            true,
            &quad,
        );
        assert_eq!((frame.vertex_count(), stats.perspective_triangles), (6, 0));
        assert!((0..6).all(|index| vertex(&frame, index)[5] & mode::PERSPECTIVE == 0));
    }

    #[test]
    fn recorder_reads_the_software_state() {
        let mut gpu = Gpu::new(Vram::new());
        let mut decoder = Recorder::new(Options::default(), &gpu);
        // Every state word, a textured polygon (its page sets the texture
        // base and depth), and a command left half received.
        let words = [
            0xe100_0629u32,
            0xe200_4a1f,
            0xe300_2c0b,
            0xe404_e9f3,
            0xe500_9004,
            0xe600_0003,
            0x2c80_8080,
            0x0000_0000,
            0x7a00_0102,
            0x0000_0010,
            0x0163_0405,
            0x0010_0000,
            0x0000_0506,
            0x0010_0010,
            0x0000_0707,
            0x6000_00ff,
        ];
        for word in words {
            decoder.gp0(word, 0, gpu.vram());
            gpu.write_gp0(word).unwrap();
        }
        let mut seeded = Recorder::new(Options::default(), &gpu);
        assert_eq!(seeded.state, decoder.state);
        assert_eq!(seeded.state.tex_base, [3 * 64, 0]);
        assert_eq!(seeded.state.depth, 2);
        assert_eq!(seeded.state.abr, 3);
        assert!(seeded.state.set_mask && seeded.state.check_mask);
        assert_eq!(seeded.fifo, [0x6000_00ff]);
        // The tile completes (outside the area), then a transfer in progress
        // continues after a re-seed.
        for word in [
            0x0000_0000,
            0x0001_0001,
            0xa000_0000,
            0x0000_0000,
            0x0001_0004,
            0x2222_1111,
        ] {
            seeded.gp0(word, 0, gpu.vram());
            gpu.write_gp0(word).unwrap();
        }
        seeded.reseed(&gpu);
        assert!(seeded.upload.is_some());
        seeded.gp0(0x4444_3333, 0, gpu.vram());
        let frame = seeded.take_frame(gpu.vram());
        assert!(seeded.upload.is_none());
        assert!(frame.ops.contains(&Op::Mirror {
            rect: Rect::new(0, 0, 4, 1),
            offset: 1024 * 512,
            pitch: 128,
        }));
    }

    #[test]
    fn texture_rects_follow_linear_addressing() {
        let texture = Texture {
            depth: 1,
            base: [960, 256],
            clut: [1008, 500],
            u: (0, 255),
            v: (10, 20),
        };
        let (texels, clut) = texture.rects([0, 0], [0, 0]);
        // 8-bit: 128 halfwords from x 960 run 64 past the edge.
        assert_eq!(
            texels,
            [Rect::new(960, 266, 64, 11), Rect::new(0, 267, 64, 11)]
        );
        assert_eq!(
            clut,
            [Rect::new(1008, 500, 16, 1), Rect::new(0, 501, 240, 1)]
        );
        // A window keeps the coordinates inside it.
        let windowed = Texture {
            depth: 2,
            base: [0, 0],
            clut: [0, 0],
            u: (0, 255),
            v: (0, 255),
        };
        let (texels, clut) = windowed.rects([0xe0, 0], [0x40, 0]);
        assert_eq!(texels, [Rect::new(0x40, 0, 32, 256)]);
        assert!(clut.is_empty());
        assert_eq!(axis_ranges(250, 260, 0, 0), [(0, 4), (250, 255)]);
    }

    #[test]
    fn replaced_primitives_name_their_slot_and_upload_origin() {
        // A 4-bit 16x16-texel texture (4x16 words) at (324, 0): page 5
        // (x 320), texture coordinates u 16..31.
        let pixels: Vec<u16> = (0..64u16).map(|i| 0x1111 * (i % 15 + 1)).collect();
        let texture = crate::asset_dump::key::texture_hash_words(4, 16, &pixels);
        let dir = std::env::temp_dir().join(format!("ac3-record-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 32, 32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255; 32 * 32 * 4]).unwrap();
            writer.finish().unwrap();
        }
        std::fs::write(dir.join(format!("ac3tex-p4-{texture}-any-16x16.png")), png).unwrap();
        for scale in [1, 2] {
            let (mut recorder, mut gpu) = pair(scale);
            recorder.set_texture_pack(Some(TexturePack::open(&dir).unwrap()), gpu.vram());
            feed(&mut recorder, &mut gpu, &FULL_AREA);
            let mut upload = vec![0xa000_0000, 0x0000_0144, 0x0010_0004];
            upload.extend(
                pixels
                    .chunks(2)
                    .map(|pair| u32::from(pair[0]) | (u32::from(pair[1]) << 16)),
            );
            feed(&mut recorder, &mut gpu, &upload);
            let clut = 480u32 << 22;
            feed(&mut recorder, &mut gpu, &[0xe100_0005]);
            // A 16x16 sprite of the texture; one starting a texel before it;
            // one with a texture window.
            feed(
                &mut recorder,
                &mut gpu,
                &[0x7c80_8080, 0x012c_0000, clut | 0x10],
            );
            feed(
                &mut recorder,
                &mut gpu,
                &[0x7c80_8080, 0x012c_0020, clut | 0x0f],
            );
            feed(&mut recorder, &mut gpu, &[0xe200_0001]);
            feed(
                &mut recorder,
                &mut gpu,
                &[0x7c80_8080, 0x012c_0040, clut | 0x10],
            );
            feed(&mut recorder, &mut gpu, &[0xe200_0000]);
            // A textured quad over all of it.
            feed(
                &mut recorder,
                &mut gpu,
                &[
                    0x2c80_8080,
                    0x0140_0000,
                    clut | 0x0010,
                    0x0140_0010,
                    0x0005_001f,
                    0x0150_0000,
                    0x0f10,
                    0x0150_0010,
                    0x0f1f,
                ],
            );
            let frame = recorder.take_frame(gpu.vram());
            let textured: Vec<(u32, u32)> = (0..frame.vertex_count())
                .map(|v| vertex(&frame, v))
                .filter(|v| v[5] & mode::TEXTURED != 0)
                .map(|v| (v[5] >> mode::SLOT_SHIFT, v[7]))
                .collect();
            let per_quad = if scale == 1 { 12 } else { 6 };
            let (sprites, quad) = textured.split_at(18);
            // Slot 0 (+1) and the upload's first texel, u 16 in the page.
            assert!(sprites[..6].iter().all(|&v| v == (1, 16)), "{sprites:?}");
            assert!(sprites[6..12].iter().all(|&v| v == (0, 0xffff)));
            assert!(sprites[12..].iter().all(|&v| v == (0, 0xfff7)));
            assert_eq!(quad.len(), per_quad);
            assert!(quad.iter().all(|&v| v == (1, 16)), "{quad:?}");
            assert_eq!(frame.replacements.len(), 1);
            let image = &frame.replacements[0];
            assert_eq!((image.slot, image.original), (0, [16, 16]));
            assert_eq!((image.image.width, image.image.height), (32, 32));
            let stats = &recorder.replacer().unwrap().stats;
            assert_eq!((stats.replaced, stats.originals, stats.windowed), (2, 1, 1));
            // Drawn over, the texture is not replaced any more.
            feed(
                &mut recorder,
                &mut gpu,
                &[0x6000_0000, 0x0000_0144, 0x0001_0001],
            );
            feed(
                &mut recorder,
                &mut gpu,
                &[0x7c80_8080, 0x012c_0000, clut | 0x10],
            );
            let frame = recorder.take_frame(gpu.vram());
            assert!(frame.replacements.is_empty());
            let sprite = vertex(&frame, frame.vertex_count() - 1);
            assert_eq!(sprite[5] >> mode::SLOT_SHIFT, 0);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Movie images that are always decoded: 32x16 pixels of one grey.
    struct Frames;
    impl MovieImages for Frames {
        fn image(
            &self,
            _: &std::sync::Arc<str>,
            file: u32,
        ) -> Option<std::sync::Arc<crate::movie_pack::RgbaImage>> {
            Some(std::sync::Arc::new(crate::movie_pack::RgbaImage {
                width: 32,
                height: 16,
                pixels: vec![file as u8; 32 * 16 * 4],
            }))
        }
    }

    #[test]
    fn movie_frames_are_drawn_from_movie_slots() {
        use super::super::replace::{FIRST_MOVIE_SLOT, MovieFrame};
        // A 15-bit 16x8 frame at (832, 256) in two 8-pixel slices, and a
        // raw sprite of it (texture page 13, y 256, 15-bit).
        let slices: Vec<u32> = [832u32, 840]
            .iter()
            .flat_map(|&x| {
                let mut words = vec![0xa000_0000, x | (256 << 16), 8 | (8 << 16)];
                words.extend((0..32).map(|i| 0x8421_8421 + i));
                words
            })
            .collect();
        let sprite = [0xe100_011d, 0x6580_8080, 8 | (264 << 16), 0, 16 | (8 << 16)];
        let frame = MovieFrame {
            rect: Rect::new(832, 256, 16, 8),
            key: "movie_0810".into(),
            file: 7,
            stp: true,
        };
        let textured = |frame: &Frame| -> Vec<u32> {
            (0..frame.vertex_count())
                .map(|v| vertex(frame, v))
                .filter(|v| v[5] & mode::TEXTURED != 0)
                .map(|v| v[5] >> mode::SLOT_SHIFT)
                .collect()
        };
        for scale in [1, 2] {
            let (mut plain, mut gpu) = pair(scale);
            let (mut movies, mut movie_gpu) = pair(scale);
            movies.set_movie_images(Some(Box::new(Frames)), movie_gpu.vram());
            // Until a frame is handed over, exactly the frame without them.
            for (recorder, gpu) in [(&mut plain, &mut gpu), (&mut movies, &mut movie_gpu)] {
                feed(recorder, gpu, &FULL_AREA);
                feed(recorder, gpu, &slices);
                feed(recorder, gpu, &sprite);
            }
            let (a, b) = (
                plain.take_frame(gpu.vram()),
                movies.take_frame(movie_gpu.vram()),
            );
            assert_eq!(
                (&a.ops, &a.vertices, &a.data),
                (&b.ops, &b.vertices, &b.data)
            );
            assert!(b.replacements.is_empty());
            // The frame's slices, then the frame: the sprite draws its image.
            feed(&mut movies, &mut movie_gpu, &slices);
            movies.movie_frame(frame.clone());
            feed(&mut movies, &mut movie_gpu, &sprite);
            let b = movies.take_frame(movie_gpu.vram());
            let slots = textured(&b);
            assert!(!slots.is_empty());
            assert!(
                slots.iter().all(|&s| s == FIRST_MOVIE_SLOT + 1),
                "{slots:?}"
            );
            assert_eq!(b.replacements.len(), 1);
            assert_eq!(b.replacements[0].original, [16, 8]);
            // Grey 7 of 255 is 1 of 31, with the frame's STP bit.
            assert_eq!(b.replacements[0].image.texels[0], 0x8421);
            // A texture pack set (or none) keeps the movie frames' images;
            // a fill over a texel puts the game's back under the sprite.
            movies.set_texture_pack(None, movie_gpu.vram());
            assert!(movies.replacer().unwrap().has_movie_images());
            feed(&mut movies, &mut movie_gpu, &slices);
            movies.movie_frame(frame.clone());
            feed(
                &mut movies,
                &mut movie_gpu,
                &[0x0200_0000, 832 | (256 << 16), 16 | (1 << 16)],
            );
            feed(&mut movies, &mut movie_gpu, &sprite);
            let b = movies.take_frame(movie_gpu.vram());
            assert!(textured(&b).iter().all(|&s| s == 0));
        }
    }

    // ---- effects frames ----

    /// Machine RAM in flight (`fx::scene` tests' words) with the flight
    /// list of `packets` ((ordering-table entry, GP0 words); see
    /// `fx::walk` tests); returns it and the list's start.
    fn flight(packets: &[(u32, Vec<u32>)]) -> (Vec<u8>, u32) {
        let mut ram = fx::scene::tests::flight_ram();
        let start = fx::walk::tests::flight_list_in(&mut ram, packets);
        (ram, start)
    }

    /// The DMA list at `start` through the recorder (its effects first) and
    /// the software GPU, as the host does.
    fn submit(recorder: &mut Recorder, gpu: &mut Gpu, ram: &[u8], start: u32) {
        recorder.list_begin(ram, start, gpu.vram(), false);
        for (address, words) in crate::flight_host::walk_list_addressed(ram, start).unwrap() {
            for (i, &word) in words.iter().enumerate() {
                recorder.gp0(word, address + 4 * (i as u32 + 1), gpu.vram());
                gpu.write_gp0(word).unwrap();
            }
        }
        recorder.list_end(gpu.vram());
    }

    fn effects_on() -> fx::Config {
        fx::Config {
            enabled: true,
            ..fx::Config::default()
        }
    }

    /// A tile (GP0 60, or 62 semi-transparent) of `w` x `h` at `x`, `y`.
    fn tile(semi: bool, color: u32, x: u32, y: u32, w: u32, h: u32) -> Vec<u32> {
        let op = if semi { 0x6200_0000 } else { 0x6000_0000 };
        vec![op | color, x | (y << 16), w | (h << 16)]
    }

    /// A flight frame: the sky over the area, opaque terrain A, a
    /// semi-transparent puff B over it, opaque C over part of B, the flare
    /// and the HUD.
    fn flight_frame() -> Vec<(u32, Vec<u32>)> {
        let mut sky = FULL_AREA.to_vec();
        sky.extend(tile(false, 0x80_4020, 0, 0, 320, 240));
        vec![
            (4095, sky),
            (4094, tile(false, 0x00_8000, 10, 10, 16, 16)),
            (3000, tile(true, 0xff_00ff, 14, 14, 16, 16)),
            (2000, tile(false, 0x00_00ff, 20, 20, 8, 8)),
            (1, tile(true, 0x40_4040, 100, 100, 4, 4)),
            (0, tile(false, 0xff_ffff, 0, 230, 320, 2)),
        ]
    }

    /// The draw orders of a frame's vertices in `op`.
    fn orders(frame: &Frame, op: &Op) -> Vec<u32> {
        let (first, count) = match op {
            Op::FxDraw { first, count, .. } | Op::Draw { first, count, .. } => (*first, *count),
            _ => return Vec::new(),
        };
        (first..first + count)
            .map(|v| frame.fx_vertices[v as usize * fx::FX_VERTEX_WORDS + 1])
            .collect()
    }

    #[test]
    fn an_effects_frame_clears_z_draws_the_world_then_the_deferred() {
        let (ram, start) = flight(&flight_frame());
        let (mut recorder, mut gpu) = pair(2);
        recorder.set_effects(Some(effects_on()));
        submit(&mut recorder, &mut gpu, &ram, start);
        let frame = recorder.take_frame(gpu.vram());
        let kinds = kinds(&frame);
        // The sky; the z clear and world begin; the world (A, then C after
        // B's would-be layer); the post; B, deferred; the flare and HUD.
        let at = |kind: &str| kinds.iter().position(|k| *k == kind).unwrap();
        assert!(at("zclear") < at("begin"), "{kinds:?}");
        assert!(at("begin") < at("world"), "{kinds:?}");
        assert!(at("world") < at("post"), "{kinds:?}");
        assert!(at("post") < at("deferred"), "{kinds:?}");
        assert_eq!(kinds.iter().filter(|k| **k == "deferred").count(), 1);
        assert!(
            kinds[at("deferred")..].contains(&"draw"),
            "the flare and HUD after"
        );
        assert!(!kinds.contains(&"late"), "no late step unless asked");
        // With an effect working late: its step after the deferred ones,
        // before the flare and HUD.
        let (mut late_recorder, mut late_gpu) = pair(2);
        late_recorder.set_effects(Some(effects_on()));
        late_recorder.add_effect(Box::new(LateProbe));
        submit(&mut late_recorder, &mut late_gpu, &ram, start);
        let late_frame = late_recorder.take_frame(late_gpu.vram());
        let late = self::kinds(&late_frame);
        let at_late = |kind: &str| late.iter().position(|k| *k == kind).unwrap();
        assert!(at_late("deferred") < at_late("late"), "{late:?}");
        assert_eq!(late.iter().filter(|k| **k == "late").count(), 1);
        assert!(late[at_late("late")..].contains(&"draw"), "{late:?}");
        let area = late_frame
            .fx
            .as_deref()
            .expect("an effects frame")
            .world_area;
        assert!(late_frame.ops.contains(&Op::FxLate(area)));
        // The z clear, the post and the world's area: the draw area.
        let fx = frame.fx.as_deref().expect("an effects frame");
        assert_eq!(fx.world_area, Rect::new(0, 0, 1024, 512));
        assert!(frame.ops.contains(&Op::FxPost(fx.world_area)));
        let markers: Vec<(Marker, u32)> = fx.markers.clone();
        assert_eq!(
            markers,
            [
                (Marker::Sky, 0x8010_3ffc),
                (Marker::WorldBegin, 0x8010_3ff8),
                (Marker::WorldEnd, 0x8010_0004),
                (Marker::Hud, 0x8010_0000),
            ]
        );
        assert_eq!(fx.scene.ots, [0x8010_0000, 0x8010_4000]);
        // Draw orders: A 1, B 2 (deferred), C 3; the sky, flare and HUD 0.
        assert_eq!(frame.fx_vertices.len(), frame.vertex_count() * 2);
        assert_eq!(fx.classes, [Class::Sprite as u8; 3]);
        assert_eq!(fx.slots, [4094, 3000, 2000]);
        let world: Vec<Vec<u32>> = frame
            .ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    Op::FxDraw {
                        pipe: Pipe::World,
                        ..
                    }
                )
            })
            .map(|op| orders(&frame, op))
            .collect();
        assert_eq!(world.concat(), [[1; 6], [3; 6]].concat());
        let deferred = frame
            .ops
            .iter()
            .find(|op| {
                matches!(
                    op,
                    Op::FxDraw {
                        pipe: Pipe::Deferred,
                        ..
                    }
                )
            })
            .unwrap();
        assert_eq!(orders(&frame, deferred), [2; 6]);
        for op in &frame.ops {
            if matches!(op, Op::Draw { .. }) {
                assert!(orders(&frame, op).iter().all(|&o| o == 0));
            }
        }
        let stats = recorder.effects_stats().unwrap();
        assert_eq!(
            (
                stats.frames,
                stats.deferred,
                stats.replayed,
                stats.flushes,
                stats.posts
            ),
            (1, 1, 1, 0, 1)
        );
        assert_eq!(stats.markers, [1; 4]);
        // The next pass is no effects frame.
        let frame = recorder.take_frame(gpu.vram());
        assert!(frame.fx.is_none() && frame.fx_vertices.is_empty());
    }

    #[test]
    fn effects_off_record_exactly_as_before() {
        let (ram, start) = flight(&flight_frame());
        let record = |config: Option<fx::Config>, ram: &[u8]| {
            let (mut recorder, mut gpu) = pair(2);
            recorder.set_effects(config);
            submit(&mut recorder, &mut gpu, ram, start);
            let frame = recorder.take_frame(gpu.vram());
            (frame, recorder.effects_stats().cloned())
        };
        let (before, none) = record(None, &ram);
        assert!(none.is_none());
        assert!(before.fx.is_none() && before.fx_vertices.is_empty());
        let mut hub = ram.clone();
        let context = 0x800b_ce60u32 + 0x2c;
        hub[(context & 0x1f_ffff) as usize] = 3;
        for (config, ram, case) in [
            (Some(fx::Config::default()), &ram, "configured, off"),
            (Some(effects_on()), &hub, "on, not in flight"),
        ] {
            let (frame, stats) = record(config, ram);
            assert_eq!(frame.ops, before.ops, "{case}");
            assert_eq!(frame.vertices, before.vertices, "{case}");
            assert_eq!(frame.data, before.data, "{case}");
            assert!(frame.fx.is_none() && frame.fx_vertices.is_empty(), "{case}");
            assert_eq!(stats.unwrap().frames, 0, "{case}");
        }
        // A 24-bit display (a movie) arms nothing either.
        let (mut recorder, gpu) = pair(2);
        recorder.set_effects(Some(effects_on()));
        recorder.list_begin(&ram, start, gpu.vram(), true);
        recorder.list_end(gpu.vram());
        assert_eq!(recorder.effects_stats().unwrap().movie, 1);
        // At 1x effects are never configured.
        let (mut recorder, _) = pair(1);
        recorder.set_effects(Some(effects_on()));
        assert!(recorder.effects_config().is_none());
    }

    #[test]
    fn reading_a_deferred_primitives_pixels_draws_it_early() {
        // B semi-transparent, then D semi-transparent over B in another draw
        // area (not the world's: drawn in place), which reads B's pixels:
        // B is drawn first, after a depth snapshot. Then E with the mask
        // check, deferred too.
        let mut frame = flight_frame();
        frame[2]
            .1
            .extend([0xe300_0000, 0xe400_0000 | 100 | (100 << 10)]);
        frame[2].1.extend(tile(true, 0x00_ff00, 16, 16, 4, 4));
        frame[2].1.extend(FULL_AREA);
        frame[2].1.extend([0xe600_0002]);
        frame[2].1.extend(tile(true, 0x00_ff00, 40, 40, 4, 4));
        frame[2].1.extend([0xe600_0000]);
        let (ram, start) = flight(&frame);
        let (mut recorder, mut gpu) = pair(2);
        recorder.set_effects(Some(effects_on()));
        submit(&mut recorder, &mut gpu, &ram, start);
        let frame = recorder.take_frame(gpu.vram());
        let kinds = kinds(&frame);
        let at = |kind: &str| kinds.iter().position(|k| *k == kind).unwrap();
        assert!(at("snapshot") < at("deferred"), "{kinds:?}");
        assert!(
            at("deferred") < at("post"),
            "B before D and the post: {kinds:?}"
        );
        assert_eq!(kinds.iter().filter(|k| **k == "deferred").count(), 2);
        let stats = recorder.effects_stats().unwrap();
        assert_eq!(
            (
                stats.deferred,
                stats.flushes,
                stats.replayed_early,
                stats.deferred_masked
            ),
            (2, 1, 1, 1)
        );
    }

    /// A palette redirect: world-phase palettes of row 256 read row 384.
    fn row_384(word: u16, entries: u16, phase: crate::effects::atmosphere::Phase) -> Option<u16> {
        use crate::effects::atmosphere::Phase;
        (phase == Phase::World && entries == 16 && (word >> 6) == 256)
            .then_some((word & 0x3f) | (384 << 6))
    }

    /// An effect that drops red tiles and counts phases.
    struct Probe {
        phases: Vec<Phase>,
        kicked: bool,
    }

    /// An effect whose card side works at the late step.
    struct LateProbe;
    impl fx::RecordEffect for LateProbe {
        fn name(&self) -> &'static str {
            "late"
        }
        fn late(&self) -> bool {
            true
        }
    }
    impl fx::RecordEffect for Probe {
        fn name(&self) -> &'static str {
            "probe"
        }
        fn kick(&mut self, scene: &fx::Scene, _: &[u8], _: &Vram) {
            self.kicked = scene.h == 0xc0;
        }
        fn phase(&mut self, phase: Phase, _: &fx::Scene) {
            self.phases.push(phase);
        }
        fn primitive(&mut self, primitive: &fx::Primitive<'_>) -> Route {
            if primitive.command & 0xfc == 0x60 && primitive.words[0] & 0xff_ffff == 0x00_00ff {
                Route::Drop
            } else {
                Route::Draw
            }
        }
        fn take(&mut self) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
            Some(std::sync::Arc::new((self.phases.len(), self.kicked)))
        }
    }

    #[test]
    fn effects_see_phases_drop_primitives_and_redirect_palettes() {
        let mut frame = flight_frame();
        // A 4-bit sprite of page 5 with the palette at (320, 256).
        let clut = (320 / 16) | (256 << 6);
        frame[1]
            .1
            .extend([0xe100_0005, 0x7c80_8080, 0x0040_0040, clut << 16]);
        let (ram, start) = flight(&frame);
        let (mut recorder, mut gpu) = pair(2);
        recorder.set_effects(Some(effects_on()));
        recorder.add_effect(Box::new(Probe {
            phases: Vec::new(),
            kicked: false,
        }));
        recorder.clut_redirect = Some(row_384);
        submit(&mut recorder, &mut gpu, &ram, start);
        let frame = recorder.take_frame(gpu.vram());
        let stats = recorder.effects_stats().unwrap();
        assert_eq!((stats.dropped, stats.redirected), (1, 1));
        // C (the red tile) is gone; the sprite decodes with row 384.
        let textured: Vec<u32> = (0..frame.vertex_count())
            .map(|v| vertex(&frame, v))
            .filter(|v| v[5] & mode::TEXTURED != 0)
            .map(|v| v[6])
            .collect();
        assert_eq!(textured.len(), 6);
        assert!(textured.iter().all(|&tex| (tex >> 13) & 0x1ff == 384));
        assert!(
            !(0..frame.vertex_count()).any(|v| vertex(&frame, v)[4] == 0x00_00ff),
            "dropped"
        );
        let fx = frame.fx.as_deref().unwrap();
        assert_eq!(fx.data.len(), 1);
        assert_eq!(fx.data[0].0, "probe");
        assert_eq!(
            fx.data[0].1.downcast_ref::<(usize, bool)>(),
            Some(&(4, true)),
            "four phases, and the kick's scene"
        );
        // The sky slot's drawing command, as the game sent it.
        assert_eq!(fx.sky.len(), 1);
        assert_eq!(fx.sky[0].words, tile(false, 0x80_4020, 0, 0, 320, 240));
        // Outside an effects frame nothing is redirected.
        assert_eq!(recorder.slot(), None);
        feed(
            &mut recorder,
            &mut gpu,
            &[0xe100_0005, 0x7c80_8080, 0x0040_0040, clut << 16],
        );
        let frame = recorder.take_frame(gpu.vram());
        let tex = vertex(&frame, frame.vertex_count() - 1)[6];
        assert_eq!((tex >> 13) & 0x1ff, 256);
        assert_eq!(recorder.effects_stats().unwrap().redirected, 1);
    }

    #[test]
    fn the_walk_tracks_the_slot_of_each_packet() {
        let (ram, start) = flight(&flight_frame());
        let (mut recorder, mut gpu) = pair(2);
        recorder.set_effects(Some(effects_on()));
        recorder.list_begin(&ram, start, gpu.vram(), false);
        let mut slots = Vec::new();
        for (address, words) in crate::flight_host::walk_list_addressed(&ram, start).unwrap() {
            for (i, &word) in words.iter().enumerate() {
                recorder.gp0(word, address + 4 * (i as u32 + 1), gpu.vram());
                gpu.write_gp0(word).unwrap();
                if i == 0 {
                    slots.push(recorder.slot());
                }
            }
        }
        recorder.list_end(gpu.vram());
        assert_eq!(
            slots,
            [
                Some(4095),
                Some(4094),
                Some(3000),
                Some(2000),
                Some(1),
                Some(0)
            ]
        );
        assert_eq!(recorder.slot(), None, "after the list");
    }

    #[test]
    fn accesses_test_exact_rectangles() {
        let mut accesses = Accesses::new();
        // Two glyph-sized writes side by side in one tile do not overlap.
        accesses.add(&Rect::new(0, 0, 5, 8), 0, true);
        assert_eq!(accesses.latest(&Rect::new(5, 0, 5, 8), Kind::Any), None);
        assert_eq!(
            accesses.latest(&Rect::new(4, 7, 5, 8), Kind::Writes),
            Some(0)
        );
        accesses.add(&Rect::new(5, 0, 5, 8), 3, false);
        assert_eq!(
            accesses.latest(&Rect::new(0, 0, 16, 16), Kind::Any),
            Some(3)
        );
        assert_eq!(
            accesses.latest(&Rect::new(0, 0, 16, 16), Kind::Writes),
            Some(0)
        );
        assert_eq!(
            accesses.latest(&Rect::new(0, 0, 16, 16), Kind::Reads),
            Some(3)
        );
        // A write covering the tile replaces what was there.
        accesses.add(&Rect::new(0, 0, 32, 16), 4, true);
        assert_eq!(accesses.tiles[0].len(), 1);
        assert_eq!(accesses.latest(&Rect::new(1, 1, 1, 1), Kind::Reads), None);
        assert_eq!(
            accesses.latest(&Rect::new(1, 1, 1, 1), Kind::Writes),
            Some(4)
        );
        // Too many accesses in a tile merge into one read and one write
        // over the whole tile, each keeping its latest layer (a write after
        // them must still follow the reads).
        accesses.add(&Rect::new(32, 0, 1, 1), 1, true);
        for i in 0..30 {
            accesses.add(&Rect::new(32 + (i % 16), 0, 1, 1), i as usize, false);
        }
        assert_eq!(accesses.tiles[2].len(), 2 + 6);
        assert_eq!(
            accesses.latest(&Rect::new(47, 15, 1, 1), Kind::Any),
            Some(23)
        );
        assert_eq!(
            accesses.latest(&Rect::new(47, 15, 1, 1), Kind::Reads),
            Some(23)
        );
        assert_eq!(
            accesses.latest(&Rect::new(47, 15, 1, 1), Kind::Writes),
            Some(1)
        );
    }
}
