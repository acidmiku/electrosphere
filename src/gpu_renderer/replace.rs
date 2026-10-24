//! Texture replacement, the recorder's half: the texture pack, which upload
//! each textured primitive samples, and the replacement it gets.
//!
//! - [`TexturePack`]: a folder of PNGs named by replacement key
//!   (`docs/ASSET_DUMP.md`), indexed once (by name only); each PNG is
//!   decoded the first time a primitive needs it.
//! - [`Replacer`]: ownership in stream order. For every VRAM word it keeps
//!   the upload that last wrote it, for the uploads whose content hash (the
//!   key's `T`) the pack names. Every other write (another upload, a
//!   primitive, a fill, a copy, a capture) clears the words it may touch. A
//!   textured primitive is replaced when every word its texture coordinates
//!   cover still belongs to one such upload, as the asset dump attributes
//!   draws (`asset_dump::TextureRecorder`), with the dump's key functions
//!   (`asset_dump::key`), so the keys are the dump's file names.
//!
//! Lookup: `ac3tex-<mode>-<T>-<P>-<W>x<H>` with the palette the primitive
//! names as it is in VRAM at that moment, then `-any-`, then `-nopal-` (a
//! static-dump image without a CLUT of its own, which can only mean every
//! palette), then the original texels. Primitives with a texture window,
//! or whose texels span two uploads or were drawn or copied, keep the
//! original.
//!
//! The card's half is [`super::atlas`]: the decoded images go to it with
//! the frame ([`take`](Replacer::take)) and each gets a slot, which the
//! primitive's vertices name.
//!
//! ## Movie frames
//! The hub's movie clips (`docs/MOVIES.md` §9) are textures that change 15
//! times a second: the hub player decodes each frame into VRAM in 16-pixel
//! slices and draws it with textured quads. When the movie tracker sees a
//! frame's last slice arrive, the host hands the frame to the replacer
//! ([`Replacer::movie_frame`]): its rectangle becomes owned by that frame,
//! exactly as an upload of a pack texture, so the same ownership and lookup
//! decide whether a primitive samples the game's frame unchanged. Its image
//! comes from the movie pack ([`MovieImages`]) the first time a primitive
//! draws it, as PlayStation texels ([`movie_texel`]: 15-bit like the
//! game's, which the card's VRAM holds), and goes to one of
//! [`MOVIE_SLOTS`] slots reserved for movie frames, reused as frames come
//! (never one a primitive of the pass being recorded uses). A frame whose
//! image is not decoded yet draws the game's texels.
use super::tiles::Rect;
use crate::asset_dump::key::{self, Key, Mode, PaletteKey};
use crate::movie_pack::RgbaImage;
use ac3_graphics::Vram;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const VRAM_WIDTH: usize = 1024;
const VRAM_HEIGHT: usize = 512;
const NONE: u32 = u32::MAX;
/// Owner-map tiles: 32 x 32 words, so writes far from any upload are cheap.
const TILE: usize = 32;
const TILES_X: usize = VRAM_WIDTH / TILE;

/// Alpha below this is transparent (the colour 0x0000).
pub const TRANSPARENT_BELOW: u8 = 64;
/// Alpha from this up is opaque (STP clear); between the two, STP set.
pub const OPAQUE_FROM: u8 = 192;
/// What opaque black becomes: 0x0000 is transparent on the PlayStation, so
/// the darkest blue (1 of 31) stands in.
pub const OPAQUE_BLACK: u16 = 0x0400;
/// Replacement slots: a vertex names slot + 1 in 16 bits of its mode word.
pub const MAX_SLOTS: u32 = 0xffff;
/// Slots for movie frames, the last ones; pack textures take the rest.
pub const MOVIE_SLOTS: u32 = 8;
pub const FIRST_MOVIE_SLOT: u32 = MAX_SLOTS - MOVIE_SLOTS;

/// Whether `slot` holds movie frames (its image changes as frames come).
pub fn is_movie_slot(slot: u32) -> bool {
    (FIRST_MOVIE_SLOT..MAX_SLOTS).contains(&slot)
}
/// The largest PNG side decoded.
pub const MAX_SIDE: u32 = 8192;
/// Upload rectangles remembered to find textures again after a re-seed.
const HISTORY: usize = 8192;
/// VRAM words hashed at most by a re-seed (16 times all of VRAM).
const RESEED_WORDS: usize = 16 * VRAM_WIDTH * VRAM_HEIGHT;
/// Folder depth scanned below the pack folder.
const DEPTH: usize = 16;

fn hex(text: &str) -> Option<u64> {
    u64::from_str_radix(text, 16).ok()
}

/// `T` of an upload (`key::texture_hash_words`) as a number.
fn texture_hash(width: u16, height: u16, words: &[u16]) -> u64 {
    hex(&key::texture_hash_words(width, height, words)).unwrap_or(0)
}

/// The PlayStation texel of a replacement's RGBA8 pixel: alpha below
/// [`TRANSPARENT_BELOW`] is transparent, [`OPAQUE_FROM`] and up opaque (STP
/// clear), anything between STP set (blended by semi-transparent
/// primitives). Channels round to 5 bits, which returns every colour of
/// the dump's convention exactly. Opaque black becomes [`OPAQUE_BLACK`].
pub fn texel_of([r, g, b, a]: [u8; 4]) -> u16 {
    if a < TRANSPARENT_BELOW {
        return 0;
    }
    match (a >= OPAQUE_FROM, rgb15([r, g, b])) {
        (true, 0) => OPAQUE_BLACK,
        (true, word) => word,
        (false, word) => word | 0x8000,
    }
}

/// 8-bit channels rounded to 5 bits (the inverse of the screen's
/// `v << 3 | v >> 2` for every 5-bit value).
fn rgb15([r, g, b]: [u8; 3]) -> u16 {
    let five = |c: u8| (u16::from(c) * 31 + 127) / 255;
    five(r) | (five(g) << 5) | (five(b) << 10)
}

/// The PlayStation texel of a movie frame's pixel (alpha ignored: frames
/// are opaque) with the MDEC's STP bit `stp`, as the game's frame has it.
/// Without it, black (transparent as a texel) becomes [`OPAQUE_BLACK`].
pub fn movie_texel([r, g, b, _]: [u8; 4], stp: bool) -> u16 {
    match (stp, rgb15([r, g, b])) {
        (true, word) => word | 0x8000,
        (false, 0) => OPAQUE_BLACK,
        (false, word) => word,
    }
}

/// A replacement PNG as the card keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    /// Palette indices, drawn through the live palette, instead of colours.
    pub indexed: bool,
    /// PlayStation colours ([`texel_of`]) or indices, row by row.
    pub texels: Vec<u16>,
}

/// Decode the PNG of `key`. An indexed PNG keyed `any` or `nopal` for a
/// 4/8-bit texture keeps its indices, which are drawn through the palette
/// in VRAM at draw time (its own `PLTE` is not used). Every other PNG
/// (true colour, grey, or indexed with an exact palette key) is drawn in
/// its own colours.
pub fn decode(bytes: &[u8], key: &Key) -> Result<Decoded, String> {
    let live = key.mode.indexed() && matches!(key.palette, PaletteKey::Any | PaletteKey::Missing);
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    let info = decoder.read_header_info().map_err(|e| e.to_string())?;
    let (width, height) = (info.width, info.height);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!(
            "{width}x{height} pixels; use 1..{MAX_SIDE} on each side"
        ));
    }
    let indexed = live && info.color_type == png::ColorType::Indexed;
    decoder.set_transformations(if indexed {
        png::Transformations::IDENTITY
    } else {
        png::Transformations::normalize_to_color8()
    });
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or("the image is too large")?;
    let mut buffer = vec![0; size];
    let frame = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let w = width as usize;
    let rows = buffer.chunks(frame.line_size.max(1)).take(height as usize);
    let mut texels = Vec::with_capacity(w * height as usize);
    if indexed {
        let bits: usize = match frame.bit_depth {
            png::BitDepth::One => 1,
            png::BitDepth::Two => 2,
            png::BitDepth::Four => 4,
            png::BitDepth::Eight => 8,
            png::BitDepth::Sixteen => return Err("16-bit indices".into()),
        };
        let per_byte = 8 / bits;
        let mask = (1u16 << bits) - 1;
        for row in rows {
            texels.extend((0..w).map(|x| {
                // The leftmost pixel is in the high bits.
                let shift = 8 - bits * (x % per_byte + 1);
                (u16::from(row[x / per_byte]) >> shift) & mask
            }));
        }
    } else {
        if frame.bit_depth != png::BitDepth::Eight {
            return Err(format!("unexpected {:?} output", frame.bit_depth));
        }
        let channels = match frame.color_type {
            png::ColorType::Grayscale => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Indexed => return Err("palette not expanded".into()),
        };
        for row in rows {
            texels.extend(row[..w * channels].chunks_exact(channels).map(|p| {
                texel_of(match *p {
                    [g] => [g, g, g, 255],
                    [g, a] => [g, g, g, a],
                    [r, g, b] => [r, g, b, 255],
                    [r, g, b, a] => [r, g, b, a],
                    _ => [0; 4],
                })
            }));
        }
    }
    if texels.len() != w * height as usize {
        return Err("the image data is short".into());
    }
    Ok(Decoded {
        width,
        height,
        indexed,
        texels,
    })
}

