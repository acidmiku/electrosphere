//! Runtime texture dump (`ac3 machine --dump-textures DIR`).
//!
//! [`TextureRecorder`] watches the two ways the game changes VRAM:
//! - [`on_upload`](TextureRecorder::on_upload): every CPU→VRAM transfer
//!   (LoadImage). Each upload is kept once per content hash (the key's
//!   `T`), and a per-word owner map remembers which upload last wrote each
//!   VRAM word.
//! - [`observe_gp0`](TextureRecorder::observe_gp0): every GPU packet, just
//!   before it executes. Drawing state (texture page, window, drawing area
//!   and offset) is tracked as the GPU does. Every pixel a command can write
//!   (polygons, lines and rectangles clipped to the drawing area, fills,
//!   copies, uploads inside a list) loses its owner. A textured primitive is
//!   attributed to an upload when every VRAM word its texture coordinates
//!   cover is still owned by that one upload, so the sampled texels are
//!   exactly the uploaded ones. For 4/8-bit modes the palette (CLUT row) is
//!   read from VRAM at that moment.
//!
//! Each distinct (upload content, mode, palette) becomes one PNG named by
//! its replacement key; see `docs/ASSET_DUMP.md`. Observing changes nothing
//! in the machine or the GPU.
//!
//! The recorder also keeps the screen coverage ([`Coverage`],
//! `coverage.json`): which primitive, texture or upload each pixel of the
//! picture the screen shows came from, counted at the host's scan-out
//! point ([`scan_out`](TextureRecorder::scan_out)).
use super::coverage::{self, Coverage, Texels};
use super::key::{self, Key, Mode, PaletteKey};
use super::png_io::{self, Pixels};
use crate::Result;
use ac3_graphics::Vram;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::Path,
};

const WIDTH: usize = 1024;
const HEIGHT: usize = 512;
const NONE: u32 = u32::MAX;
/// Owner-map tiles: 32x32 words, so drawing far from any upload is cheap.
const TILE: usize = 32;
const TILES_X: usize = WIDTH / TILE;
/// Distinct positions/CLUTs listed per entry in the manifest.
const LIST_LIMIT: usize = 16;
/// Distinct primitive sizes counted per texture (the rest are "other").
const SIZE_LIMIT: usize = 32;

fn sign_extend_11(value: u32) -> i32 {
    ((value << 21) as i32) >> 21
}

fn over_limit(a: (i32, i32), b: (i32, i32)) -> bool {
    (a.0 - b.0).abs() >= 1024 || (a.1 - b.1).abs() >= 512
}

fn terminator(word: u32) -> bool {
    word & 0xf000_f000 == 0x5000_5000
}

/// One distinct upload content.
#[derive(Clone)]
struct Content {
    texture: String,
    width: u16,
    height: u16,
    /// Kept while an upload of it is live in VRAM or once it was drawn.
    data: Option<Vec<u16>>,
    uploads: u64,
    positions: Vec<(u16, u16)>,
    first_vblank: u64,
    first_state: u32,
    /// Live uploads (some word still owned).
    live: u32,
    drawn: bool,
    /// Distinct textures whose palette this content supplied.
    palette_uses: u64,
    /// Cached index ranges for p4 and p8.
    ranges: [Option<Option<(u8, u8)>>; 2],
}

/// One upload event: a content at a VRAM rectangle.
#[derive(Clone)]
struct Instance {
    content: u32,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    live_words: u32,
    /// Words lost so far; an epoch for the overwrite checks.
    lost: u32,
}

/// One distinct palette: the entries `lo..=hi` that keyed it.
#[derive(Clone)]
struct Palette {
    hash: String,
    lo: u8,
    hi: u8,
    /// All 16/256 entries at its first sighting (for the PNG).
    entries: Vec<u16>,
}

/// One dumped texture: (content, mode, palette).
#[derive(Clone)]
struct Combo {
    content: u32,
    mode: Mode,
    palette: u32,
    draws: u64,
    polygons: u64,
    rectangles: u64,
    semi_transparent: u64,
    raw: u64,
    /// Bounding box of the sampled texels, in the upload's texel space.
    region: [i32; 4],
    first_vblank: u64,
    first_state: u32,
    last_vblank: u64,
    cluts: Vec<(u16, u16)>,
    /// The upload that last wrote the CLUT (content, x and y inside it).
    palette_source: Option<(u32, u16, u16)>,
    /// Draws by on-screen size: (polygon, width, height) → count; a
    /// polygon's size is its bounding box.
    sizes: BTreeMap<(bool, u16, u16), u64>,
    other_sizes: u64,
}

/// GPU drawing state as the recorder tracks it.
#[derive(Clone, Copy, Default)]
struct DrawState {
    page: u32,
    window: [u32; 4],
    /// Inclusive drawing area; `None` until the first E3/E4 (then all VRAM
    /// is assumed writable).
    area: Option<[i32; 4]>,
    offset: [i32; 2],
}

#[derive(Clone, Default)]
struct Stats {
    uploads: u64,
    upload_words: u64,
    skipped_uploads: u64,
    list_uploads: u64,
    packets: u64,
    commands: u64,
    incomplete: u64,
    fills: u64,
    copies: u64,
    textured: u64,
    attributed: u64,
    windowed: u64,
    unattributed: BTreeMap<&'static str, u64>,
    /// Primitives spanning several uploads, by the size (VRAM words) of
    /// the upload holding their first texel.
    spans: BTreeMap<(u16, u16), u64>,
    /// The same by that upload's rectangle `[x, y, w, h]`: primitives and
    /// the VRAM words they sampled (`[x0, y0, x1, y1]`, inclusive).
    span_places: HashMap<[u16; 4], (u64, [i32; 4])>,
    released_contents: u64,
}

/// Distinct upload rectangles kept for the spanning primitives.
const SPAN_PLACES: usize = 4096;

/// Last palette read at a CLUT position for an index range: the entries
/// and their palette id.
type PaletteCache = HashMap<(u16, u16, u8, u8), (Vec<u16>, u32)>;

