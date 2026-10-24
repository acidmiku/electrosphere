//! Screen coverage for the runtime texture dump (`coverage.json`): where
//! each pixel of the picture the screen shows came from.
//!
//! [`Coverage`] keeps one tag per VRAM word, written by the
//! [`TextureRecorder`](super::TextureRecorder) for every write it observes,
//! in stream order:
//! - a primitive tags the pixels it draws: untextured ones
//!   [`UNTEXTURED`]; textured ones by what a texture pack can do with them:
//!   [`TEXTURE`] plus the dumped texture's index (replaceable), or why not
//!   ([`WINDOWED`], [`SPANS`], [`OVERWRITTEN`], [`RENDERED`]). Texels with
//!   the colour 0x0000 are not drawn, so they keep the tag under them;
//! - fills tag [`FILL`], uploads [`UPLOAD`] (movie frames, pictures,
//!   restored screen areas), and copies move the tags with the pixels.
//!
//! The rasterizer samples pixel centres and interpolates texture
//! coordinates affinely, close to the GPU's rules but not exact at edges;
//! the mask bits are ignored. That is enough for shares of the screen.
//!
//! At each field the screen shows ([`Coverage::scan_out`], the host's
//! scan-out point) the tags of the display area are counted, per game
//! state and per block of [`BLOCK_VBLANKS`] VBlanks; a 24-bit display (a
//! movie) counts as [`MOVIE`]. Observation only, like the recorder.
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

const WIDTH: usize = 1024;
const HEIGHT: usize = 512;

/// Not written since the dump started.
pub const NEVER: u32 = 0;
/// GP0 02, the fill (screen clears).
pub const FILL: u32 = 1;
/// Untextured polygons, lines and rectangles: already sharp at N x.
pub const UNTEXTURED: u32 = 2;
/// Textured with a texture window (GP0 E2): packs keep the original.
pub const WINDOWED: u32 = 3;
/// Texels from several uploads, or across the VRAM edge.
pub const SPANS: u32 = 4;
/// Texels of an upload that was partly drawn or copied over since.
pub const OVERWRITTEN: u32 = 5;
/// Texels the game drew or copied itself (render to texture).
pub const RENDERED: u32 = 6;
/// Written by an upload (LoadImage): movie frames, pictures, saved areas.
pub const UPLOAD: u32 = 7;
/// A 24-bit display (the MDEC movies); only counted, never a tag.
pub const MOVIE: u32 = 8;
/// Pixel classes, by tag below [`TEXTURE`].
pub const CLASS_NAMES: [&str; 9] = [
    "never_written",
    "fill",
    "untextured",
    "texture_window",
    "spans_uploads",
    "overwritten",
    "render_to_texture",
    "upload",
    "movie_24bit",
];
/// A replaceable textured pixel: `TEXTURE + n`, n the dumped texture's
/// index (in the order first drawn, the manifest's `textures`).
pub const TEXTURE: u32 = 16;
/// VBlanks per block of the timeline (the default picture interval).
pub const BLOCK_VBLANKS: u64 = 30;
/// A class map of the shown field is kept every this many VBlanks.
pub const MAP_VBLANKS: u64 = 300;
/// The class maps' colours (RGB555), by class, then [`TEXTURE`]'s (green).
pub const MAP_COLOURS: [u16; 10] = [
    rgb(2, 2, 2),
    rgb(12, 12, 12),
    rgb(6, 12, 31),
    rgb(31, 16, 0),
    rgb(31, 0, 0),
    rgb(31, 0, 31),
    rgb(31, 31, 0),
    rgb(16, 0, 24),
    rgb(31, 31, 31),
    rgb(0, 26, 0),
];

const fn rgb(r: u16, g: u16, b: u16) -> u16 {
    r | (g << 5) | (b << 10)
}

/// The class map of one shown field: per pixel the class (index into
/// [`CLASS_NAMES`]) or 9 for a replaceable texture ([`MAP_COLOURS`]).
#[derive(Clone)]
pub struct Map {
    pub vblank: u64,
    pub state: u32,
    pub width: usize,
    pub height: usize,
    pub classes: Vec<u8>,
}

/// Where a textured primitive's texels are read: the texture page, the
/// palette and the texture window as the recorder tracks them, in VRAM
/// before the packet.
pub struct Texels<'a> {
    vram: &'a [u16],
    /// 0: 4-bit, 1: 8-bit, 2: 15-bit.
    depth: u32,
    base: [u32; 2],
    clut: [u32; 2],
    /// `[mask_x, mask_y, offset_x, offset_y]` in texels.
    window: [u32; 4],
    /// No palette entry is 0x0000: every texel is drawn.
    opaque: bool,
}