// ---- the pack ---------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    /// Not decoded yet.
    Waiting,
    Slot(u32),
    /// Could not be read or decoded (logged once); the original is drawn.
    Failed,
}

#[derive(Debug)]
struct PackFile {
    path: PathBuf,
    key: Key,
    load: Load,
}

/// The pack's files for one upload content (`T`) in one mode.
#[derive(Debug, Default)]
struct Entry {
    /// Size in texels (the keys' `W` x `H`).
    width: usize,
    height: usize,
    /// By palette hash `P`.
    exact: HashMap<u64, usize>,
    any: Option<usize>,
    nopal: Option<usize>,
    /// 15-bit textures (no palette).
    direct: Option<usize>,
}

impl Entry {
    /// The lookup order: the exact palette, `any`, `nopal`; or the 15-bit file.
    fn choose(&self, palette: Option<u64>) -> Option<usize> {
        palette
            .and_then(|p| self.exact.get(&p).copied())
            .or(self.any)
            .or(self.nopal)
            .or(self.direct)
    }
}

/// A texture pack: the PNGs in a folder and its subfolders, by the
/// replacement key at the end of their names.
#[derive(Debug)]
pub struct TexturePack {
    root: PathBuf,
    files: Vec<PackFile>,
    entries: HashMap<(Mode, u64), Entry>,
    /// Every key's `T`.
    textures: HashSet<u64>,
    /// Upload sizes in VRAM words (width, height) the keys name.
    sizes: HashSet<(u16, u16)>,
    /// Where the pack's textures were uploaded before ([`POSITIONS`]).
    known: HashSet<(u64, [u16; 4])>,
    /// PNGs that are not replacements, with the reason.
    pub skipped: Vec<(PathBuf, &'static str)>,
}

/// The file in a pack folder that lists where the game uploaded the pack's
/// textures (one `T x y w h` line each, VRAM words), kept by the game. A
/// re-seed (a quick load) looks there too, so a state saved in another
/// session finds its textures at once. Deleting it is harmless.
pub const POSITIONS: &str = "ac3-texture-positions.txt";

/// The [`POSITIONS`] lines of `text` that name a texture of the pack.
fn parse_positions(text: &str) -> Vec<(u64, [u16; 4])> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let texture = hex(fields.next()?)?;
            let mut area = [0u16; 4];
            for value in &mut area {
                *value = fields.next()?.parse().ok()?;
            }
            let [x, y, w, h] = area.map(usize::from);
            (w > 0 && h > 0 && x + w <= VRAM_WIDTH && y + h <= VRAM_HEIGHT)
                .then_some((texture, area))
        })
        .collect()
}

/// Every `.png` below `folder` (at most [`DEPTH`] folders down).
fn collect(folder: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < DEPTH {
                collect(&path, out, depth + 1);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
        {
            out.push(path);
        }
    }
}

impl TexturePack {
    /// Index the PNGs of `root` by their names (nothing is decoded yet).
    pub fn open(root: &Path) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("{} is not a folder", root.display()));
        }
        let mut paths = Vec::new();
        collect(root, &mut paths, 0);
        paths.sort();
        let mut pack = Self {
            root: root.to_owned(),
            files: Vec::new(),
            entries: HashMap::new(),
            textures: HashSet::new(),
            sizes: HashSet::new(),
            known: HashSet::new(),
            skipped: Vec::new(),
        };
        for path in paths {
            if let Err(reason) = pack.add(&path) {
                pack.skipped.push((path, reason));
            }
        }
        if let Ok(text) = std::fs::read_to_string(root.join(POSITIONS)) {
            pack.known = parse_positions(&text)
                .into_iter()
                .filter(|(texture, [_, _, w, h])| {
                    pack.textures.contains(texture) && pack.sizes.contains(&(*w, *h))
                })
                .collect();
        }
        Ok(pack)
    }

    fn add(&mut self, path: &Path) -> Result<(), &'static str> {
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or("the name is not text")?;
        let key = Key::from_stem(stem).ok_or("no replacement key (ac3tex-...) in the name")?;
        let per_word = key
            .mode
            .texels_per_word()
            .ok_or("24-bit images are not textures")?;
        let texture = hex(&key.texture).ok_or("bad texture hash")?;
        if key.width == 0
            || key.height == 0
            || !key.width.is_multiple_of(per_word)
            || key.width / per_word > VRAM_WIDTH
            || key.height > VRAM_HEIGHT
        {
            return Err("the size in the name is not an upload's");
        }
        let index = self.files.len();
        let entry = self
            .entries
            .entry((key.mode, texture))
            .or_insert_with(|| Entry {
                width: key.width,
                height: key.height,
                ..Entry::default()
            });
        if (entry.width, entry.height) != (key.width, key.height) {
            return Err("another file names this texture with another size");
        }
        // The first file of a key (in path order) is the one used.
        const TAKEN: &str = "an earlier file has the same key";
        match &key.palette {
            PaletteKey::Hash(palette) => {
                let palette = hex(palette).ok_or("bad palette hash")?;
                match entry.exact.entry(palette) {
                    std::collections::hash_map::Entry::Occupied(_) => return Err(TAKEN),
                    std::collections::hash_map::Entry::Vacant(free) => {
                        free.insert(index);
                    }
                }
            }
            PaletteKey::Any | PaletteKey::Missing | PaletteKey::None => {
                let slot = match key.palette {
                    PaletteKey::Any => &mut entry.any,
                    PaletteKey::Missing => &mut entry.nopal,
                    _ => &mut entry.direct,
                };
                if slot.is_some() {
                    return Err(TAKEN);
                }
                *slot = Some(index);
            }
        }
        self.textures.insert(texture);
        self.sizes
            .insert(((key.width / per_word) as u16, key.height as u16));
        self.files.push(PackFile {
            path: path.to_owned(),
            key,
            load: Load::Waiting,
        });
        Ok(())
    }

    /// No texture pack: a replacer for movie frames only.
    pub fn none() -> Self {
        Self {
            root: PathBuf::new(),
            files: Vec::new(),
            entries: HashMap::new(),
            textures: HashSet::new(),
            sizes: HashSet::new(),
            known: HashSet::new(),
            skipped: Vec::new(),
        }
    }
    /// Whether this is [`Self::none`].
    pub fn is_none(&self) -> bool {
        self.root.as_os_str().is_empty()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Replacement files.
    pub fn len(&self) -> usize {
        self.files.len()
    }
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The log line of an opened pack.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{}: {} replacement{} for {} texture{}",
            self.root.display(),
            self.files.len(),
            if self.files.len() == 1 { "" } else { "s" },
            self.textures.len(),
            if self.textures.len() == 1 { "" } else { "s" },
        );
        if !self.skipped.is_empty() {
            let (path, reason) = &self.skipped[0];
            text += &format!(
                "; {} PNG{} skipped (e.g. {}: {reason})",
                self.skipped.len(),
                if self.skipped.len() == 1 { "" } else { "s" },
                path.strip_prefix(&self.root).unwrap_or(path).display()
            );
        }
        text
    }
}

// ---- ownership and lookup -----------------------------------------------------------------------

/// A replacement decoded for the card, with its slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub slot: u32,
    /// The key (for messages).
    pub key: String,
    /// The original's size in texels (the key's `W` x `H`).
    pub original: [u32; 2],
    pub image: Decoded,
}

/// One tracked upload: a content the pack names at a VRAM rectangle, or a
/// movie frame there.
#[derive(Clone, Copy, Debug)]
struct Instance {
    /// The content's `T`, or with `movie` the frame's serial.
    texture: u64,
    movie: bool,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    /// Words still owned.
    live: u32,
}