/// Records uploads and textured draws for the runtime texture dump.
/// Host-side observation, not game state: a savestate need not include it.
#[derive(Clone)]
pub struct TextureRecorder {
    contents: Vec<Content>,
    by_digest: HashMap<[u8; 32], u32>,
    instances: Vec<Instance>,
    owner: Vec<u32>,
    tile_owned: Vec<u32>,
    palettes: Vec<Palette>,
    palette_ids: HashMap<(u8, u8, Vec<u16>), u32>,
    palette_cache: PaletteCache,
    combos: Vec<Combo>,
    combo_ids: HashMap<(u32, Mode, u32), u32>,
    verified: HashMap<(u32, [u16; 4]), u32>,
    state: DrawState,
    vblank: u64,
    game_state: u32,
    stats: Stats,
    coverage: Coverage,
}

impl Default for TextureRecorder {
    fn default() -> Self {
        Self::new()
    }
}

/// A primitive's texture use, as decoded from its command.
struct Sample {
    page: u32,
    clut: u32,
    u: [i32; 2],
    v: [i32; 2],
    polygon: bool,
    semi: bool,
    raw: bool,
    /// On-screen size (a polygon's bounding box).
    size: (u16, u16),
}

impl TextureRecorder {
    pub fn new() -> Self {
        Self {
            contents: Vec::new(),
            by_digest: HashMap::new(),
            instances: Vec::new(),
            owner: vec![NONE; WIDTH * HEIGHT],
            tile_owned: vec![0; (WIDTH / TILE) * (HEIGHT / TILE)],
            palettes: Vec::new(),
            palette_ids: HashMap::new(),
            palette_cache: HashMap::new(),
            combos: Vec::new(),
            combo_ids: HashMap::new(),
            verified: HashMap::new(),
            state: DrawState::default(),
            vblank: 0,
            game_state: 0,
            stats: Stats::default(),
            coverage: Coverage::new(),
        }
    }

    /// The time stamp of what follows: the VBlank count and the resident
    /// game state (context +0x2C: 2 flight, 3 hub/briefing, 4 menus, ...).
    pub fn set_frame(&mut self, vblank: u64, game_state: u32) {
        self.vblank = vblank;
        self.game_state = game_state;
    }

    /// Distinct textures (content, mode, palette) seen drawn so far.
    pub fn textures(&self) -> usize {
        self.combos.len()
    }

    /// Distinct upload contents seen so far.
    pub fn uploads(&self) -> usize {
        self.contents.len()
    }

    /// The screen coverage so far.
    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// The field the screen shows, at the host's scan-out point (the first
    /// VRAM write after a VBlank, or the next VBlank when nothing was
    /// written): count the display area `[x, y, w, h]` (VRAM words; a
    /// 24-bit display is a movie) in the screen coverage.
    pub fn scan_out(&mut self, display: [i32; 4], movie: bool) {
        self.coverage
            .scan_out(display, movie, self.vblank, self.game_state);
    }

    /// A CPU→VRAM transfer of `rect` = (x, y, w, h) in VRAM words, with its
    /// `w * h` halfwords, as it reaches the GPU. Transfers that are empty or
    /// cross the VRAM edge (which the GPU rejects) are only counted.
    pub fn on_upload(&mut self, rect: [u16; 4], pixels: &[u16]) {
        let [x, y, w, h] = rect.map(usize::from);
        if w == 0 || h == 0 || x + w > WIDTH || y + h > HEIGHT || pixels.len() < w * h {
            self.stats.skipped_uploads += 1;
            return;
        }
        let pixels = &pixels[..w * h];
        self.coverage.fill_rect(
            [x as i32, y as i32, (x + w) as i32 - 1, (y + h) as i32 - 1],
            coverage::UPLOAD,
        );
        self.stats.uploads += 1;
        self.stats.upload_words += pixels.len() as u64;
        let mut hasher = Sha256::new();
        hasher.update(rect[2].to_le_bytes());
        hasher.update(rect[3].to_le_bytes());
        for word in pixels {
            hasher.update(word.to_le_bytes());
        }
        let digest: [u8; 32] = hasher.finalize().into();
        let content = match self.by_digest.get(&digest) {
            Some(&id) => id,
            None => {
                let id = self.contents.len() as u32;
                self.by_digest.insert(digest, id);
                self.contents.push(Content {
                    texture: digest[..8].iter().map(|b| format!("{b:02x}")).collect(),
                    width: rect[2],
                    height: rect[3],
                    data: None,
                    uploads: 0,
                    positions: Vec::new(),
                    first_vblank: self.vblank,
                    first_state: self.game_state,
                    live: 0,
                    drawn: false,
                    palette_uses: 0,
                    ranges: [None; 2],
                });
                id
            }
        };
        let entry = &mut self.contents[content as usize];
        entry.uploads += 1;
        entry.live += 1;
        if entry.data.is_none() {
            entry.data = Some(pixels.to_vec());
        }
        let position = (rect[0], rect[1]);
        if entry.positions.len() < LIST_LIMIT && !entry.positions.contains(&position) {
            entry.positions.push(position);
        }
        let id = self.instances.len() as u32;
        self.instances.push(Instance {
            content,
            x: rect[0],
            y: rect[1],
            width: rect[2],
            height: rect[3],
            live_words: (w * h) as u32,
            lost: 0,
        });
        for row in y..y + h {
            for column in x..x + w {
                let at = row * WIDTH + column;
                let previous = self.owner[at];
                if previous == NONE {
                    self.tile_owned[(row / TILE) * TILES_X + column / TILE] += 1;
                } else {
                    self.lose_word(previous);
                }
                self.owner[at] = id;
            }
        }
    }

    fn lose_word(&mut self, instance: u32) {
        let instance = &mut self.instances[instance as usize];
        instance.live_words -= 1;
        instance.lost = instance.lost.wrapping_add(1);
        if instance.live_words == 0 {
            let content = &mut self.contents[instance.content as usize];
            content.live -= 1;
            if content.live == 0 && !content.drawn {
                content.data = None;
                self.stats.released_contents += 1;
            }
        }
    }