impl<'a> Texels<'a> {
    /// The texels of texture page `page` (GP0 E1 bits) with the palette
    /// `clut` (a primitive's CLUT field) through `window`
    /// (`[mask_x, mask_y, offset_x, offset_y]`).
    pub fn new(vram: &'a [u16], page: u32, clut: u32, window: [u32; 4]) -> Self {
        let depth = ((page >> 7) & 3).min(2);
        let clut = [(clut & 63) * 16, (clut >> 6) & 511];
        let entries = match depth {
            0 => 16,
            1 => 256,
            _ => 0,
        };
        let row = clut[1] as usize * WIDTH;
        let opaque =
            entries > 0 && (0..entries).all(|i| vram[row + (clut[0] as usize + i) % WIDTH] != 0);
        Self {
            vram,
            depth,
            base: [(page & 15) * 64, ((page >> 4) & 1) * 256],
            clut,
            window,
            opaque,
        }
    }

    /// The 16-bit colour at texture coordinates (u, v), as the GPU reads it.
    pub fn at(&self, u: i32, v: i32) -> u16 {
        let [mask_x, mask_y, offset_x, offset_y] = self.window;
        let u = ((u as u32 & 0xff) & !mask_x) | offset_x;
        let v = ((v as u32 & 0xff) & !mask_y) | offset_y;
        let row = ((self.base[1] + v) as usize % HEIGHT) * WIDTH;
        let read = |x: u32| self.vram[row + x as usize % WIDTH];
        let index = match self.depth {
            0 => (read(self.base[0] + u / 4) >> ((u & 3) * 4)) & 15,
            1 => (read(self.base[0] + u / 2) >> ((u & 1) * 8)) & 0xff,
            _ => return read(self.base[0] + u),
        };
        self.vram
            [self.clut[1] as usize * WIDTH + (self.clut[0] as usize + usize::from(index)) % WIDTH]
    }

    /// Every texel is drawn (no palette entry is transparent).
    pub fn opaque(&self) -> bool {
        self.opaque
    }
}

/// Per-dumped-texture totals over the fields of a state.
#[derive(Clone, Default)]
struct TextureTotals {
    pixels: u64,
    fields: u64,
    /// The most pixels in one field.
    most: u64,
}

/// Counted fields: pixels by class and by dumped texture.
#[derive(Clone, Default)]
struct Totals {
    fields: u64,
    pixels: u64,
    classes: [u64; CLASS_NAMES.len()],
    textures: HashMap<u32, TextureTotals>,
}

impl Totals {
    fn add(&mut self, field: &Field) {
        self.fields += 1;
        self.pixels += field.pixels;
        for (total, count) in self.classes.iter_mut().zip(field.classes) {
            *total += count;
        }
        for &(texture, pixels) in &field.textures {
            let entry = self.textures.entry(texture).or_default();
            entry.pixels += pixels;
            entry.fields += 1;
            entry.most = entry.most.max(pixels);
        }
    }

    fn classes_json(&self) -> Value {
        let textured: u64 = self.textures.values().map(|t| t.pixels).sum();
        let mut map: serde_json::Map<String, Value> = CLASS_NAMES
            .iter()
            .zip(self.classes)
            .map(|(name, count)| ((*name).to_owned(), json!(count)))
            .collect();
        map.insert("texture".to_owned(), json!(textured));
        Value::Object(map)
    }
}

/// One field's counts.
#[derive(Clone, Default)]
struct Field {
    pixels: u64,
    classes: [u64; CLASS_NAMES.len()],
    textures: Vec<(u32, u64)>,
}

/// The coverage map and its counts; see the module documentation.
#[derive(Clone)]
pub struct Coverage {
    tags: Vec<u32>,
    /// Pixels per tag in the field being counted (dense by tag).
    counts: Vec<u64>,
    touched: Vec<u32>,
    /// A tag changed since the last field was counted.
    dirty: bool,
    /// The last field counted, with its display area and movie flag.
    last: Option<([i32; 4], bool, Field)>,
    /// Class maps, one every [`MAP_VBLANKS`].
    maps: Vec<Map>,
    states: BTreeMap<u32, Totals>,
    blocks: Vec<BlockOut>,
}