/// A movie frame the game decoded into VRAM (one of the hub's clips), to
/// draw from the movie pack's image while its texels are the game's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovieFrame {
    /// Its VRAM rectangle: 15-bit, a texel a word.
    pub rect: Rect,
    /// The movie (`movie_0810`) and the number of its image in the pack.
    pub key: Arc<str>,
    pub file: u32,
    /// The MDEC's STP bit on every texel.
    pub stp: bool,
}

/// The movie pack's decoded images, for [`MovieFrame`]s
/// (`movie_pack::HubImages`).
pub trait MovieImages {
    /// Image `file` of movie `key` once decoded; `None` while it is not
    /// (or when it cannot be).
    fn image(&self, key: &Arc<str>, file: u32) -> Option<Arc<RgbaImage>>;
}

/// A movie frame owning VRAM words.
struct MovieOwner {
    frame: MovieFrame,
    /// Its texels once the image came, and the movie slot holding them on
    /// the card.
    texels: Option<Decoded>,
    slot: Option<u32>,
}

/// A movie slot: the frame whose image it holds, and the last pass that
/// drew with it.
#[derive(Clone, Copy, Debug, Default)]
struct MovieSlot {
    frame: Option<u64>,
    used: u64,
}

/// What movie frame replacement has done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct MovieStats {
    /// Frames handed over ([`Replacer::movie_frame`]).
    pub frames: u64,
    /// Images sent to the card, and their texels.
    pub images: u64,
    pub texels: u64,
    /// Textured primitives drawn with a frame's image; those drawn with the
    /// game's texels because its image was not decoded yet; frames not
    /// replaced because every movie slot was in use by the pass.
    pub replaced: u64,
    pub late: u64,
    pub no_slot: u64,
}

/// A tracked upload content.
#[derive(Clone, Debug)]
struct Content {
    data: Vec<u16>,
    /// Index ranges for p4 and p8, once computed.
    ranges: [Option<Option<(u8, u8)>>; 2],
}

/// What texture replacement has done.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReplaceStats {
    /// Replacement files in the pack, and PNGs skipped.
    pub files: u64,
    pub skipped: u64,
    /// Uploads of a texture the pack names.
    pub tracked_uploads: u64,
    /// Replacements decoded, those that failed, their texels and time.
    pub loaded: u64,
    pub failed: u64,
    pub loaded_texels: u64,
    pub decode_micros: u64,
    /// Textured primitives drawn with a replacement.
    pub replaced: u64,
    /// Textured primitives drawn with the original texels: no replacement
    /// in the pack, or texels that are not one tracked upload's.
    pub originals: u64,
    /// Textured primitives with a texture window (drawn as the original).
    pub windowed: u64,
    pub reloads: u64,
}

/// What a textured primitive samples, without a texture window.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// 0: 4-bit, 1: 8-bit, 2: 15-bit.
    pub depth: u32,
    /// Texture page base in VRAM words.
    pub base: [i32; 2],
    /// Palette position in VRAM words.
    pub clut: [i32; 2],
    /// Texture coordinates used, inclusive.
    pub u: (i32, i32),
    pub v: (i32, i32),
}

/// The last palette read at a CLUT position (x, y) for an index range
/// (lo, hi): its entries lo..=hi and `P`.
type PaletteCache = HashMap<(u16, u16, u8, u8), (Vec<u16>, u64)>;

/// Ownership tracking and lookup; see the module documentation.
pub struct Replacer {
    pack: TexturePack,
    owner: Vec<u32>,
    tile_owned: Vec<u32>,
    instances: Vec<Instance>,
    free: Vec<u32>,
    contents: HashMap<u64, Content>,
    /// Upload rectangles (x, y, w, h) by the order they were last seen.
    history: HashMap<[u16; 4], u64>,
    sequence: u64,
    /// The last palette read at (x, y) for an index range: entries and `P`.
    palettes: PaletteCache,
    next_slot: u32,
    images: Vec<Image>,
    reset: bool,
    pub stats: ReplaceStats,
    /// Movie frames: their images, the frames owning words (by serial), the
    /// last serial, the movie slots, and the passes taken so far.
    movie_images: Option<Box<dyn MovieImages>>,
    movies: HashMap<u64, MovieOwner>,
    movie_serial: u64,
    movie_slots: [MovieSlot; MOVIE_SLOTS as usize],
    passes: u64,
    pub movie_stats: MovieStats,
}

/// The 8-bit coordinate ranges (inclusive) that `lo..=hi` reaches without
/// a texture window (`record::axis_ranges`).
fn ranges(lo: i32, hi: i32) -> Vec<(i32, i32)> {
    super::record::axis_ranges(lo, hi, 0, 0)
}

impl Replacer {
    pub fn new(pack: TexturePack) -> Self {
        let stats = ReplaceStats {
            files: pack.files.len() as u64,
            skipped: pack.skipped.len() as u64,
            ..ReplaceStats::default()
        };
        Self {
            pack,
            owner: vec![NONE; VRAM_WIDTH * VRAM_HEIGHT],
            tile_owned: vec![0; TILES_X * (VRAM_HEIGHT / TILE)],
            instances: Vec::new(),
            free: Vec::new(),
            contents: HashMap::new(),
            history: HashMap::new(),
            sequence: 0,
            palettes: HashMap::new(),
            next_slot: 0,
            images: Vec::new(),
            reset: false,
            stats,
            movie_images: None,
            movies: HashMap::new(),
            movie_serial: 0,
            movie_slots: [MovieSlot::default(); MOVIE_SLOTS as usize],
            passes: 0,
            movie_stats: MovieStats::default(),
        }
    }

    pub fn pack(&self) -> &TexturePack {
        &self.pack
    }

    /// Where movie frames' images come from (`None`: frames are not taken).
    pub fn set_movie_images(&mut self, images: Option<Box<dyn MovieImages>>) {
        self.movie_images = images;
    }
    pub fn take_movie_images(&mut self) -> Option<Box<dyn MovieImages>> {
        self.movie_images.take()
    }
    pub fn has_movie_images(&self) -> bool {
        self.movie_images.is_some()
    }

    /// The images decoded since the last call, and whether the slots
    /// started over (a reload) before them. A pass ends here.
    pub fn take(&mut self) -> (bool, Vec<Image>) {
        self.passes += 1;
        (
            std::mem::take(&mut self.reset),
            std::mem::take(&mut self.images),
        )
    }

    /// A CPU→VRAM transfer of `rect` (inside VRAM) with its pixels.
    pub fn upload(&mut self, rect: Rect, pixels: &[u16]) {
        self.written(&rect);
        let area = [rect.x, rect.y, rect.w, rect.h].map(|v| v as u16);
        let count = (rect.w * rect.h) as usize;
        if pixels.len() < count {
            return;
        }
        self.remember(area);
        if !self.pack.sizes.contains(&(area[2], area[3])) {
            return;
        }
        let pixels = &pixels[..count];
        let texture = texture_hash(area[2], area[3], pixels);
        if self.pack.textures.contains(&texture) {
            self.own(area, texture, pixels);
            self.note_position(texture, area);
        }
    }

