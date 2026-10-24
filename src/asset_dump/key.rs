//! The texture replacement key (`docs/ASSET_DUMP.md`, "Replacement key").
//!
//! A texture is one CPU→VRAM upload (one LoadImage rectangle, e.g. a TIM's
//! pixel block), drawn in one texel mode, with one palette:
//!
//! - `T`, the texture hash: SHA-256 of the upload's width and height in
//!   VRAM words (u16 LE each) followed by its halfwords (LE, row-major);
//!   the first 8 digest bytes as 16 lowercase hex digits. The VRAM position
//!   is not hashed, so a texture keeps its key wherever it is loaded.
//! - the mode: `p4`, `p8` (4/8-bit indices) or `c16` (15-bit colour);
//!   `c24` only names 24-bit images of the static dump, which the GPU
//!   cannot draw as textures.
//! - `P`, the palette hash (`p4`/`p8` only): `lo` and `hi`, the smallest and
//!   largest index that occurs anywhere in the upload under that mode, then
//!   the palette entries `lo..=hi` (u16 LE) as read from VRAM, SHA-256, the
//!   first 8 bytes as 16 hex digits. Entries the texture never indexes do
//!   not change the key.
//!
//! The file stem is `ac3tex-<mode>-<T>-<P>-<W>x<H>` (`ac3tex-c16-<T>-<W>x<H>`
//! for direct colour), `W` x `H` being the original size in texels.
use sha2::{Digest, Sha256};
use std::fmt;

/// Texel mode of a texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    P4,
    P8,
    C16,
    C24,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::P4 => "p4",
            Self::P8 => "p8",
            Self::C16 => "c16",
            Self::C24 => "c24",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "p4" => Self::P4,
            "p8" => Self::P8,
            "c16" => Self::C16,
            "c24" => Self::C24,
            _ => return None,
        })
    }
    /// The GPU texture-page depth field (GP0 E1 / polygon page bits 7..8).
    pub fn from_page(page: u32) -> Self {
        match (page >> 7) & 3 {
            0 => Self::P4,
            1 => Self::P8,
            _ => Self::C16,
        }
    }
    pub fn from_depth(depth: ac3_graphics::tim::Depth) -> Self {
        use ac3_graphics::tim::Depth;
        match depth {
            Depth::Indexed4 => Self::P4,
            Depth::Indexed8 => Self::P8,
            Depth::Direct15 => Self::C16,
            Depth::Direct24 => Self::C24,
        }
    }
    /// Texels per VRAM word for drawable modes (`c24`: none).
    pub fn texels_per_word(self) -> Option<usize> {
        match self {
            Self::P4 => Some(4),
            Self::P8 => Some(2),
            Self::C16 => Some(1),
            Self::C24 => None,
        }
    }
    /// Palette entries an index addresses (0 for direct colour).
    pub fn palette_entries(self) -> usize {
        match self {
            Self::P4 => 16,
            Self::P8 => 256,
            Self::C16 | Self::C24 => 0,
        }
    }
    pub fn indexed(self) -> bool {
        matches!(self, Self::P4 | Self::P8)
    }
}