    /// Every word in the inclusive rectangle (clipped to VRAM) loses its owner.
    fn disown(&mut self, [x0, y0, x1, y1]: [i32; 4]) {
        let (x0, y0) = (x0.max(0) as usize, y0.max(0) as usize);
        if x1 < 0 || y1 < 0 {
            return;
        }
        let (x1, y1) = ((x1 as usize).min(WIDTH - 1), (y1 as usize).min(HEIGHT - 1));
        if x0 > x1 || y0 > y1 {
            return;
        }
        for tile_y in y0 / TILE..=y1 / TILE {
            for tile_x in x0 / TILE..=x1 / TILE {
                let tile = tile_y * TILES_X + tile_x;
                if self.tile_owned[tile] == 0 {
                    continue;
                }
                let rows = (tile_y * TILE).max(y0)..=(tile_y * TILE + TILE - 1).min(y1);
                let columns = (tile_x * TILE).max(x0)..=(tile_x * TILE + TILE - 1).min(x1);
                for row in rows {
                    for column in columns.clone() {
                        let at = row * WIDTH + column;
                        let previous = self.owner[at];
                        if previous != NONE {
                            self.owner[at] = NONE;
                            self.tile_owned[tile] -= 1;
                            self.lose_word(previous);
                        }
                    }
                }
            }
        }
    }

    /// A `w` x `h` rectangle at (x, y) that wraps at the VRAM edges.
    fn disown_wrapped(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let (x, y) = (x % WIDTH, y % HEIGHT);
        let first_w = w.min(WIDTH - x);
        let first_h = h.min(HEIGHT - y);
        let rect = |x: usize, y: usize, w: usize, h: usize| {
            [x as i32, y as i32, (x + w) as i32 - 1, (y + h) as i32 - 1]
        };
        self.disown(rect(x, y, first_w, first_h));
        if w > first_w {
            self.disown(rect(0, y, w - first_w, first_h));
        }
        if h > first_h {
            self.disown(rect(x, 0, first_w, h - first_h));
            if w > first_w {
                self.disown(rect(0, 0, w - first_w, h - first_h));
            }
        }
    }

    /// Clip a drawing bounding box to the drawing area and disown it.
    fn disown_drawn(&mut self, [x0, y0, x1, y1]: [i32; 4]) {
        let [left, top, right, bottom] = self.state.area.unwrap_or([0, 0, 1023, 511]);
        self.disown([x0.max(left), y0.max(top), x1.min(right), y1.min(bottom)]);
    }

    /// GP1 control words: a reset (00) clears the drawing state as the GPU
    /// does. Optional; without it the recorder keeps the last state.
    pub fn observe_gp1(&mut self, word: u32) {
        if word >> 24 == 0 {
            self.state = DrawState {
                area: Some([0; 4]),
                ..DrawState::default()
            };
        }
    }

    /// One GPU packet (whole GP0 commands), before the GPU executes it;
    /// `vram` is the VRAM the packet will sample.
    pub fn observe_gp0(&mut self, words: &[u32], vram: &Vram) {
        self.stats.packets += 1;
        let mut at = 0;
        while at < words.len() {
            let Some(length) = command_length(&words[at..]) else {
                self.stats.incomplete += 1;
                break;
            };
            self.command(&words[at..at + length], vram);
            at += length;
        }
    }

    fn command(&mut self, words: &[u32], vram: &Vram) {
        self.stats.commands += 1;
        let command = words[0];
        let op = (command >> 24) as u8;
        match op {
            0x02 => {
                self.stats.fills += 1;
                let x = (words[1] & 0x3f0) as i32;
                let y = (words[1] >> 16) as i32;
                let w = ((((words[2] & 0x3ff) + 0xf) & !0xf) as i32).min(1024);
                let h = ((words[2] >> 16) & 0x1ff) as i32;
                if w > 0 && h > 0 {
                    self.disown([x, y, x + w - 1, y + h - 1]);
                    self.coverage
                        .fill_rect([x, y, x + w - 1, y + h - 1], coverage::FILL);
                }
            }
            0x20..=0x3f => self.polygon(words, vram),
            0x40..=0x5f => self.line(words),
            0x60..=0x7f => self.rectangle(words, vram),
            0x80..=0x9f => {
                self.stats.copies += 1;
                let sx = (words[1] & 0x3ff) as usize;
                let sy = ((words[1] >> 16) & 0x1ff) as usize;
                let dx = (words[2] & 0x3ff) as usize;
                let dy = ((words[2] >> 16) & 0x1ff) as usize;
                let w = (((words[3] & 0xffff).wrapping_sub(1) & 0x3ff) + 1) as usize;
                let h = (((words[3] >> 16).wrapping_sub(1) & 0x1ff) + 1) as usize;
                self.disown_wrapped(dx, dy, w, h);
                self.coverage.copy([sx, sy], [dx, dy], w, h);
            }
            0xa0..=0xbf => {
                self.stats.list_uploads += 1;
                let x = (words[1] & 0xffff) as u16 % WIDTH as u16;
                let y = (words[1] >> 16) as u16 % HEIGHT as u16;
                let w = (((words[2] & 0xffff).wrapping_sub(1) & 0x3ff) + 1) as u16;
                let h = (((words[2] >> 16).wrapping_sub(1) & 0x1ff) + 1) as u16;
                let count = usize::from(w) * usize::from(h);
                let pixels: Vec<u16> = (0..count)
                    .map(|i| (words[3 + i / 2] >> (16 * (i & 1))) as u16)
                    .collect();
                if usize::from(x) + usize::from(w) > WIDTH
                    || usize::from(y) + usize::from(h) > HEIGHT
                {
                    // The GPU rejects it; nothing is written.
                    self.stats.skipped_uploads += 1;
                } else {
                    self.on_upload([x, y, w, h], &pixels);
                }
            }
            0xe1 => self.state.page = command & 0x1ff,
            0xe2 => {
                let field = |shift: u32| (command >> shift) & 0x1f;
                let (mask_x, mask_y) = (field(0) << 3, field(5) << 3);
                self.state.window = [
                    mask_x,
                    mask_y,
                    (field(10) << 3) & mask_x,
                    (field(15) << 3) & mask_y,
                ];
            }
            0xe3 => {
                let mut area = self.state.area.unwrap_or([0, 0, 1023, 511]);
                area[0] = (command & 0x3ff) as i32;
                area[1] = ((command >> 10) & 0x3ff) as i32;
                self.state.area = Some(area);
            }
            0xe4 => {
                let mut area = self.state.area.unwrap_or([0; 4]);
                area[2] = (command & 0x3ff) as i32;
                area[3] = ((command >> 10) & 0x3ff) as i32;
                self.state.area = Some(area);
            }
            0xe5 => {
                self.state.offset = [sign_extend_11(command), sign_extend_11(command >> 11)];
            }
            _ => {}
        }
    }