    /// Add a new place of a pack texture to the pack's [`POSITIONS`] file
    /// (best effort: a folder that cannot be written is fine).
    fn note_position(&mut self, texture: u64, area: [u16; 4]) {
        if !self.pack.known.insert((texture, area)) {
            return;
        }
        use std::io::Write;
        let [x, y, w, h] = area;
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.pack.root.join(POSITIONS))
            .and_then(|mut file| writeln!(file, "{texture:016x} {x} {y} {w} {h}"));
    }

    fn remember(&mut self, area: [u16; 4]) {
        self.sequence += 1;
        self.history.insert(area, self.sequence);
        if self.history.len() > HISTORY {
            // Forget the older half.
            let mut order: Vec<u64> = self.history.values().copied().collect();
            order.sort_unstable();
            let keep = order[order.len() / 2];
            self.history.retain(|_, seen| *seen >= keep);
        }
    }

    fn own(&mut self, area: [u16; 4], texture: u64, pixels: &[u16]) {
        self.contents.entry(texture).or_insert_with(|| Content {
            data: pixels.to_vec(),
            ranges: [None; 2],
        });
        self.stats.tracked_uploads += 1;
        self.claim(area, texture, false);
    }

    /// A new instance (`texture`, or a movie frame's serial) owns every
    /// word of the area.
    fn claim(&mut self, [x, y, w, h]: [u16; 4], texture: u64, movie: bool) {
        let instance = Instance {
            texture,
            movie,
            x,
            y,
            w,
            h,
            live: 0,
        };
        let id = match self.free.pop() {
            Some(id) => {
                self.instances[id as usize] = instance;
                id
            }
            None => {
                self.instances.push(instance);
                (self.instances.len() - 1) as u32
            }
        };
        for row in usize::from(y)..usize::from(y + h) {
            for column in usize::from(x)..usize::from(x + w) {
                let at = row * VRAM_WIDTH + column;
                let previous = self.owner[at];
                if previous == NONE {
                    self.tile_owned[(row / TILE) * TILES_X + column / TILE] += 1;
                } else {
                    self.lose(previous);
                }
                self.owner[at] = id;
                self.instances[id as usize].live += 1;
            }
        }
    }

    fn lose(&mut self, id: u32) {
        let instance = &mut self.instances[id as usize];
        instance.live -= 1;
        if instance.live == 0 {
            self.free.push(id);
            if instance.movie {
                let serial = instance.texture;
                self.movies.remove(&serial);
                for slot in &mut self.movie_slots {
                    if slot.frame == Some(serial) {
                        slot.frame = None;
                    }
                }
            }
        }
    }

    /// A movie frame is complete in VRAM: its rectangle (inside VRAM)
    /// belongs to it from now on, until something else writes there.
    /// Ignored without [`MovieImages`].
    pub fn movie_frame(&mut self, mut frame: MovieFrame) {
        let rect = frame.rect.clipped();
        if self.movie_images.is_none() || rect.is_empty() {
            return;
        }
        frame.rect = rect;
        self.movie_serial += 1;
        let serial = self.movie_serial;
        self.movie_stats.frames += 1;
        self.claim(
            [rect.x, rect.y, rect.w, rect.h].map(|v| v as u16),
            serial,
            true,
        );
        self.movies.insert(
            serial,
            MovieOwner {
                frame,
                texels: None,
                slot: None,
            },
        );
    }

    /// The slot of movie frame `serial` for a primitive of this pass:
    /// the one holding its image, else a movie slot its image goes to now
    /// (fetched the first time). `None` (the game's texels): no image yet,
    /// or every movie slot is drawn by this pass.
    fn movie_lookup(&mut self, serial: u64) -> Option<u32> {
        let owner = self.movies.get_mut(&serial)?;
        if let Some(slot) = owner.slot {
            self.movie_slots[(slot - FIRST_MOVIE_SLOT) as usize].used = self.passes;
            self.movie_stats.replaced += 1;
            return Some(slot);
        }
        if owner.texels.is_none() {
            let frame = &owner.frame;
            let Some(image) = self
                .movie_images
                .as_ref()
                .and_then(|images| images.image(&frame.key, frame.file))
            else {
                self.movie_stats.late += 1;
                return None;
            };
            let texels = image
                .pixels
                .chunks_exact(4)
                .map(|p| movie_texel([p[0], p[1], p[2], p[3]], frame.stp))
                .collect();
            owner.texels = Some(Decoded {
                width: image.width,
                height: image.height,
                indexed: false,
                texels,
            });
        }
        // A free slot first, else the one drawn longest ago (not by this pass).
        let pass = self.passes;
        let Some(index) = (0..self.movie_slots.len())
            .filter(|&i| self.movie_slots[i].frame.is_none() || self.movie_slots[i].used < pass)
            .min_by_key(|&i| {
                (
                    self.movie_slots[i].frame.is_some(),
                    self.movie_slots[i].used,
                )
            })
        else {
            self.movie_stats.no_slot += 1;
            return None;
        };
        let slot = FIRST_MOVIE_SLOT + index as u32;
        let previous = std::mem::replace(
            &mut self.movie_slots[index],
            MovieSlot {
                frame: Some(serial),
                used: pass,
            },
        );
        if let Some(other) = previous.frame.and_then(|f| self.movies.get_mut(&f)) {
            other.slot = None;
        }
        let owner = self.movies.get_mut(&serial).expect("looked up");
        owner.slot = Some(slot);
        let image = owner.texels.clone().expect("fetched");
        let rect = owner.frame.rect;
        self.movie_stats.images += 1;
        self.movie_stats.texels += image.texels.len() as u64;
        self.movie_stats.replaced += 1;
        self.images.push(Image {
            slot,
            key: format!("{} image {}", owner.frame.key, owner.frame.file),
            original: [rect.w as u32, rect.h as u32],
            image,
        });
        Some(slot)
    }

    /// Every word of `rect` (clipped to VRAM) may have been written by
    /// something else than its upload.
    pub fn written(&mut self, rect: &Rect) {
        let rect = rect.clipped();
        if rect.is_empty() {
            return;
        }
        let (x0, y0) = (rect.x as usize, rect.y as usize);
        let (x1, y1) = (rect.right() as usize - 1, rect.bottom() as usize - 1);
        for tile_y in y0 / TILE..=y1 / TILE {
            for tile_x in x0 / TILE..=x1 / TILE {
                let tile = tile_y * TILES_X + tile_x;
                if self.tile_owned[tile] == 0 {
                    continue;
                }
                for row in (tile_y * TILE).max(y0)..=(tile_y * TILE + TILE - 1).min(y1) {
                    for column in (tile_x * TILE).max(x0)..=(tile_x * TILE + TILE - 1).min(x1) {
                        let at = row * VRAM_WIDTH + column;
                        let previous = self.owner[at];
                        if previous != NONE {
                            self.owner[at] = NONE;
                            self.tile_owned[tile] -= 1;
                            self.lose(previous);
                        }
                    }
                }
            }
        }
    }

    /// Start over from `vram` (a quick load, or a new pack): every upload
    /// rectangle seen lately whose VRAM content is a texture of the pack is
    /// owned again; the rest is unknown (movie frames too, until the next
    /// one comes).
    pub fn reseed(&mut self, vram: &Vram) {
        self.owner.fill(NONE);
        self.tile_owned.fill(0);
        self.instances.clear();
        self.free.clear();
        self.movies.clear();
        self.movie_slots = [MovieSlot::default(); MOVIE_SLOTS as usize];
        // Upload rectangles seen in this session, and before it (order 0).
        let mut seen: Vec<([u16; 4], u64)> = self
            .history
            .iter()
            .filter(|(area, _)| self.pack.sizes.contains(&(area[2], area[3])))
            .map(|(&area, &order)| (area, order))
            .collect();
        let mut earlier: Vec<[u16; 4]> = self
            .pack
            .known
            .iter()
            .map(|&(_, area)| area)
            .filter(|area| !self.history.contains_key(area))
            .collect();
        earlier.sort_unstable();
        earlier.dedup();
        seen.extend(earlier.into_iter().map(|area| (area, 0)));
        // The latest ones, up to RESEED_WORDS hashed, then in upload order
        // (a later upload over an earlier one owns the words).
        seen.sort_unstable_by_key(|&(_, order)| std::cmp::Reverse(order));
        let mut budget = RESEED_WORDS;
        seen.retain(|&([_, _, w, h], _)| {
            let size = usize::from(w) * usize::from(h);
            let keep = size <= budget;
            budget = budget.saturating_sub(size);
            keep
        });
        seen.reverse();
        let words = vram.words();
        for ([x, y, w, h], _) in seen {
            let pixels: Vec<u16> = (usize::from(y)..usize::from(y + h))
                .flat_map(|row| {
                    let start = row * VRAM_WIDTH + usize::from(x);
                    words[start..start + usize::from(w)].iter().copied()
                })
                .collect();
            let texture = texture_hash(w, h, &pixels);
            if self.pack.textures.contains(&texture) {
                self.own([x, y, w, h], texture, &pixels);
            }
        }
    }

    /// Read the pack's folder again (F9): every replacement is decoded
    /// again when next drawn, and textures still in VRAM are found again.
    pub fn reload(&mut self, vram: &Vram) -> Result<String, String> {
        let pack = TexturePack::open(&self.pack.root)?;
        let summary = pack.summary();
        self.stats.files = pack.files.len() as u64;
        self.stats.skipped = pack.skipped.len() as u64;
        self.stats.reloads += 1;
        self.pack = pack;
        self.next_slot = 0;
        self.images.clear();
        self.reset = true;
        self.reseed(vram);
        Ok(summary)
    }

    /// The replacement for a primitive sampling `sample` (no texture
    /// window), with the position of its upload's first texel in the
    /// page's texture coordinates; `vram` holds the palette it names.
    pub fn lookup(&mut self, sample: &Sample, vram: &Vram) -> Option<(u32, [i32; 2])> {
        let mode = match sample.depth {
            0 => Mode::P4,
            1 => Mode::P8,
            _ => Mode::C16,
        };
        let per_word = mode.texels_per_word()? as i32;
        let shift = per_word.trailing_zeros() as i32;
        let [base_x, base_y] = sample.base;
        let mut found = None;
        for &(u0, u1) in &ranges(sample.u.0, sample.u.1) {
            for &(v0, v1) in &ranges(sample.v.0, sample.v.1) {
                let (x0, x1) = (base_x + (u0 >> shift), base_x + (u1 >> shift));
                let (y0, y1) = (base_y + v0, base_y + v1);
                if x0 < 0 || y0 < 0 || x1 >= VRAM_WIDTH as i32 || y1 >= VRAM_HEIGHT as i32 {
                    return None;
                }
                let id = self.owner[y0 as usize * VRAM_WIDTH + x0 as usize];
                if id == NONE || found.is_some_and(|other| other != id) {
                    return None;
                }
                found = Some(id);
                let instance = self.instances[id as usize];
                if instance.movie && mode != Mode::C16 {
                    return None;
                }
                let (ix, iy) = (i32::from(instance.x), i32::from(instance.y));
                let (iw, ih) = (i32::from(instance.w), i32::from(instance.h));
                if x1 >= ix + iw || y1 >= iy + ih || x0 < ix || y0 < iy {
                    return None;
                }
                if instance.live != (iw * ih) as u32 {
                    let intact = (y0..=y1).all(|row| {
                        (x0..=x1).all(|column| {
                            self.owner[row as usize * VRAM_WIDTH + column as usize] == id
                        })
                    });
                    if !intact {
                        return None;
                    }
                }
            }
        }
        let instance = self.instances[found? as usize];
        let origin = [
            (i32::from(instance.x) - base_x) * per_word,
            i32::from(instance.y) - base_y,
        ];
        if instance.movie {
            return Some((self.movie_lookup(instance.texture)?, origin));
        }
        let entry_key = (mode, instance.texture);
        let entry = self.pack.entries.get(&entry_key)?;
        let size = (
            usize::from(instance.w) * per_word as usize,
            usize::from(instance.h),
        );
        if (entry.width, entry.height) != size {
            return None;
        }
        let palette = if mode.indexed() && !entry.exact.is_empty() {
            self.palette_hash(instance.texture, mode, sample.clut, vram)
        } else {
            None
        };
        let file = self.pack.entries.get(&entry_key)?.choose(palette)?;
        let slot = self.load(file)?;
        self.stats.replaced += 1;
        Some((slot, origin))
    }

    /// `P` of the palette at `clut` (VRAM words) for upload `texture` in
    /// `mode`, read from `vram` as the dump reads it.
    fn palette_hash(
        &mut self,
        texture: u64,
        mode: Mode,
        clut: [i32; 2],
        vram: &Vram,
    ) -> Option<u64> {
        let slot = usize::from(mode == Mode::P8);
        let content = self.contents.get_mut(&texture)?;
        let (lo, hi) = *content.ranges[slot]
            .get_or_insert_with(|| key::index_range_words(&content.data, mode))
            .as_ref()?;
        let (x, y) = (
            (clut[0] as usize) % VRAM_WIDTH,
            (clut[1] as usize) % VRAM_HEIGHT,
        );
        let words = vram.words();
        let read = |index: usize| words[y * VRAM_WIDTH + (x + index) % VRAM_WIDTH];
        let cache = (x as u16, y as u16, lo, hi);
        if let Some((entries, hash)) = self.palettes.get(&cache)
            && entries
                .iter()
                .enumerate()
                .all(|(i, &entry)| read(usize::from(lo) + i) == entry)
        {
            return Some(*hash);
        }
        let mut entries = vec![0u16; usize::from(hi) + 1];
        for (index, entry) in entries.iter_mut().enumerate().skip(usize::from(lo)) {
            *entry = read(index);
        }
        let hash = hex(&key::palette_hash(lo, hi, &entries))?;
        self.palettes
            .insert(cache, (entries[usize::from(lo)..].to_vec(), hash));
        Some(hash)
    }

    /// The slot of pack file `file`, decoding it the first time.
    fn load(&mut self, file: usize) -> Option<u32> {
        match self.pack.files[file].load {
            Load::Slot(slot) => return Some(slot),
            Load::Failed => return None,
            Load::Waiting => {}
        }
        let started = std::time::Instant::now();
        let entry = &self.pack.files[file];
        let result = if self.next_slot >= FIRST_MOVIE_SLOT {
            Err(format!("more than {FIRST_MOVIE_SLOT} replacements in use"))
        } else {
            std::fs::read(&entry.path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| decode(&bytes, &entry.key))
        };
        self.stats.decode_micros += started.elapsed().as_micros() as u64;
        let entry = &mut self.pack.files[file];
        match result {
            Ok(image) => {
                let slot = self.next_slot;
                self.next_slot += 1;
                entry.load = Load::Slot(slot);
                self.stats.loaded += 1;
                self.stats.loaded_texels += image.texels.len() as u64;
                self.images.push(Image {
                    slot,
                    key: entry.key.to_string(),
                    original: [entry.key.width as u32, entry.key.height as u32],
                    image,
                });
                Some(slot)
            }
            Err(error) => {
                eprintln!(
                    "ac3: texture pack: {}: {error}; the original is drawn",
                    entry.path.display()
                );
                entry.load = Load::Failed;
                self.stats.failed += 1;
                None
            }
        }
    }
}