/// The fields of one block of VBlanks in one game state.
#[derive(Clone)]
struct BlockOut {
    key: BlockKey,
    totals: Totals,
}

/// A block's place: its block number, state, VBlanks and largest display.
#[derive(Clone, Copy, PartialEq, Eq)]
struct BlockKey {
    block: u64,
    state: u32,
    first_vblank: u64,
    last_vblank: u64,
    size: [i32; 2],
}

impl Default for Coverage {
    fn default() -> Self {
        Self::new()
    }
}

impl Coverage {
    pub fn new() -> Self {
        Self {
            tags: vec![NEVER; WIDTH * HEIGHT],
            counts: vec![0; TEXTURE as usize],
            touched: Vec::new(),
            dirty: true,
            last: None,
            maps: Vec::new(),
            states: BTreeMap::new(),
            blocks: Vec::new(),
        }
    }

    /// The tag of a VRAM word (tests and checks).
    pub fn tag(&self, x: usize, y: usize) -> u32 {
        self.tags[(y % HEIGHT) * WIDTH + x % WIDTH]
    }

    fn set(&mut self, x: i32, y: i32, tag: u32) {
        self.tags[y as usize * WIDTH + x as usize] = tag;
    }

    /// Tag the inclusive rectangle, clipped to VRAM.
    pub fn fill_rect(&mut self, [x0, y0, x1, y1]: [i32; 4], tag: u32) {
        self.dirty = true;
        let (x0, y0) = (x0.max(0), y0.max(0));
        let (x1, y1) = (x1.min(WIDTH as i32 - 1), y1.min(HEIGHT as i32 - 1));
        for y in y0..=y1 {
            if x0 <= x1 {
                let row = y as usize * WIDTH;
                self.tags[row + x0 as usize..=row + x1 as usize].fill(tag);
            }
        }
    }

    /// A VRAM copy (GP0 80): the tags move with the pixels, wrapping at the
    /// VRAM edges as the GPU does.
    pub fn copy(&mut self, source: [usize; 2], target: [usize; 2], w: usize, h: usize) {
        self.dirty = true;
        let mut rows = Vec::with_capacity(w * h);
        for row in 0..h {
            let y = (source[1] + row) % HEIGHT;
            rows.extend((0..w).map(|column| self.tags[y * WIDTH + (source[0] + column) % WIDTH]));
        }
        for row in 0..h {
            let y = (target[1] + row) % HEIGHT;
            for column in 0..w {
                self.tags[y * WIDTH + (target[0] + column) % WIDTH] = rows[row * w + column];
            }
        }
    }