    fn offset_point(&self, word: u32) -> (i32, i32) {
        let (x, y) = (sign_extend_11(word), sign_extend_11(word >> 16));
        (
            sign_extend_11((x + self.state.offset[0]) as u32),
            sign_extend_11((y + self.state.offset[1]) as u32),
        )
    }

    fn polygon(&mut self, words: &[u32], vram: &Vram) {
        let op = (words[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let textured = op & 4 != 0;
        let count = if op & 8 != 0 { 4 } else { 3 };
        let mut points = [(0, 0); 4];
        let mut uv = [(0, 0); 4];
        let (mut clut, mut page) = (0, 0);
        let mut at = 1;
        for i in 0..count {
            if gouraud && i > 0 {
                at += 1;
            }
            points[i] = self.offset_point(words[at]);
            at += 1;
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
        let edges: &[(usize, usize)] = if count == 4 {
            &[(0, 1), (1, 3), (3, 2), (2, 0), (1, 2)]
        } else {
            &[(0, 1), (1, 2), (2, 0)]
        };
        if edges.iter().any(|&(a, b)| over_limit(points[a], points[b])) {
            return;
        }
        let xs = points[..count].iter().map(|p| p.0);
        let ys = points[..count].iter().map(|p| p.1);
        let bounds = [
            xs.clone().min().unwrap_or(0),
            ys.clone().min().unwrap_or(0),
            xs.max().unwrap_or(0),
            ys.max().unwrap_or(0),
        ];
        let tag = if textured {
            // A textured polygon's page word becomes the GPU's texture page.
            self.state.page = page & 0x1ff;
            // The texels read: the vertices' range, without the right and
            // bottom ends a polygon names but never reads.
            let vertices = |axis: fn((i32, i32)) -> i32| -> Vec<(f32, f32, i32)> {
                (0..count)
                    .map(|i| (points[i].0 as f32, points[i].1 as f32, axis(uv[i])))
                    .collect()
            };
            let us = uv[..count].iter().map(|p| p.0);
            let vs = uv[..count].iter().map(|p| p.1);
            self.sample(
                Sample {
                    page,
                    clut,
                    u: [
                        us.min().unwrap_or(0),
                        key::sampled_hi(&vertices(|(u, _)| u)),
                    ],
                    v: [
                        vs.min().unwrap_or(0),
                        key::sampled_hi(&vertices(|(_, v)| v)),
                    ],
                    polygon: true,
                    semi: op & 2 != 0,
                    raw: op & 1 != 0,
                    size: (
                        (bounds[2] - bounds[0]).clamp(0, 0xffff) as u16,
                        (bounds[3] - bounds[1]).clamp(0, 0xffff) as u16,
                    ),
                },
                vram,
            )
        } else {
            coverage::UNTEXTURED
        };
        let texels = textured.then(|| Texels::new(vram.words(), page, clut, self.state.window));
        let area = self.state.area.unwrap_or([0, 0, 1023, 511]);
        let corners: &[[usize; 3]] = if count == 4 {
            &[[0, 1, 2], [1, 2, 3]]
        } else {
            &[[0, 1, 2]]
        };
        for &[a, b, c] in corners {
            self.coverage.triangle(
                [points[a], points[b], points[c]],
                [uv[a], uv[b], uv[c]],
                texels.as_ref(),
                area,
                tag,
            );
        }
        self.disown_drawn(bounds);
    }

    fn line(&mut self, words: &[u32]) {
        let op = (words[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let end = if op & 8 != 0 {
            words.len() - 1
        } else {
            words.len()
        };
        let mut points = vec![self.offset_point(words[1])];
        let mut at = 2;
        while at < end {
            if gouraud {
                at += 1;
                if at >= end {
                    break;
                }
            }
            points.push(self.offset_point(words[at]));
            at += 1;
        }
        let area = self.state.area.unwrap_or([0, 0, 1023, 511]);
        for pair in points.windows(2) {
            if over_limit(pair[0], pair[1]) {
                continue;
            }
            self.coverage.line(pair[0], pair[1], area);
            self.disown_drawn([
                pair[0].0.min(pair[1].0),
                pair[0].1.min(pair[1].1),
                pair[0].0.max(pair[1].0),
                pair[0].1.max(pair[1].1),
            ]);
        }
    }

    fn rectangle(&mut self, words: &[u32], vram: &Vram) {
        let op = (words[0] >> 24) as u8;
        let textured = op & 4 != 0;
        let x = sign_extend_11(words[1]) + self.state.offset[0];
        let y = sign_extend_11(words[1] >> 16) + self.state.offset[1];
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
        let area = self.state.area.unwrap_or([0, 0, 1023, 511]);
        if textured {
            let word = words[2];
            let (u, v) = ((word & 0xff) as i32, ((word >> 8) & 0xff) as i32);
            // Coordinates past 255 wrap: the whole axis may be sampled.
            let span = |start: i32, size: i32| {
                if start + size - 1 > 255 {
                    [0, 255]
                } else {
                    [start, start + size - 1]
                }
            };
            let tag = self.sample(
                Sample {
                    page: self.state.page,
                    clut: word >> 16,
                    u: span(u, w),
                    v: span(v, h),
                    polygon: false,
                    semi: op & 2 != 0,
                    raw: op & 1 != 0,
                    size: (w as u16, h as u16),
                },
                vram,
            );
            let texels = Texels::new(vram.words(), self.state.page, word >> 16, self.state.window);
            self.coverage
                .rectangle([x, y], [w, h], (u, v), Some(&texels), area, tag);
        } else {
            self.coverage
                .rectangle([x, y], [w, h], (0, 0), None, area, coverage::UNTEXTURED);
        }
        self.disown_drawn([x, y, x + w - 1, y + h - 1]);
    }

    /// A primitive not attributed for `reason`; returns its coverage `tag`.
    fn miss(&mut self, reason: &'static str, tag: u32) -> u32 {
        *self.stats.unattributed.entry(reason).or_default() += 1;
        tag
    }

    /// The upload a primitive samples, its mode and palette; returns the
    /// primitive's coverage tag (the dumped texture, or why a texture pack
    /// cannot replace it).
    fn sample(&mut self, sample: Sample, vram: &Vram) -> u32 {
        self.stats.textured += 1;
        let mode = Mode::from_page(sample.page);
        let [mask_x, mask_y, offset_x, offset_y] = self.state.window;
        let window = |[low, high]: [i32; 2], mask: u32, offset: u32| -> [i32; 2] {
            if mask == 0 {
                return [low, high];
            }
            let mapped = (low..=high).map(|t| ((t as u32 & !mask) | offset) as i32);
            [
                mapped.clone().min().unwrap_or(low),
                mapped.max().unwrap_or(high),
            ]
        };
        let windowed = mask_x != 0 || mask_y != 0;
        if windowed {
            self.stats.windowed += 1;
        }
        let u = window(sample.u, mask_x, offset_x);
        let v = window(sample.v, mask_y, offset_y);
        let per_word = mode.texels_per_word().unwrap_or(1) as i32;
        let base_x = ((sample.page & 15) * 64) as i32;
        let base_y = (((sample.page >> 4) & 1) * 256) as i32;
        let (x0, x1) = (base_x + u[0] / per_word, base_x + u[1] / per_word);
        let (y0, y1) = (base_y + v[0], base_y + v[1]);
        if x1 >= WIDTH as i32 || y1 >= HEIGHT as i32 {
            return self.miss("crosses the VRAM edge", coverage::SPANS);
        }
        let id = self.owner[y0 as usize * WIDTH + x0 as usize];
        if id == NONE {
            return self.miss(
                "not an upload (drawn, copied or never written)",
                coverage::RENDERED,
            );
        }
        let (ix, iy, iw, ih, live_words, lost, content) = {
            let instance = &self.instances[id as usize];
            (
                i32::from(instance.x),
                i32::from(instance.y),
                i32::from(instance.width),
                i32::from(instance.height),
                instance.live_words,
                instance.lost,
                instance.content,
            )
        };
        if x1 >= ix + iw || y1 >= iy + ih {
            *self.stats.spans.entry((iw as u16, ih as u16)).or_default() += 1;
            let place = [ix, iy, iw, ih].map(|v| v as u16);
            let places = &mut self.stats.span_places;
            if places.len() < SPAN_PLACES || places.contains_key(&place) {
                let entry = places.entry(place).or_insert((0, [x0, y0, x1, y1]));
                entry.0 += 1;
                let sampled = &mut entry.1;
                *sampled = [
                    sampled[0].min(x0),
                    sampled[1].min(y0),
                    sampled[2].max(x1),
                    sampled[3].max(y1),
                ];
            }
            return self.miss("spans several uploads", coverage::SPANS);
        }
        if live_words != (iw * ih) as u32 {
            let region = [x0 as u16, y0 as u16, x1 as u16, y1 as u16];
            if self.verified.get(&(id, region)) != Some(&lost) {
                let intact = (y0..=y1).all(|row| {
                    (x0..=x1).all(|column| self.owner[row as usize * WIDTH + column as usize] == id)
                });
                if !intact {
                    return self.miss("partly overwritten", coverage::OVERWRITTEN);
                }
                self.verified.insert((id, region), lost);
            }
        }
        let local = [
            (base_x - ix) * per_word + u[0],
            (base_y - iy) + v[0],
            (base_x - ix) * per_word + u[1],
            (base_y - iy) + v[1],
        ];
        let clut_x = ((sample.clut & 63) * 16) as u16;
        let clut_y = ((sample.clut >> 6) & 511) as u16;
        let palette = if mode.indexed() {
            let (lo, hi) = self.index_range(content, mode);
            self.palette(vram, clut_x, clut_y, lo, hi, mode.palette_entries())
        } else {
            NONE
        };
        self.stats.attributed += 1;
        let combo = match self.combo_ids.get(&(content, mode, palette)) {
            Some(&combo) => combo,
            None => {
                let combo = self.combos.len() as u32;
                self.combo_ids.insert((content, mode, palette), combo);
                let palette_source = (palette != NONE)
                    .then(|| {
                        let lo = usize::from(self.palettes[palette as usize].lo);
                        let at = usize::from(clut_y) * WIDTH + (usize::from(clut_x) + lo) % WIDTH;
                        let owner = self.owner[at];
                        (owner != NONE).then(|| {
                            let source = &self.instances[owner as usize];
                            let word = (usize::from(clut_x) + lo) % WIDTH;
                            (source.content, word as u16 - source.x, clut_y - source.y)
                        })
                    })
                    .flatten();
                if let Some((source, _, _)) = palette_source {
                    self.contents[source as usize].palette_uses += 1;
                }
                self.contents[content as usize].drawn = true;
                self.combos.push(Combo {
                    content,
                    mode,
                    palette,
                    draws: 0,
                    polygons: 0,
                    rectangles: 0,
                    semi_transparent: 0,
                    raw: 0,
                    region: local,
                    first_vblank: self.vblank,
                    first_state: self.game_state,
                    last_vblank: self.vblank,
                    cluts: Vec::new(),
                    palette_source,
                    sizes: BTreeMap::new(),
                    other_sizes: 0,
                });
                combo
            }
        };
        let entry = &mut self.combos[combo as usize];
        entry.draws += 1;
        entry.polygons += u64::from(sample.polygon);
        entry.rectangles += u64::from(!sample.polygon);
        entry.semi_transparent += u64::from(sample.semi);
        entry.raw += u64::from(sample.raw);
        entry.last_vblank = self.vblank;
        entry.region = [
            entry.region[0].min(local[0]),
            entry.region[1].min(local[1]),
            entry.region[2].max(local[2]),
            entry.region[3].max(local[3]),
        ];
        if mode.indexed()
            && entry.cluts.len() < LIST_LIMIT
            && !entry.cluts.contains(&(clut_x, clut_y))
        {
            entry.cluts.push((clut_x, clut_y));
        }
        let size = (sample.polygon, sample.size.0, sample.size.1);
        if entry.sizes.len() < SIZE_LIMIT || entry.sizes.contains_key(&size) {
            *entry.sizes.entry(size).or_default() += 1;
        } else {
            entry.other_sizes += 1;
        }
        if windowed {
            coverage::WINDOWED
        } else {
            coverage::TEXTURE + combo
        }
    }

    fn index_range(&mut self, content: u32, mode: Mode) -> (u8, u8) {
        let slot = usize::from(mode == Mode::P8);
        let entry = &mut self.contents[content as usize];
        if let Some(range) = entry.ranges[slot] {
            return range.unwrap_or((0, 0));
        }
        let range = entry
            .data
            .as_deref()
            .and_then(|data| key::index_range_words(data, mode));
        entry.ranges[slot] = Some(range);
        range.unwrap_or((0, 0))
    }

    fn palette(&mut self, vram: &Vram, x: u16, y: u16, lo: u8, hi: u8, entries: usize) -> u32 {
        let words = vram.words();
        let row = usize::from(y) * WIDTH;
        let read = |index: usize| words[row + (usize::from(x) + index) % WIDTH];
        let used: Vec<u16> = (usize::from(lo)..=usize::from(hi)).map(read).collect();
        if let Some((cached, id)) = self.palette_cache.get(&(x, y, lo, hi))
            && *cached == used
        {
            return *id;
        }
        let id = match self.palette_ids.get(&(lo, hi, used.clone())) {
            Some(&id) => id,
            None => {
                let id = self.palettes.len() as u32;
                let all: Vec<u16> = (0..entries).map(read).collect();
                self.palettes.push(Palette {
                    hash: key::palette_hash(lo, hi, &all),
                    lo,
                    hi,
                    entries: all,
                });
                self.palette_ids.insert((lo, hi, used.clone()), id);
                id
            }
        };
        self.palette_cache.insert((x, y, lo, hi), (used, id));
        id
    }

    /// The key of a dumped texture.
    fn key(&self, combo: &Combo) -> Key {
        let content = &self.contents[combo.content as usize];
        let per_word = combo.mode.texels_per_word().unwrap_or(1);
        Key {
            mode: combo.mode,
            texture: content.texture.clone(),
            palette: if combo.palette == NONE {
                PaletteKey::None
            } else {
                PaletteKey::Hash(self.palettes[combo.palette as usize].hash.clone())
            },
            width: usize::from(content.width) * per_word,
            height: usize::from(content.height),
        }
    }

    /// Write `dir/textures/<state>/<key>.png` for every texture seen drawn
    /// and `dir/manifest.json`; returns the manifest's summary.
    pub fn write(&self, dir: &Path) -> Result<Value> {
        let root = dir.join("textures");
        fs::create_dir_all(&root)?;
        let mut textures = Vec::with_capacity(self.combos.len());
        let mut png_bytes = 0usize;
        let mut by_mode: BTreeMap<&str, u64> = BTreeMap::new();
        let mut by_state: BTreeMap<String, u64> = BTreeMap::new();
        // Palettes seen per (upload content, mode): animated palettes.
        let mut variants: HashMap<(u32, Mode), u64> = HashMap::new();
        for combo in &self.combos {
            *variants.entry((combo.content, combo.mode)).or_default() += 1;
        }
        for combo in &self.combos {
            let content = &self.contents[combo.content as usize];
            let data = content
                .data
                .as_deref()
                .ok_or("a drawn texture lost its pixels")?;
            let key = self.key(combo);
            let (width, height) = (key.width, key.height);
            let png = match combo.mode {
                Mode::P4 | Mode::P8 => {
                    let indices: Vec<u8> = if combo.mode == Mode::P4 {
                        data.iter()
                            .flat_map(|&w| [0, 4, 8, 12].map(|s| ((w >> s) & 15) as u8))
                            .collect()
                    } else {
                        data.iter().flat_map(|w| w.to_le_bytes()).collect()
                    };
                    // A palette first read for the other indexed mode (same
                    // entries lo..=hi) may be longer than this mode's.
                    let palette = &self.palettes[combo.palette as usize].entries;
                    let entries = palette.len().min(combo.mode.palette_entries());
                    png_io::encode(
                        width,
                        height,
                        Pixels::Indexed {
                            bits: if combo.mode == Mode::P4 { 4 } else { 8 },
                            indices: &indices,
                            palette: &palette[..entries],
                        },
                    )?
                }
                Mode::C16 | Mode::C24 => png_io::encode(width, height, Pixels::Color15(data))?,
            };
            let folder = state_folder(combo.first_state);
            let stem = key.to_string();
            let relative = format!("textures/{folder}/{stem}.png");
            fs::create_dir_all(root.join(&folder))?;
            fs::write(dir.join(&relative), &png)?;
            png_bytes += png.len();
            *by_mode.entry(combo.mode.name()).or_default() += 1;
            *by_state.entry(folder).or_default() += 1;
            let palette = (combo.palette != NONE).then(|| {
                let palette = &self.palettes[combo.palette as usize];
                json!({
                    "hash": palette.hash,
                    "index_range": [palette.lo, palette.hi],
                    "cluts": combo.cluts.iter().map(|&(x, y)| json!([x, y])).collect::<Vec<_>>(),
                    "source": combo.palette_source.map(|(source, x, y)| json!({
                        "texture": self.contents[source as usize].texture,
                        "x": x,
                        "y": y,
                    })),
                })
            });
            textures.push(json!({
                "key": stem,
                "file": relative,
                "sha256": crate::sha256(&png),
                "mode": combo.mode.name(),
                "texture": content.texture,
                "width": width,
                "height": height,
                "upload": {"w": content.width, "h": content.height,
                    "positions": content.positions.iter().map(|&(x, y)| json!([x, y])).collect::<Vec<_>>()},
                "palette": palette,
                "palette_variants": variants[&(combo.content, combo.mode)],
                "sampled_region": combo.region,
                "draws": combo.draws,
                "polygons": combo.polygons,
                "rectangles": combo.rectangles,
                "semi_transparent_draws": combo.semi_transparent,
                "raw_draws": combo.raw,
                "sizes": sizes_json(combo),
                "first_vblank": combo.first_vblank,
                "last_vblank": combo.last_vblank,
                "first_state": combo.first_state,
            }));
        }
        // Uploads that served as a texture or a palette, in full; the rest
        // (movie frames, saved screen areas, unused data) by size only, so
        // long runs with movies keep a small manifest.
        let mut other: BTreeMap<String, [u64; 2]> = BTreeMap::new();
        for content in self
            .contents
            .iter()
            .filter(|c| !c.drawn && c.palette_uses == 0)
        {
            let entry = other
                .entry(format!("{}x{}", content.width, content.height))
                .or_default();
            entry[0] += 1;
            entry[1] += content.uploads;
        }
        let other: serde_json::Map<String, Value> = other
            .into_iter()
            .map(|(size, [contents, uploads])| {
                (size, json!({"contents": contents, "uploads": uploads}))
            })
            .collect();
        let uploads: Vec<Value> = self
            .contents
            .iter()
            .filter(|c| c.drawn || c.palette_uses > 0)
            .map(|content| {
                json!({
                    "texture": content.texture,
                    "w": content.width,
                    "h": content.height,
                    "uploads": content.uploads,
                    "positions": content.positions.iter().map(|&(x, y)| json!([x, y])).collect::<Vec<_>>(),
                    "first_vblank": content.first_vblank,
                    "first_state": content.first_state,
                    "drawn": content.drawn,
                    "palette_uses": content.palette_uses,
                })
            })
            .collect();
        let stats = &self.stats;
        let summary = json!({
            "textures": self.combos.len(),
            "by_mode": by_mode,
            "by_state": by_state,
            "png_bytes": png_bytes,
            "palettes": self.palettes.len(),
            "upload_contents": self.contents.len(),
            "uploads": stats.uploads,
            "upload_bytes": stats.upload_words * 2,
            "skipped_uploads": stats.skipped_uploads,
            "uploads_inside_lists": stats.list_uploads,
            "packets": stats.packets,
            "commands": stats.commands,
            "incomplete_packets": stats.incomplete,
            "fills": stats.fills,
            "copies": stats.copies,
            "textured_primitives": stats.textured,
            "attributed": stats.attributed,
            "windowed": stats.windowed,
            "unattributed": stats.unattributed,
            "released_contents": stats.released_contents,
            "spans_by_first_upload": stats
                .spans
                .iter()
                .map(|(&(w, h), &count)| (format!("{w}x{h}"), json!(count)))
                .collect::<serde_json::Map<String, Value>>(),
            "span_places": span_places(&stats.span_places),
        });
        let manifest = json!({
            "schema": "ac3-asset-dump/runtime/1",
            "alpha": super::ALPHA_NOTE,
            "key": super::KEY_NOTE,
            "summary": summary,
            "textures": textures,
            "uploads": uploads,
            "other_uploads": other,
        });
        fs::write(
            dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)? + "\n",
        )?;
        let keys: Vec<String> = self
            .combos
            .iter()
            .map(|combo| self.key(combo).to_string())
            .collect();
        let coverage = self
            .coverage
            .report(&|texture| keys[texture as usize].clone(), &state_folder);
        fs::write(
            dir.join("coverage.json"),
            serde_json::to_string(&coverage)? + "\n",
        )?;
        if !self.coverage.maps().is_empty() {
            fs::create_dir_all(dir.join("coverage"))?;
        }
        for map in self.coverage.maps() {
            let png = png_io::encode(
                map.width,
                map.height,
                Pixels::Indexed {
                    bits: 8,
                    indices: &map.classes,
                    palette: &coverage::MAP_COLOURS,
                },
            )?;
            let name = format!(
                "coverage/vblank-{:06}-{}.png",
                map.vblank,
                state_folder(map.state)
            );
            fs::write(dir.join(name), png)?;
        }
        let mut summary = summary;
        summary["coverage_fields"] = json!(self.coverage.fields());
        Ok(summary)
    }
}

/// The upload rectangles where most spanning primitives start (the 64
/// most), with the VRAM words those primitives sampled.
fn span_places(places: &HashMap<[u16; 4], (u64, [i32; 4])>) -> Value {
    let mut places: Vec<_> = places.iter().collect();
    places.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    places
        .into_iter()
        .take(64)
        .map(|(upload, (primitives, sampled))| {
            json!({"first_upload": upload, "sampled": sampled, "primitives": primitives})
        })
        .collect()
}

/// A texture's draws by on-screen size, most frequent first: `"s16x16"`
/// for a 16 x 16 sprite, `"p24x8"` for a polygon with a 24 x 8 bounding box.
fn sizes_json(combo: &Combo) -> Value {
    let mut sizes: Vec<(&(bool, u16, u16), &u64)> = combo.sizes.iter().collect();
    sizes.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut map: serde_json::Map<String, Value> = sizes
        .into_iter()
        .map(|(&(polygon, w, h), &count)| {
            let kind = if polygon { 'p' } else { 's' };
            (format!("{kind}{w}x{h}"), json!(count))
        })
        .collect();
    if combo.other_sizes > 0 {
        map.insert("other".to_owned(), json!(combo.other_sizes));
    }
    Value::Object(map)
}

/// Folder of a texture by the game state that first drew it.
pub fn state_folder(state: u32) -> String {
    let name = match state {
        2 => "flight",
        3 => "hub",
        4 => "menus",
        5 => "movie",
        6 => "picture",
        7 => "credits",
        8 => "terminal",
        9 => "music",
        _ => "",
    };
    if name.is_empty() {
        format!("state{state}")
    } else {
        format!("state{state}-{name}")
    }
}

/// Words of the whole GP0 command at the start of `words`, as the GPU
/// counts them; `None` when the packet ends inside it.
fn command_length(words: &[u32]) -> Option<usize> {
    let op = (words[0] >> 24) as u8;
    let needed = match op {
        0x02 => 3,
        0x20..=0x3f => {
            let vertices = if op & 8 != 0 { 4 } else { 3 };
            let textured = usize::from(op & 4 != 0);
            1 + vertices * (1 + textured) + if op & 0x10 != 0 { vertices - 1 } else { 0 }
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
            return (first_two..words.len())
                .find(|&at| terminator(words[at]))
                .map(|at| at + 1);
        }
        0x60..=0x7f => 2 + usize::from(op & 4 != 0) + usize::from(op & 0x18 == 0),
        0x80..=0x9f => 4,
        0xa0..=0xbf => {
            let size = *words.get(2)?;
            let w = ((size & 0xffff).wrapping_sub(1) & 0x3ff) as usize + 1;
            let h = ((size >> 16).wrapping_sub(1) & 0x1ff) as usize + 1;
            3 + (w * h).div_ceil(2)
        }
        0xc0..=0xdf => 3,
        _ => 1,
    };
    (words.len() >= needed).then_some(needed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4-bit 2x4-word texture (8x4 texels) at (640, 0), its CLUT at (0, 480).
    fn setup() -> (TextureRecorder, Vram, Vec<u16>, Vec<u16>) {
        let pixels: Vec<u16> = (0..8u16)
            .map(|i| 0x3210 + ((i * 0x1111) % 0x4444))
            .collect();
        let clut: Vec<u16> = (0..16u16).map(|i| (i << 10) | (0x8000 * (i & 1))).collect();
        let mut recorder = TextureRecorder::new();
        let mut vram = Vram::new();
        recorder.set_frame(10, 4);
        for (rect, data) in [([640u16, 0, 2, 4], &pixels), ([0, 480, 16, 1], &clut)] {
            recorder.on_upload(rect, data);
            let bytes: Vec<u8> = data.iter().flat_map(|w| w.to_le_bytes()).collect();
            vram.load_block(&ac3_graphics::tim::Block {
                x: rect[0],
                y: rect[1],
                width: rect[2],
                height: rect[3],
                data: &bytes,
            })
            .unwrap();
        }
        (recorder, vram, pixels, clut)
    }

    /// E1 page (640, 0) 4-bit, then an 8x4 sprite at (10, 10), UV (0, 0),
    /// CLUT (0, 480).
    fn sprite_packet() -> Vec<u32> {
        let clut = (480 << 6) as u32;
        vec![
            0xe100_000a,
            0xe300_0000,
            0xe407_fbff,
            0x6400_0000 | 0x80_8080,
            (10 << 16) | 10,
            clut << 16,
            (4 << 16) | 8,
        ]
    }

    #[test]
    fn sprites_are_keyed_like_the_static_dump() {
        let (mut recorder, vram, pixels, clut) = setup();
        recorder.observe_gp0(&sprite_packet(), &vram);
        assert_eq!(recorder.textures(), 1);
        let combo = &recorder.combos[0];
        assert_eq!(combo.mode, Mode::P4);
        assert_eq!(combo.region, [0, 0, 7, 3]);
        assert_eq!(combo.first_state, 4);
        let key = recorder.key(combo);
        // The same texture through the static path (TIM blocks).
        let bytes: Vec<u8> = pixels.iter().flat_map(|w| w.to_le_bytes()).collect();
        let (lo, hi) = key::index_range(&bytes, Mode::P4).unwrap();
        assert_eq!(key.texture, key::texture_hash_bytes(2, 4, &bytes));
        assert_eq!(
            key.palette,
            PaletteKey::Hash(key::palette_hash(lo, hi, &clut))
        );
        assert_eq!((key.width, key.height), (8, 4));
        // The CLUT upload is recorded as this texture's palette source.
        assert_eq!(combo.palette_source, Some((1, 0, 0)));
        assert_eq!(recorder.contents[1].palette_uses, 1);
        // Drawing it again only counts.
        recorder.observe_gp0(&sprite_packet(), &vram);
        assert_eq!(recorder.textures(), 1);
        assert_eq!(recorder.combos[0].draws, 2);
    }

    #[test]
    fn overwritten_or_unloaded_texels_are_not_attributed() {
        let (mut recorder, vram, _, _) = setup();
        // A tile drawn with the drawing area over the texture (640, 2).
        let tile = [
            0xe300_0000 | 640,
            0xe400_0000 | (511 << 10) | 1023,
            0x6000_00ff,
            (2 << 16) | 640,
            (1 << 16) | 1,
        ];
        recorder.observe_gp0(&tile, &vram);
        recorder.observe_gp0(&sprite_packet(), &vram);
        assert_eq!(recorder.textures(), 0);
        assert_eq!(recorder.stats.unattributed["partly overwritten"], 1);
        // A sprite sampling words that were never uploaded.
        let mut far = sprite_packet();
        far[0] = 0xe100_000c;
        recorder.observe_gp0(&far, &vram);
        assert_eq!(
            recorder.stats.unattributed["not an upload (drawn, copied or never written)"],
            1
        );
        // Re-uploading restores attribution.
        let (fresh, ..) = setup();
        let mut recorder = fresh;
        recorder.observe_gp0(&sprite_packet(), &vram);
        assert_eq!(recorder.textures(), 1);
    }

    #[test]
    fn released_uploads_keep_drawn_pixels_only() {
        let mut recorder = TextureRecorder::new();
        let vram = Vram::new();
        recorder.on_upload([0, 0, 4, 1], &[1, 2, 3, 4]);
        recorder.on_upload([0, 0, 4, 1], &[5, 6, 7, 8]);
        assert!(recorder.contents[0].data.is_none(), "replaced, never drawn");
        assert_eq!(recorder.stats.released_contents, 1);
        // Draw the second as 15-bit, then replace it: its pixels stay.
        recorder.observe_gp0(&[0xe100_0100, 0x6500_0000, 0, 0, (1 << 16) | 4], &vram);
        assert_eq!(recorder.textures(), 1);
        recorder.on_upload([0, 0, 4, 1], &[9, 9, 9, 9]);
        assert!(recorder.contents[1].data.is_some());
        assert_eq!(recorder.key(&recorder.combos[0]).mode, Mode::C16);
    }

    #[test]
    fn command_lengths_follow_the_gpu() {
        assert_eq!(command_length(&[0x2c00_0000; 9]), Some(9));
        assert_eq!(command_length(&[0x3c00_0000; 12]), Some(12));
        assert_eq!(command_length(&[0x3c00_0000; 11]), None);
        assert_eq!(command_length(&[0x6400_0000, 0, 0, 0]), Some(4));
        assert_eq!(command_length(&[0x7c00_0000, 0, 0]), Some(3));
        assert_eq!(
            command_length(&[0x4800_0000, 0, 0, 0, 0x5555_5555]),
            Some(5)
        );
        assert_eq!(
            command_length(&[0xa000_0000, 0, 0x0001_0003, 0, 0]),
            Some(5)
        );
        assert_eq!(command_length(&[0xe100_0000]), Some(1));
    }

    #[test]
    fn manifest_and_pngs_are_written() {
        let (mut recorder, vram, pixels, _) = setup();
        recorder.observe_gp0(&sprite_packet(), &vram);
        let dir = std::env::temp_dir().join(format!("ac3-runtime-dump-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let summary = recorder.write(&dir).unwrap();
        assert_eq!(summary["textures"], 1);
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
        let file = manifest["textures"][0]["file"].as_str().unwrap();
        assert!(file.starts_with("textures/state4-menus/ac3tex-p4-"));
        let png = fs::read(dir.join(file)).unwrap();
        let png_io::Decoded::Indexed { indices, .. } = png_io::decode(&png).unwrap() else {
            panic!("indexed");
        };
        let expected: Vec<u8> = pixels
            .iter()
            .flat_map(|&w| [0, 4, 8, 12].map(|s| ((w >> s) & 15) as u8))
            .collect();
        assert_eq!(indices, expected);
        fs::remove_dir_all(&dir).unwrap();
    }
}