fn hex8(digest: &[u8]) -> String {
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// `T` of an upload given as little-endian bytes (a TIM block's data).
pub fn texture_hash_bytes(width: u16, height: u16, data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(width.to_le_bytes());
    hasher.update(height.to_le_bytes());
    hasher.update(data);
    hex8(&hasher.finalize())
}

/// `T` of an upload given as halfwords.
pub fn texture_hash_words(width: u16, height: u16, words: &[u16]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(width.to_le_bytes());
    hasher.update(height.to_le_bytes());
    for word in words {
        hasher.update(word.to_le_bytes());
    }
    hex8(&hasher.finalize())
}

/// Smallest and largest index in an upload's little-endian bytes under
/// `mode` (`p4`: every nibble, `p8`: every byte). `None` for direct colour
/// or no data.
pub fn index_range(data: &[u8], mode: Mode) -> Option<(u8, u8)> {
    let mut range: Option<(u8, u8)> = None;
    let mut add = |index: u8| {
        range = Some(match range {
            None => (index, index),
            Some((lo, hi)) => (lo.min(index), hi.max(index)),
        });
    };
    match mode {
        Mode::P4 => data.iter().for_each(|&b| {
            add(b & 15);
            add(b >> 4);
        }),
        Mode::P8 => data.iter().for_each(|&b| add(b)),
        Mode::C16 | Mode::C24 => {}
    }
    range
}

/// [`index_range`] over halfwords.
pub fn index_range_words(words: &[u16], mode: Mode) -> Option<(u8, u8)> {
    let mut lo = u8::MAX;
    let mut hi = 0u8;
    let mut any = false;
    for &word in words {
        match mode {
            Mode::P4 => {
                for shift in [0, 4, 8, 12] {
                    let index = ((word >> shift) & 15) as u8;
                    lo = lo.min(index);
                    hi = hi.max(index);
                }
            }
            Mode::P8 => {
                for index in word.to_le_bytes() {
                    lo = lo.min(index);
                    hi = hi.max(index);
                }
            }
            Mode::C16 | Mode::C24 => return None,
        }
        any = true;
    }
    any.then_some((lo, hi))
}

/// The last texel a polygon reads on one axis, from its vertices
/// `(x, y, coordinate)` (screen position, texture coordinate): the largest
/// coordinate, or one less when every vertex holding it is on the polygon's
/// right-most column (largest x) or bottom row (largest y). The GPU draws
/// neither (spans stop before their right end, a polygon before its bottom
/// row), and coordinates are interpolated, so every pixel drawn has a
/// smaller one: a quad mapping texels `u0..u0+w-1` names `u0+w` at its
/// right corners without reading it. Mirrored quads, with the largest
/// coordinate on the left or top, keep it. The asset dump and texture
/// replacement both attribute polygons by this range.
pub fn sampled_hi(vertices: &[(f32, f32, i32)]) -> i32 {
    let hi = vertices.iter().map(|v| v.2).max().unwrap_or(0);
    let lo = vertices.iter().map(|v| v.2).min().unwrap_or(0);
    let x_max = vertices.iter().map(|v| v.0).fold(f32::MIN, f32::max);
    let y_max = vertices.iter().map(|v| v.1).fold(f32::MIN, f32::max);
    let unread = vertices
        .iter()
        .filter(|v| v.2 == hi)
        .all(|v| v.0 == x_max || v.1 == y_max);
    if unread && hi > lo { hi - 1 } else { hi }
}

/// `P` of palette `entries` (indexed from 0) over `lo..=hi`. Entries past
/// the end of `entries` count as 0x0000.
pub fn palette_hash(lo: u8, hi: u8, entries: &[u16]) -> String {
    let mut hasher = Sha256::new();
    hasher.update([lo, hi]);
    for index in usize::from(lo)..=usize::from(hi) {
        hasher.update(entries.get(index).copied().unwrap_or(0).to_le_bytes());
    }
    hex8(&hasher.finalize())
}

/// Which palettes a replacement applies to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PaletteKey {
    /// Direct colour: no palette.
    None,
    /// Exactly this `P`.
    Hash(String),
    /// Every palette (`any` in a replacement file name; never dumped).
    Any,
    /// A static 4/8-bit image without its own CLUT (`nopal`, grey ramp).
    Missing,
}

/// A parsed or generated replacement key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub mode: Mode,
    pub texture: String,
    pub palette: PaletteKey,
    /// Original size in texels.
    pub width: usize,
    pub height: usize,
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ac3tex-{}-{}", self.mode.name(), self.texture)?;
        match &self.palette {
            PaletteKey::None => {}
            PaletteKey::Hash(hash) => write!(f, "-{hash}")?,
            PaletteKey::Any => write!(f, "-any")?,
            PaletteKey::Missing => write!(f, "-nopal")?,
        }
        write!(f, "-{}x{}", self.width, self.height)
    }
}