/// The replacement texel a sample point reads (the shader's mapping, for
/// tests): `coords` the texel's 8-bit texture coordinates, `fraction` the
/// position inside it, `origin` the upload's first texel in the page's
/// coordinates; the replacement is `size` pixels for `original` texels.
pub fn replacement_pixel(
    coords: [i32; 2],
    fraction: [f32; 2],
    origin: [i32; 2],
    size: [u32; 2],
    original: [u32; 2],
) -> [u32; 2] {
    std::array::from_fn(|axis| {
        let local = ((coords[axis] & 255) - origin[axis]) as f32 + fraction[axis];
        let scaled = (local * size[axis] as f32 / original[axis] as f32).floor();
        scaled.clamp(0.0, (size[axis] - 1) as f32) as u32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_dump::png_io::{self, Pixels};

    /// A folder of its own under the temp folder.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ac3-replace-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn rgba_png(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&pixels.iter().flatten().copied().collect::<Vec<_>>())
            .unwrap();
        writer.finish().unwrap();
        out
    }

    /// A 4-bit 2x4-word texture (8x4 texels) at (640, 0) and its CLUT at
    /// (0, 480), in a VRAM and as the recorder's uploads.
    struct Scene {
        vram: Vram,
        pixels: Vec<u16>,
        clut: Vec<u16>,
    }

    impl Scene {
        fn new() -> Self {
            let pixels: Vec<u16> = (0..8u16).map(|i| 0x3210 + (i * 0x1111) % 0x4444).collect();
            let clut: Vec<u16> = (0..16u16).map(|i| (i << 10) | 1).collect();
            let mut scene = Self {
                vram: Vram::new(),
                pixels,
                clut,
            };
            scene.load([640, 0, 2, 4], &scene.pixels.clone());
            scene.load([0, 480, 16, 1], &scene.clut.clone());
            scene
        }
        fn load(&mut self, [x, y, width, height]: [u16; 4], data: &[u16]) {
            let bytes: Vec<u8> = data.iter().flat_map(|w| w.to_le_bytes()).collect();
            self.vram
                .load_block(&ac3_graphics::tim::Block {
                    x,
                    y,
                    width,
                    height,
                    data: &bytes,
                })
                .unwrap();
        }
        fn texture(&self) -> String {
            key::texture_hash_words(2, 4, &self.pixels)
        }
        fn palette(&self) -> String {
            let (lo, hi) = key::index_range_words(&self.pixels, Mode::P4).unwrap();
            key::palette_hash(lo, hi, &self.clut)
        }
        /// A sprite of page (640, 0) 4-bit with the CLUT at (0, 480).
        fn sample(&self, u: (i32, i32), v: (i32, i32)) -> Sample {
            Sample {
                depth: 0,
                base: [640, 0],
                clut: [0, 480],
                u,
                v,
            }
        }
        fn replacer(&self, dir: &Path) -> Replacer {
            let mut replacer = Replacer::new(TexturePack::open(dir).unwrap());
            replacer.upload(Rect::new(640, 0, 2, 4), &self.pixels);
            replacer.upload(Rect::new(0, 480, 16, 1), &self.clut);
            replacer
        }
    }

    fn write(dir: &Path, name: &str, png: &[u8]) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, png).unwrap();
    }

    #[test]
    fn transparency_follows_the_alpha_rules() {
        assert_eq!(texel_of([255, 255, 255, 0]), 0);
        assert_eq!(texel_of([255, 255, 255, 63]), 0);
        assert_eq!(texel_of([255, 0, 0, 64]), 0x801f, "STP set");
        assert_eq!(texel_of([255, 0, 0, 191]), 0x801f);
        assert_eq!(texel_of([255, 0, 0, 192]), 0x001f, "opaque");
        assert_eq!(
            texel_of([0, 0, 0, 255]),
            OPAQUE_BLACK,
            "opaque black stays visible"
        );
        assert_eq!(texel_of([3, 3, 3, 255]), OPAQUE_BLACK);
        assert_eq!(texel_of([0, 0, 0, 128]), 0x8000, "STP black, as the game's");
        assert_eq!(texel_of([0, 255, 0, 255]), 0x03e0);
        assert_eq!(texel_of([132, 132, 132, 255]), 16 | (16 << 5) | (16 << 10));
        // Every colour of the dump's convention comes back exactly.
        for word in 0..=u16::MAX {
            assert_eq!(texel_of(png_io::rgba(word)), word, "{word:#06x}");
        }
    }

    #[test]
    fn pngs_decode_as_colours_or_live_indices() {
        let key = |palette: PaletteKey| Key {
            mode: Mode::P4,
            texture: "0123456789abcdef".into(),
            palette,
            width: 4,
            height: 2,
        };
        let palette: Vec<u16> = (0..16u16).map(|i| i | ((i & 1) << 15)).collect();
        let indices: Vec<u8> = vec![0, 1, 2, 3, 15, 14, 13, 12];
        let indexed = png_io::encode(
            4,
            2,
            Pixels::Indexed {
                bits: 4,
                indices: &indices,
                palette: &palette,
            },
        )
        .unwrap();
        // `any`: the indices, for the live palette.
        let live = decode(&indexed, &key(PaletteKey::Any)).unwrap();
        assert!(live.indexed);
        assert_eq!(
            live.texels,
            indices.iter().map(|&i| u16::from(i)).collect::<Vec<_>>()
        );
        assert!(decode(&indexed, &key(PaletteKey::Missing)).unwrap().indexed);
        // An exact palette: the PNG's own colours (its PLTE and tRNS).
        let own = decode(&indexed, &key(PaletteKey::Hash("fedcba9876543210".into()))).unwrap();
        assert!(!own.indexed);
        let expected: Vec<u16> = indices.iter().map(|&i| palette[usize::from(i)]).collect();
        assert_eq!(own.texels, expected);
        // 8-bit and 2-bit indices too, and true colour.
        let wide: Vec<u8> = (0..16).map(|i| i * 16).collect();
        let png = png_io::encode(
            4,
            4,
            Pixels::Indexed {
                bits: 8,
                indices: &wide,
                palette: &[0x1234; 256],
            },
        )
        .unwrap();
        let eight = Key {
            mode: Mode::P8,
            ..key(PaletteKey::Any)
        };
        assert_eq!(
            decode(&png, &eight).unwrap().texels,
            wide.iter().map(|&i| u16::from(i)).collect::<Vec<_>>()
        );
        let mut two = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut two, 5, 1);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Two);
            encoder.set_palette(vec![0u8; 12]);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[0b0001_1011, 0b1100_0000])
                .unwrap();
            writer.finish().unwrap();
        }
        assert_eq!(
            decode(&two, &key(PaletteKey::Any)).unwrap().texels,
            [0, 1, 2, 3, 3]
        );
        let rgba = rgba_png(2, 1, &[[255, 0, 0, 255], [0, 0, 255, 100]]);
        let colour = decode(&rgba, &key(PaletteKey::Any)).unwrap();
        assert!(!colour.indexed, "true colour stays colour with any");
        assert_eq!(colour.texels, [0x001f, 0xfc00]);
        assert!(decode(b"not a png", &key(PaletteKey::Any)).is_err());
    }

    #[test]
    fn files_are_found_by_the_key_at_the_end_of_their_names() {
        let dir = folder("names");
        let scene = Scene::new();
        let (t, p) = (scene.texture(), scene.palette());
        let png = rgba_png(1, 1, &[[255; 4]]);
        write(&dir, &format!("hud/edited-ac3tex-p4-{t}-{p}-8x4.png"), &png);
        write(&dir, &format!("deep/a/b/ac3tex-p4-{t}-any-8x4.PNG"), &png);
        write(
            &dir,
            &format!("x-ac3tex-p4-{}-any-8x4.png", t.to_uppercase()),
            &png,
        );
        write(&dir, "readme.png", &png);
        write(&dir, &format!("ac3tex-p4-{t}-{p}-8x4.txt"), &png);
        // (After the others in path order: the first size of a texture wins.)
        write(&dir, &format!("z/ac3tex-p4-{t}-{p}-16x4.png"), &png);
        write(&dir, &format!("ac3tex-p4-{t}-{p}-9x4.png"), &png);
        let pack = TexturePack::open(&dir).unwrap();
        assert_eq!(pack.len(), 2, "{:?}", pack.skipped);
        assert_eq!(pack.skipped.len(), 4, "{:?}", pack.skipped);
        let reasons: Vec<&str> = pack.skipped.iter().map(|(_, reason)| *reason).collect();
        assert!(reasons.contains(&"an earlier file has the same key"));
        assert!(reasons.contains(&"no replacement key (ac3tex-...) in the name"));
        assert!(reasons.contains(&"another file names this texture with another size"));
        assert!(reasons.contains(&"the size in the name is not an upload's"));
        assert!(pack.summary().contains("2 replacements for 1 texture"));
        assert_eq!(pack.sizes, HashSet::from([(2, 4)]));
        assert!(TexturePack::open(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lookup_takes_the_exact_palette_then_any_then_nopal() {
        let dir = folder("order");
        let mut scene = Scene::new();
        let (t, p) = (scene.texture(), scene.palette());
        let png = rgba_png(16, 8, &[[255; 4]; 128]);
        write(&dir, &format!("exact-ac3tex-p4-{t}-{p}-8x4.png"), &png);
        write(&dir, &format!("any-ac3tex-p4-{t}-any-8x4.png"), &png);
        write(&dir, &format!("nopal-ac3tex-p4-{t}-nopal-8x4.png"), &png);
        let file = |replacer: &Replacer, slot: u32| {
            let path = &replacer
                .pack
                .files
                .iter()
                .find(|f| f.load == Load::Slot(slot))
                .unwrap()
                .path;
            path.file_name().unwrap().to_string_lossy()[..3].to_owned()
        };
        let mut replacer = scene.replacer(&dir);
        let sprite = scene.sample((0, 7), (0, 3));
        let (slot, origin) = replacer.lookup(&sprite, &scene.vram).unwrap();
        assert_eq!(file(&replacer, slot), "exa");
        assert_eq!(origin, [0, 0]);
        // Decoded once, handed to the card once.
        let (reset, images) = replacer.take();
        assert!(!reset);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].original, [8, 4]);
        assert_eq!((images[0].image.width, images[0].image.height), (16, 8));
        assert_eq!(replacer.lookup(&sprite, &scene.vram).unwrap().0, slot);
        assert!(replacer.take().1.is_empty());
        // Another palette in VRAM (a used entry changes): `any`.
        let mut clut = scene.clut.clone();
        clut[1] ^= 0x7fff;
        scene.load([0, 480, 16, 1], &clut);
        let (slot, _) = replacer.lookup(&sprite, &scene.vram).unwrap();
        assert_eq!(file(&replacer, slot), "any");
        // An entry outside the texture's index range does not count.
        let (lo, hi) = key::index_range_words(&scene.pixels, Mode::P4).unwrap();
        assert!(lo > 0 || hi < 15, "the test texture leaves an entry unused");
        let mut clut = scene.clut.clone();
        let unused = if lo > 0 { 0 } else { 15 };
        clut[unused] ^= 0x7fff;
        scene.load([0, 480, 16, 1], &clut);
        let (slot, _) = replacer.lookup(&sprite, &scene.vram).unwrap();
        assert_eq!(file(&replacer, slot), "exa");
        // Without `any`: `nopal`; without either: the original.
        std::fs::remove_file(dir.join(format!("any-ac3tex-p4-{t}-any-8x4.png"))).unwrap();
        scene.load([0, 480, 16, 1], &[0x7fff; 16]);
        replacer.reload(&scene.vram).unwrap();
        let (slot, _) = replacer.lookup(&sprite, &scene.vram).unwrap();
        assert_eq!(file(&replacer, slot), "nop");
        std::fs::remove_file(dir.join(format!("nopal-ac3tex-p4-{t}-nopal-8x4.png"))).unwrap();
        replacer.reload(&scene.vram).unwrap();
        assert_eq!(replacer.lookup(&sprite, &scene.vram), None);
        // The 8-bit reading of the same upload is another texture.
        let wide = Sample {
            depth: 1,
            ..scene.sample((0, 3), (0, 3))
        };
        assert_eq!(replacer.lookup(&wide, &scene.vram), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn primitives_fall_back_to_the_original() {
        let dir = folder("fallback");
        let scene = Scene::new();
        let t = scene.texture();
        write(
            &dir,
            &format!("ac3tex-p4-{t}-any-8x4.png"),
            &rgba_png(8, 4, &[[255; 4]; 32]),
        );
        let mut replacer = scene.replacer(&dir);
        let vram = &scene.vram;
        assert!(
            replacer
                .lookup(&scene.sample((0, 7), (0, 3)), vram)
                .is_some()
        );
        // Past the upload: spans it and other words.
        assert_eq!(replacer.lookup(&scene.sample((0, 8), (0, 3)), vram), None);
        assert_eq!(replacer.lookup(&scene.sample((0, 7), (0, 4)), vram), None);
        // Another page base: words never uploaded.
        let elsewhere = Sample {
            base: [704, 0],
            ..scene.sample((0, 7), (0, 3))
        };
        assert_eq!(replacer.lookup(&elsewhere, vram), None);
        // A pixel drawn over one texel word: sprites over it fall back,
        // the rest of the texture is still replaced.
        replacer.written(&Rect::new(641, 2, 1, 1));
        assert_eq!(replacer.lookup(&scene.sample((4, 7), (0, 3)), vram), None);
        assert!(
            replacer
                .lookup(&scene.sample((0, 3), (0, 3)), vram)
                .is_some()
        );
        assert!(
            replacer
                .lookup(&scene.sample((4, 7), (0, 1)), vram)
                .is_some()
        );
        // Uploading it again (here a re-seed from VRAM) restores it.
        replacer.reseed(vram);
        assert!(
            replacer
                .lookup(&scene.sample((4, 7), (0, 3)), vram)
                .is_some()
        );
        // An upload over all of it: gone.
        replacer.upload(Rect::new(640, 0, 2, 4), &[0; 8]);
        assert_eq!(replacer.lookup(&scene.sample((0, 7), (0, 3)), vram), None);
        assert_eq!(replacer.stats.tracked_uploads, 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn uploads_spanning_pages_keep_their_texel_origin() {
        let dir = folder("origin");
        // A 15-bit 96x2 upload at (40, 300): page bases 0 and 64 (y 256).
        let pixels: Vec<u16> = (0..192u16).map(|i| i | 0x0400).collect();
        let t = key::texture_hash_words(96, 2, &pixels);
        write(
            &dir,
            &format!("ac3tex-c16-{t}-96x2.png"),
            &rgba_png(1, 1, &[[255; 4]]),
        );
        let mut replacer = Replacer::new(TexturePack::open(&dir).unwrap());
        replacer.upload(Rect::new(40, 300, 96, 2), &pixels);
        let vram = Vram::new();
        let sample = |base_x: i32, u: (i32, i32)| Sample {
            depth: 2,
            base: [base_x, 256],
            clut: [0, 0],
            u,
            v: (44, 45),
        };
        assert_eq!(
            replacer.lookup(&sample(0, (40, 135)), &vram).map(|r| r.1),
            Some([40, 44])
        );
        assert_eq!(
            replacer.lookup(&sample(64, (0, 71)), &vram).map(|r| r.1),
            Some([-24, 44])
        );
        assert_eq!(replacer.lookup(&sample(64, (0, 72)), &vram), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reseeds_find_textures_still_in_vram() {
        let dir = folder("reseed");
        let mut scene = Scene::new();
        let t = scene.texture();
        write(
            &dir,
            &format!("ac3tex-p4-{t}-any-8x4.png"),
            &rgba_png(8, 4, &[[255; 4]; 32]),
        );
        let mut replacer = scene.replacer(&dir);
        let sprite = scene.sample((0, 7), (0, 3));
        assert!(replacer.lookup(&sprite, &scene.vram).is_some());
        // A quick load whose VRAM holds the texture: found again.
        replacer.reseed(&scene.vram);
        assert!(replacer.lookup(&sprite, &scene.vram).is_some());
        // One whose VRAM holds something else there: not.
        scene.load([640, 0, 2, 4], &[7; 8]);
        replacer.reseed(&scene.vram);
        assert_eq!(replacer.lookup(&sprite, &scene.vram), None);
        // A reload starts the slots over and says so once.
        scene.load([640, 0, 2, 4], &scene.pixels.clone());
        let summary = replacer.reload(&scene.vram).unwrap();
        assert!(summary.contains("1 replacement for 1 texture"), "{summary}");
        assert_eq!(replacer.lookup(&sprite, &scene.vram).unwrap().0, 0);
        let (reset, images) = replacer.take();
        assert!(reset);
        assert_eq!(images.len(), 1);
        assert!(!replacer.take().0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_new_session_finds_textures_where_they_were_uploaded_before() {
        let dir = folder("positions");
        let scene = Scene::new();
        let t = scene.texture();
        write(
            &dir,
            &format!("ac3tex-p4-{t}-any-8x4.png"),
            &rgba_png(8, 4, &[[255; 4]; 32]),
        );
        let mut first = scene.replacer(&dir);
        let lines = || std::fs::read_to_string(dir.join(POSITIONS)).unwrap();
        assert_eq!(lines(), format!("{t} 640 0 2 4\n"));
        // Uploaded again: nothing new to note.
        first.upload(Rect::new(640, 0, 2, 4), &scene.pixels);
        assert_eq!(lines().lines().count(), 1);
        // A new session starting from a state that holds the texture
        // (a quick load): no upload seen, but its old place is known.
        let mut second = Replacer::new(TexturePack::open(&dir).unwrap());
        let sprite = scene.sample((0, 7), (0, 3));
        assert_eq!(second.lookup(&sprite, &scene.vram), None);
        second.reseed(&scene.vram);
        assert!(second.lookup(&sprite, &scene.vram).is_some());
        // Lines that do not parse or leave VRAM are ignored.
        let parsed =
            parse_positions("zz 1 2 3 4\n0123456789abcdef 1000 0 64 1\n00ff 1 2 3\n1 2 3 4 5\n");
        assert_eq!(parsed, [(1, [2, 3, 4, 5])]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_pngs_are_reported_once_and_draw_the_original() {
        let dir = folder("broken");
        let scene = Scene::new();
        let t = scene.texture();
        write(&dir, &format!("ac3tex-p4-{t}-any-8x4.png"), b"not a png");
        let mut replacer = scene.replacer(&dir);
        let sprite = scene.sample((0, 7), (0, 3));
        assert_eq!(replacer.lookup(&sprite, &scene.vram), None);
        assert_eq!(replacer.lookup(&sprite, &scene.vram), None);
        assert_eq!(replacer.stats.failed, 1);
        assert!(replacer.take().1.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ---- movie frames ----

    /// Movie images of `movie_0810` that are decoded (by number): 8x4
    /// pixels for a 4x2-texel frame, pixel (0, 0) black.
    struct Images(std::rc::Rc<std::cell::RefCell<HashSet<u32>>>);

    fn movie_pixel(x: u32, y: u32, file: u32) -> [u8; 4] {
        if (x, y) == (0, 0) {
            return [0, 0, 0, 255];
        }
        [(x * 30) as u8, (y * 60) as u8, (file * 10) as u8, 200]
    }

    impl MovieImages for Images {
        fn image(&self, key: &Arc<str>, file: u32) -> Option<Arc<RgbaImage>> {
            assert_eq!(&**key, "movie_0810");
            self.0.borrow().contains(&file).then(|| {
                let pixels = (0..4)
                    .flat_map(|y| (0..8).flat_map(move |x| movie_pixel(x, y, file)))
                    .collect();
                Arc::new(RgbaImage {
                    width: 8,
                    height: 4,
                    pixels,
                })
            })
        }
    }

    /// A movie-only replacer and the set of decoded images.
    fn movie_replacer() -> (Replacer, std::rc::Rc<std::cell::RefCell<HashSet<u32>>>) {
        let ready = std::rc::Rc::default();
        let mut replacer = Replacer::new(TexturePack::none());
        replacer.set_movie_images(Some(Box::new(Images(std::rc::Rc::clone(&ready)))));
        (replacer, ready)
    }

    /// Frame `file` of `movie_0810`, 4x2 texels at (`x`, 300).
    fn movie(x: i32, file: u32) -> MovieFrame {
        MovieFrame {
            rect: Rect::new(x, 300, 4, 2),
            key: "movie_0810".into(),
            file,
            stp: true,
        }
    }

    /// A 15-bit sprite of the 4x2 texels at (`x`, 300).
    fn movie_sample(x: i32) -> Sample {
        Sample {
            depth: 2,
            base: [x - x % 64, 256],
            clut: [0, 0],
            u: (x % 64, x % 64 + 3),
            v: (44, 45),
        }
    }

    #[test]
    fn movie_texels_keep_the_frames_stp_bit() {
        assert_eq!(movie_texel([0, 0, 0, 255], true), 0x8000);
        assert_eq!(movie_texel([0, 0, 0, 255], false), OPAQUE_BLACK);
        assert_eq!(movie_texel([255, 0, 0, 0], true), 0x801f, "alpha ignored");
        assert_eq!(movie_texel([0, 255, 0, 10], false), 0x03e0);
        // Every colour of the screen's expansion comes back exactly.
        for word in 0..0x8000u16 {
            let [r, g, b, _] = png_io::rgba(word | 0x8000);
            assert_eq!(movie_texel([r, g, b, 255], true), word | 0x8000);
        }
    }

    #[test]
    fn movie_frames_draw_their_image_while_they_own_their_texels() {
        let vram = Vram::new();
        // Without images, frames are not taken.
        let mut replacer = Replacer::new(TexturePack::none());
        assert!(replacer.pack().is_none() && !replacer.has_movie_images());
        replacer.movie_frame(movie(64, 0));
        assert_eq!(replacer.lookup(&movie_sample(64), &vram), None);
        assert_eq!(replacer.movie_stats.frames, 0);
        let (mut replacer, ready) = movie_replacer();
        replacer.movie_frame(movie(64, 3));
        // Not decoded yet: the game's texels, until it is.
        assert_eq!(replacer.lookup(&movie_sample(64), &vram), None);
        assert_eq!(replacer.movie_stats.late, 1);
        ready.borrow_mut().insert(3);
        let (slot, origin) = replacer.lookup(&movie_sample(64), &vram).unwrap();
        assert!(is_movie_slot(slot));
        assert_eq!(origin, [0, 44]);
        let (reset, images) = replacer.take();
        assert!(!reset);
        assert_eq!(images.len(), 1);
        let image = &images[0];
        assert_eq!((image.slot, image.original), (slot, [4, 2]));
        assert_eq!((image.image.width, image.image.height), (8, 4));
        assert!(!image.image.indexed);
        assert_eq!(
            image.image.texels[0], 0x8000,
            "black keeps the frame's STP bit"
        );
        assert_eq!(
            image.image.texels[8 * 3 + 5],
            movie_texel(movie_pixel(5, 3, 3), true)
        );
        // Drawn again (another pass): the same slot, nothing sent again.
        assert_eq!(replacer.lookup(&movie_sample(64), &vram).unwrap().0, slot);
        assert!(replacer.take().1.is_empty());
        // Read as 4-bit texels, or past it: the original.
        let indexed = Sample {
            depth: 0,
            u: (0, 15),
            ..movie_sample(64)
        };
        assert_eq!(replacer.lookup(&indexed, &vram), None);
        let wider = Sample {
            u: (0, 4),
            ..movie_sample(64)
        };
        assert_eq!(replacer.lookup(&wider, &vram), None);
        // Drawn over at one texel: the sprites over it fall back.
        replacer.written(&Rect::new(66, 301, 1, 1));
        assert_eq!(replacer.lookup(&movie_sample(64), &vram), None);
        let left = Sample {
            u: (0, 1),
            ..movie_sample(64)
        };
        assert_eq!(replacer.lookup(&left, &vram).unwrap().0, slot);
        // The next frame's slices over it: the old frame is gone.
        replacer.upload(Rect::new(64, 300, 2, 2), &[0; 4]);
        replacer.upload(Rect::new(66, 300, 2, 2), &[0; 4]);
        assert_eq!(replacer.lookup(&left, &vram), None);
        assert!(replacer.movies.is_empty());
        assert!(replacer.movie_slots.iter().all(|s| s.frame.is_none()));
        // A re-seed (a quick load) forgets frames.
        replacer.movie_frame(movie(64, 3));
        replacer.reseed(&vram);
        assert_eq!(replacer.lookup(&movie_sample(64), &vram), None);
        assert_eq!(replacer.movie_stats.frames, 2);
    }

    #[test]
    fn movie_slots_are_reused_but_never_within_a_pass() {
        let vram = Vram::new();
        let (mut replacer, ready) = movie_replacer();
        ready.borrow_mut().extend(0..16);
        let frames: Vec<i32> = (0..=MOVIE_SLOTS as i32).map(|i| 64 + 8 * i).collect();
        for (file, &x) in frames.iter().enumerate() {
            replacer.movie_frame(movie(x, file as u32));
        }
        // One pass draws them all: one slot each, the ninth draws the
        // game's texels.
        let slots: Vec<Option<u32>> = frames
            .iter()
            .map(|&x| replacer.lookup(&movie_sample(x), &vram).map(|r| r.0))
            .collect();
        let taken: HashSet<u32> = slots.iter().flatten().copied().collect();
        assert_eq!(taken.len(), MOVIE_SLOTS as usize);
        assert!(taken.iter().all(|&slot| is_movie_slot(slot)));
        assert_eq!(slots[MOVIE_SLOTS as usize], None);
        assert_eq!(replacer.movie_stats.no_slot, 1);
        assert_eq!(replacer.take().1.len(), MOVIE_SLOTS as usize);
        // The next pass: it takes the slot drawn longest ago (the first
        // frame's), which gets a slot again when drawn after it.
        let last = frames[MOVIE_SLOTS as usize];
        assert_eq!(
            replacer.lookup(&movie_sample(last), &vram).unwrap().0,
            slots[0].unwrap()
        );
        let again = replacer.lookup(&movie_sample(frames[0]), &vram).unwrap().0;
        assert_eq!(again, slots[1].unwrap());
        let sent = replacer.take().1;
        assert_eq!(sent.len(), 2);
        assert!(sent[0].key.ends_with("image 8") && sent[1].key.ends_with("image 0"));
        // A frame overwritten frees its slot: a new frame takes a free one
        // before any other.
        replacer.upload(Rect::new(frames[2], 300, 4, 2), &[0; 8]);
        replacer.movie_frame(movie(frames[2], 15));
        assert_eq!(
            replacer.lookup(&movie_sample(frames[2]), &vram).unwrap().0,
            slots[2].unwrap()
        );
        assert_eq!(replacer.movie_stats.images, MOVIE_SLOTS as u64 + 3);
    }

    #[test]
    fn uv_mapping_scales_into_the_replacement() {
        // 4x: texel 3, sub-sample 2 of 4 (fraction 0.625) of an upload at
        // the page origin reads replacement pixel 14.
        assert_eq!(
            replacement_pixel([3, 0], [0.625, 0.125], [0, 0], [32, 16], [8, 4]),
            [14, 0]
        );
        // The same texture with a nearest-neighbour 4x replacement reads a
        // pixel of the same texel at every sample point.
        for texel in 0..8 {
            for sub in 0..4 {
                let fraction = (sub as f32 + 0.5) / 4.0;
                let [x, _] = replacement_pixel([texel, 0], [fraction; 2], [0, 0], [32, 16], [8, 4]);
                assert_eq!(x / 4, texel as u32);
            }
        }
        // Non-integer scales (1.5x), origins inside the page, clamping at
        // the edges and wrapping 8-bit coordinates.
        assert_eq!(
            replacement_pixel([5, 3], [0.5, 0.5], [0, 0], [12, 6], [8, 4]),
            [8, 5]
        );
        assert_eq!(
            replacement_pixel([130, 70], [0.0, 0.99], [128, 64], [16, 8], [8, 4]),
            [4, 7]
        );
        assert_eq!(
            replacement_pixel([127, 63], [0.5, 0.5], [128, 64], [16, 8], [8, 4]),
            [0, 0]
        );
        assert_eq!(
            replacement_pixel([256 + 2, 1], [0.0, 0.0], [0, 0], [8, 4], [8, 4]),
            [2, 1]
        );
        // A texel origin left of the page (an upload starting in the page
        // before).
        assert_eq!(
            replacement_pixel([0, 0], [0.0, 0.0], [-24, 0], [192, 4], [96, 2]),
            [48, 0]
        );
    }
}