    /// A triangle with vertices `points` (drawing offset applied), clipped
    /// to the inclusive drawing `area`; textured ones skip texels 0x0000.
    pub fn triangle(
        &mut self,
        points: [(i32, i32); 3],
        uv: [(i32, i32); 3],
        texels: Option<&Texels>,
        area: [i32; 4],
        tag: u32,
    ) {
        self.dirty = true;
        let [a, b, c] = points.map(|(x, y)| (x as f32, y as f32));
        let det = (b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1);
        if det == 0.0 {
            return;
        }
        let x0 = points
            .iter()
            .map(|p| p.0)
            .min()
            .unwrap_or(0)
            .max(area[0])
            .max(0);
        let x1 = (points.iter().map(|p| p.0).max().unwrap_or(0) - 1)
            .min(area[2])
            .min(WIDTH as i32 - 1);
        let y0 = points
            .iter()
            .map(|p| p.1)
            .min()
            .unwrap_or(0)
            .max(area[1])
            .max(0);
        let y1 = (points.iter().map(|p| p.1).max().unwrap_or(0) - 1)
            .min(area[3])
            .min(HEIGHT as i32 - 1);
        // Affine texture coordinates: constant gradients over the triangle.
        let gradient = |t: [f32; 3]| {
            let (db, dc) = (t[1] - t[0], t[2] - t[0]);
            (
                (db * (c.1 - a.1) - dc * (b.1 - a.1)) / det,
                (dc * (b.0 - a.0) - db * (c.0 - a.0)) / det,
            )
        };
        let us = [uv[0].0, uv[1].0, uv[2].0].map(|u| u as f32);
        let vs = [uv[0].1, uv[1].1, uv[2].1].map(|v| v as f32);
        let (dudx, dudy) = gradient(us);
        let (dvdx, dvdy) = gradient(vs);
        let edges = [(a, b), (b, c), (c, a)];
        for y in y0..=y1 {
            // The row's span at the pixel centres: x + 0.5 in [lo, hi).
            let cy = y as f32 + 0.5;
            let (mut lo, mut hi) = (f32::MAX, f32::MIN);
            for (p, q) in edges {
                if (p.1 <= cy) != (q.1 <= cy) {
                    let x = p.0 + (cy - p.1) * (q.0 - p.0) / (q.1 - p.1);
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
            if lo > hi {
                continue;
            }
            let start = ((lo - 0.5).ceil() as i32).max(x0);
            let end = ((hi - 0.5).ceil() as i32 - 1).min(x1);
            if start > end {
                continue;
            }
            let row = y as usize * WIDTH;
            match texels.filter(|texels| !texels.opaque()) {
                None => self.tags[row + start as usize..=row + end as usize].fill(tag),
                Some(texels) => {
                    let (fx, fy) = (start as f32 + 0.5 - a.0, cy - a.1);
                    let mut u = us[0] + dudx * fx + dudy * fy;
                    let mut v = vs[0] + dvdx * fx + dvdy * fy;
                    for x in start..=end {
                        if texels.at(u.floor() as i32, v.floor() as i32) != 0 {
                            self.tags[row + x as usize] = tag;
                        }
                        u += dudx;
                        v += dvdx;
                    }
                }
            }
        }
    }

    /// A rectangle (sprite) at (x, y), `w` x `h`, clipped to the inclusive
    /// drawing `area`; a textured one reads texel (u + dx, v + dy).
    #[allow(clippy::too_many_arguments)]
    pub fn rectangle(
        &mut self,
        [x, y]: [i32; 2],
        [w, h]: [i32; 2],
        uv: (i32, i32),
        texels: Option<&Texels>,
        area: [i32; 4],
        tag: u32,
    ) {
        self.dirty = true;
        let x0 = x.max(area[0]).max(0);
        let x1 = (x + w - 1).min(area[2]).min(WIDTH as i32 - 1);
        let y0 = y.max(area[1]).max(0);
        let y1 = (y + h - 1).min(area[3]).min(HEIGHT as i32 - 1);
        if texels.is_none_or(Texels::opaque) {
            self.fill_rect([x0, y0, x1, y1], tag);
            return;
        }
        for row in y0..=y1 {
            for column in x0..=x1 {
                if let Some(texels) = texels
                    && texels.at(uv.0 + column - x, uv.1 + row - y) == 0
                {
                    continue;
                }
                self.set(column, row, tag);
            }
        }
    }

    /// A line segment, both ends included, clipped to the drawing `area`.
    pub fn line(&mut self, a: (i32, i32), b: (i32, i32), area: [i32; 4]) {
        self.dirty = true;
        let (dx, dy) = ((b.0 - a.0).abs(), -(b.1 - a.1).abs());
        let (sx, sy) = ((b.0 - a.0).signum(), (b.1 - a.1).signum());
        let (mut x, mut y, mut error) = (a.0, a.1, dx + dy);
        loop {
            if (area[0]..=area[2]).contains(&x)
                && (area[1]..=area[3]).contains(&y)
                && (0..WIDTH as i32).contains(&x)
                && (0..HEIGHT as i32).contains(&y)
            {
                self.set(x, y, UNTEXTURED);
            }
            if (x, y) == b {
                break;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }

    /// Count the field the screen shows: the display area `[x, y, w, h]`
    /// (VRAM words; a 24-bit display counts as [`MOVIE`]) at `vblank` in
    /// game state `state`.
    pub fn scan_out(&mut self, [x, y, w, h]: [i32; 4], movie: bool, vblank: u64, state: u32) {
        let (w, h) = (w.max(0), h.max(0));
        let display = [x, y, w, h];
        // Nothing drawn since the last field and the same display: the
        // same counts (the second field of a frame drawn every two).
        let field = match &self.last {
            Some((last, was_movie, field))
                if !self.dirty && *last == display && *was_movie == movie =>
            {
                field.clone()
            }
            _ => self.count(display, movie),
        };
        self.dirty = false;
        self.states.entry(state).or_default().add(&field);
        let block = vblank / BLOCK_VBLANKS;
        let same = self
            .blocks
            .last()
            .is_some_and(|out| out.key.block == block && out.key.state == state);
        if !same {
            self.blocks.push(BlockOut {
                key: BlockKey {
                    block,
                    state,
                    first_vblank: vblank,
                    last_vblank: vblank,
                    size: [w, h],
                },
                totals: Totals::default(),
            });
        }
        let out = self.blocks.last_mut().expect("a block");
        out.key.last_vblank = vblank;
        out.key.size = [out.key.size[0].max(w), out.key.size[1].max(h)];
        out.totals.add(&field);
        self.last = Some((display, movie, field));
        if self
            .maps
            .last()
            .is_none_or(|map| map.vblank / MAP_VBLANKS != vblank / MAP_VBLANKS)
        {
            self.keep_map(display, movie, vblank, state);
        }
    }

    /// Keep the class map of the display area.
    fn keep_map(&mut self, [x, y, w, h]: [i32; 4], movie: bool, vblank: u64, state: u32) {
        let x0 = x.clamp(0, WIDTH as i32) as usize;
        let x1 = (x + w).clamp(0, WIDTH as i32) as usize;
        let y0 = y.clamp(0, HEIGHT as i32) as usize;
        let y1 = (y + h).clamp(0, HEIGHT as i32) as usize;
        let classes = if movie {
            vec![MOVIE as u8; (x1 - x0) * (y1 - y0)]
        } else {
            (y0..y1)
                .flat_map(|row| &self.tags[row * WIDTH + x0..row * WIDTH + x1])
                .map(|&tag| if tag >= TEXTURE { 9 } else { tag as u8 })
                .collect()
        };
        self.maps.push(Map {
            vblank,
            state,
            width: x1 - x0,
            height: y1 - y0,
            classes,
        });
    }

    /// The class maps kept, one every [`MAP_VBLANKS`].
    pub fn maps(&self) -> &[Map] {
        &self.maps
    }

    /// The counts of the display area's tags, by runs of equal tags.
    fn count(&mut self, [x, y, w, h]: [i32; 4], movie: bool) -> Field {
        let mut field = Field::default();
        if movie {
            field.pixels = (w * h) as u64;
            field.classes[MOVIE as usize] = field.pixels;
            return field;
        }
        let x0 = x.clamp(0, WIDTH as i32) as usize;
        let x1 = (x + w).clamp(0, WIDTH as i32) as usize;
        let y0 = y.clamp(0, HEIGHT as i32) as usize;
        let y1 = (y + h).clamp(0, HEIGHT as i32) as usize;
        let add = |tag: u32, run: u64, counts: &mut Vec<u64>, touched: &mut Vec<u32>| {
            let tag = tag as usize;
            if tag >= counts.len() {
                counts.resize(tag + 1, 0);
            }
            if counts[tag] == 0 {
                touched.push(tag as u32);
            }
            counts[tag] += run;
        };
        for row in y0..y1 {
            let pixels = &self.tags[row * WIDTH + x0..row * WIDTH + x1];
            let Some(&first) = pixels.first() else {
                continue;
            };
            let (mut tag, mut run) = (first, 0u64);
            for &next in pixels {
                if next == tag {
                    run += 1;
                } else {
                    add(tag, run, &mut self.counts, &mut self.touched);
                    (tag, run) = (next, 1);
                }
            }
            add(tag, run, &mut self.counts, &mut self.touched);
        }
        field.pixels = ((x1 - x0) * (y1 - y0)) as u64;
        for tag in self.touched.drain(..) {
            let count = std::mem::take(&mut self.counts[tag as usize]);
            if tag < TEXTURE {
                field.classes[tag as usize] += count;
            } else {
                field.textures.push((tag - TEXTURE, count));
            }
        }
        field
    }

    /// Fields counted so far.
    pub fn fields(&self) -> u64 {
        self.states.values().map(|totals| totals.fields).sum()
    }

    /// The report (`coverage.json`): per state and per block, with the
    /// dumped textures named by `key` (their index → replacement key) and
    /// `folder` naming a state.
    pub fn report(&self, key: &dyn Fn(u32) -> String, folder: &dyn Fn(u32) -> String) -> Value {
        let states: serde_json::Map<String, Value> = self
            .states
            .iter()
            .map(|(&state, totals)| {
                let mut textures: Vec<(&u32, &TextureTotals)> = totals.textures.iter().collect();
                textures.sort_by(|a, b| b.1.pixels.cmp(&a.1.pixels).then(a.0.cmp(b.0)));
                let per_field = totals.pixels.max(1) as f64 / totals.fields.max(1) as f64;
                let textures: Vec<Value> = textures
                    .into_iter()
                    .map(|(&texture, t)| {
                        json!({
                            "key": key(texture),
                            "pixels": t.pixels,
                            "share": t.pixels as f64 / totals.pixels.max(1) as f64,
                            "fields": t.fields,
                            "max_share": t.most as f64 / per_field,
                        })
                    })
                    .collect();
                (
                    folder(state),
                    json!({
                        "state": state,
                        "fields": totals.fields,
                        "pixels": totals.pixels,
                        "classes": totals.classes_json(),
                        "textures": textures,
                    }),
                )
            })
            .collect();
        let blocks: Vec<Value> = self
            .blocks
            .iter()
            .map(|out| {
                let mut textures: Vec<(&u32, &TextureTotals)> =
                    out.totals.textures.iter().collect();
                textures.sort_by(|a, b| b.1.pixels.cmp(&a.1.pixels).then(a.0.cmp(b.0)));
                json!({
                    "first_vblank": out.key.first_vblank,
                    "last_vblank": out.key.last_vblank,
                    "state": out.key.state,
                    "width": out.key.size[0],
                    "height": out.key.size[1],
                    "fields": out.totals.fields,
                    "pixels": out.totals.pixels,
                    "classes": out.totals.classes_json(),
                    "textures": textures.into_iter()
                        .map(|(&texture, t)| json!([key(texture), t.pixels]))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({
            "schema": "ac3-asset-dump/coverage/1",
            "classes": CLASS_NAMES.iter().chain(["texture"].iter()).collect::<Vec<_>>(),
            "block_vblanks": BLOCK_VBLANKS,
            "fields": self.fields(),
            "states": states,
            "blocks": blocks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [i32; 4] = [0, 0, 1023, 511];

    #[test]
    fn triangles_cover_their_inside_and_skip_clear_texels() {
        let mut coverage = Coverage::new();
        // A 4x4 square from two triangles covers exactly 16 pixels.
        let square = [(10, 10), (14, 10), (10, 14), (14, 14)];
        let uv = [(0, 0); 3];
        coverage.triangle([square[0], square[1], square[2]], uv, None, ALL, 2);
        coverage.triangle([square[1], square[2], square[3]], uv, None, ALL, 2);
        let covered = (0..30)
            .flat_map(|y| (0..30).map(move |x| (x, y)))
            .filter(|&(x, y)| coverage.tag(x, y) == 2)
            .count();
        assert_eq!(covered, 16);
        assert_eq!(coverage.tag(13, 13), 2);
        assert_eq!(coverage.tag(14, 14), NEVER);
        // A 15-bit sprite whose left half is 0x0000: only the right half.
        let mut vram = vec![0u16; WIDTH * HEIGHT];
        for x in 2..4 {
            vram[x] = 0x7fff;
            vram[WIDTH + x] = 0x7fff;
        }
        let texels = Texels::new(&vram, 0x100, 0, [0; 4]);
        coverage.rectangle([100, 100], [4, 2], (0, 0), Some(&texels), ALL, 20);
        let tags: Vec<u32> = (100..104).map(|x| coverage.tag(x, 100)).collect();
        assert_eq!(tags, [NEVER, NEVER, 20, 20]);
    }

    #[test]
    fn copies_move_tags_and_fields_are_counted() {
        let mut coverage = Coverage::new();
        coverage.fill_rect([0, 0, 9, 9], FILL);
        coverage.rectangle([0, 0], [5, 10], (0, 0), None, ALL, TEXTURE + 3);
        coverage.copy([0, 0], [20, 0], 10, 10);
        assert_eq!(coverage.tag(20, 0), TEXTURE + 3);
        assert_eq!(coverage.tag(29, 9), FILL);
        coverage.scan_out([20, 0, 10, 10], false, 31, 4);
        coverage.scan_out([0, 0, 320, 240], true, 32, 5);
        let report = coverage.report(&|t| format!("t{t}"), &|s| format!("s{s}"));
        assert_eq!(report["states"]["s4"]["classes"]["fill"], 50);
        assert_eq!(report["states"]["s4"]["classes"]["texture"], 50);
        assert_eq!(report["states"]["s4"]["textures"][0]["key"], "t3");
        assert_eq!(report["states"]["s5"]["classes"]["movie_24bit"], 76800);
        assert_eq!(report["blocks"].as_array().unwrap().len(), 2);
        assert_eq!(report["blocks"][0]["first_vblank"], 31);
    }
}