impl Key {
    /// Find the key in a file stem: the last `ac3tex-` through the end of
    /// the stem (any prefix, such as a source name, is allowed). Hex digits
    /// may be in either case.
    pub fn from_stem(stem: &str) -> Option<Self> {
        let at = stem.rfind("ac3tex-")?;
        let fields: Vec<&str> = stem[at + 7..].split('-').collect();
        let hex16 = |s: &str| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
        let (mode, texture, palette, size) = match fields.as_slice() {
            [mode, texture, size] => (*mode, *texture, PaletteKey::None, *size),
            [mode, texture, palette, size] => {
                let palette = match *palette {
                    "any" => PaletteKey::Any,
                    "nopal" => PaletteKey::Missing,
                    hash if hex16(hash) => PaletteKey::Hash(hash.to_ascii_lowercase()),
                    _ => return None,
                };
                (*mode, *texture, palette, *size)
            }
            _ => return None,
        };
        let mode = Mode::parse(mode)?;
        if !hex16(texture) || mode.indexed() == (palette == PaletteKey::None) {
            return None;
        }
        let (width, height) = size.split_once('x')?;
        Some(Self {
            mode,
            texture: texture.to_ascii_lowercase(),
            palette,
            width: width.parse().ok()?,
            height: height.parse().ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_cover_size_content_and_used_palette_range() {
        let words = [0x1234u16, 0xabcd];
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(
            texture_hash_words(2, 1, &words),
            texture_hash_bytes(2, 1, &bytes)
        );
        assert_ne!(
            texture_hash_words(2, 1, &words),
            texture_hash_words(1, 2, &words)
        );
        assert_eq!(texture_hash_words(2, 1, &words).len(), 16);
        assert_eq!(index_range(&bytes, Mode::P4), Some((1, 13)));
        assert_eq!(index_range_words(&words, Mode::P4), Some((1, 13)));
        assert_eq!(index_range(&bytes, Mode::P8), Some((0x12, 0xcd)));
        assert_eq!(index_range_words(&words, Mode::P8), Some((0x12, 0xcd)));
        assert_eq!(index_range(&bytes, Mode::C16), None);
        // Entries outside lo..=hi do not change P.
        let mut palette = [7u16; 16];
        let p = palette_hash(1, 13, &palette);
        palette[0] = 9;
        palette[14] = 9;
        assert_eq!(palette_hash(1, 13, &palette), p);
        palette[5] = 9;
        assert_ne!(palette_hash(1, 13, &palette), p);
    }

    #[test]
    fn polygons_do_not_read_the_texel_past_their_right_and_bottom_ends() {
        let corners = [(10.0, 20.0), (18.0, 20.0), (10.0, 32.0), (18.0, 32.0)];
        let with = |coordinates: [i32; 4]| -> Vec<(f32, f32, i32)> {
            corners
                .iter()
                .zip(coordinates)
                .map(|(&(x, y), c)| (x, y, c))
                .collect()
        };
        // An 8 x 12 quad mapping u 240..=247, v 126..=137 names 248 and 138.
        assert_eq!(sampled_hi(&with([240, 248, 240, 248])), 247);
        assert_eq!(sampled_hi(&with([126, 126, 138, 138])), 137);
        // Mirrored, the largest coordinate is on the left (drawn): kept.
        assert_eq!(sampled_hi(&with([248, 240, 248, 240])), 248);
        // One coordinate everywhere: that texel is read.
        assert_eq!(sampled_hi(&with([5, 5, 5, 5])), 5);
        // A triangle whose largest u is at its top-left corner keeps it.
        assert_eq!(
            sampled_hi(&[(0.0, 0.0, 9), (8.0, 4.0, 1), (2.0, 9.0, 3)]),
            9
        );
    }

    #[test]
    fn stems_format_and_parse() {
        let key = Key {
            mode: Mode::P4,
            texture: "0123456789abcdef".into(),
            palette: PaletteKey::Hash("fedcba9876543210".into()),
            width: 64,
            height: 32,
        };
        let stem = key.to_string();
        assert_eq!(stem, "ac3tex-p4-0123456789abcdef-fedcba9876543210-64x32");
        assert_eq!(Key::from_stem(&stem), Some(key.clone()));
        assert_eq!(Key::from_stem(&format!("0003_0007-{stem}")), Some(key));
        let direct = Key::from_stem("ac3tex-c16-0123456789ABCDEF-16x16").unwrap();
        assert_eq!(direct.palette, PaletteKey::None);
        assert_eq!(direct.texture, "0123456789abcdef");
        let any = Key::from_stem("hud-ac3tex-p8-0123456789abcdef-any-32x8").unwrap();
        assert_eq!(any.palette, PaletteKey::Any);
        for bad in [
            "ac3tex-p4-0123456789abcdef-64x32",
            "ac3tex-c16-0123456789abcdef-fedcba9876543210-64x32",
            "ac3tex-p4-0123-fedcba9876543210-64x32",
            "ac3tex-q4-0123456789abcdef-fedcba9876543210-64x32",
            "ac3tex-p4-0123456789abcdef-fedcba9876543210-64",
        ] {
            assert_eq!(Key::from_stem(bad), None, "{bad}");
        }
    }
}
