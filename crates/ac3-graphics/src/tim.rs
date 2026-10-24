//! Standard TIM blocks, retaining the original VRAM coordinates and packed words.

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Indexed4,
    Indexed8,
    Direct15,
    Direct24,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block<'a> {
    pub x: u16,
    pub y: u16,
    /// Width in 16-bit VRAM words, not texels.
    pub width: u16,
    pub height: u16,
    pub data: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimImage<'a> {
    pub depth: Depth,
    pub clut: Option<Block<'a>>,
    pub pixels: Block<'a>,
    pub bytes_consumed: usize,
}

fn word(data: &[u8], offset: usize) -> Result<u32> {
    let bytes = offset
        .checked_add(4)
        .and_then(|end| data.get(offset..end))
        .ok_or(Error::Truncated("TIM word"))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn block<'a>(data: &'a [u8], pos: &mut usize) -> Result<Block<'a>> {
    let size = word(data, *pos)? as usize;
    if size < 12 {
        return Err(Error::Invalid("TIM block is smaller than its header"));
    }
    let end = pos
        .checked_add(size)
        .ok_or(Error::Invalid("TIM extent overflow"))?;
    let bytes = data.get(*pos..end).ok_or(Error::Truncated("TIM block"))?;
    let half = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let (x, y, width, height) = (half(4), half(6), half(8), half(10));
    if usize::from(x) + usize::from(width) > 1024 || usize::from(y) + usize::from(height) > 512 {
        return Err(Error::Invalid("TIM block extends outside VRAM"));
    }
    let count = usize::from(width) * usize::from(height) * 2;
    if size - 12 != count {
        return Err(Error::Invalid(
            "TIM block dimensions differ from stored length",
        ));
    }
    *pos = end;
    Ok(Block {
        x,
        y,
        width,
        height,
        data: &bytes[12..],
    })
}

/// Validate a TIM and borrow its palette/image bytes without allocating.
/// Trailing container padding is allowed and measured by `bytes_consumed`.
pub fn parse(data: &[u8]) -> Result<TimImage<'_>> {
    if word(data, 0)? != 0x10 {
        return Err(Error::Invalid("expected TIM magic 0x10"));
    }
    let flags = word(data, 4)?;
    if flags & !0xb != 0 {
        return Err(Error::Unsupported("TIM flags"));
    }
    let depth = match flags & 3 {
        0 => Depth::Indexed4,
        1 => Depth::Indexed8,
        2 => Depth::Direct15,
        _ => Depth::Direct24,
    };
    let mut pos = 8;
    let clut = if flags & 8 != 0 {
        Some(block(data, &mut pos)?)
    } else {
        None
    };
    let pixels = block(data, &mut pos)?;
    Ok(TimImage {
        depth,
        clut,
        pixels,
        bytes_consumed: pos,
    })
}

impl Depth {
    /// Bits per texel.
    pub fn bits(self) -> u8 {
        match self {
            Self::Indexed4 => 4,
            Self::Indexed8 => 8,
            Self::Direct15 => 16,
            Self::Direct24 => 24,
        }
    }

    /// Texels across `words` 16-bit VRAM words (24-bit: three bytes each,
    /// so two words hold one and a third texel; partial texels are dropped).
    pub fn texel_width(self, words: u16) -> usize {
        let words = usize::from(words);
        match self {
            Self::Indexed4 => words * 4,
            Self::Indexed8 => words * 2,
            Self::Direct15 => words,
            Self::Direct24 => words * 2 / 3,
        }
    }

    /// Palette entries one index addresses: 16, 256, or 0 for direct colour.
    pub fn palette_entries(self) -> usize {
        match self {
            Self::Indexed4 => 16,
            Self::Indexed8 => 256,
            Self::Direct15 | Self::Direct24 => 0,
        }
    }
}

/// A TIM as the game loads it ([`parse_loaded`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loaded<'a> {
    /// The blocks; `bytes_consumed` ends after the pixel data that the
    /// pixel rectangle transfers.
    pub image: TimImage<'a>,
    /// Whether both blocks' stated lengths equal `12 + w * h * 2`. A few
    /// PAL resources state a pixel length 4 or 8 bytes short; the game loads them
    /// anyway (see [`parse_loaded`]).
    pub lengths_match: bool,
}

/// Parse a TIM the way libgs ReadTIM (800A9228/800A9574) and the game's
/// LoadImage calls consume it: the CLUT block's stated length locates the
/// pixel block, and each rectangle transfers `w * h` halfwords from its
/// block's +12, whatever that block's stated length says. Unlike [`parse`],
/// a stated length that differs from the rectangle is accepted (and
/// reported); the transferred bytes must still lie inside `data`, and both
/// rectangles inside VRAM.
pub fn parse_loaded(data: &[u8]) -> Result<Loaded<'_>> {
    if word(data, 0)? != 0x10 {
        return Err(Error::Invalid("expected TIM magic 0x10"));
    }
    let flags = word(data, 4)?;
    if flags & !0xb != 0 {
        return Err(Error::Unsupported("TIM flags"));
    }
    let depth = match flags & 3 {
        0 => Depth::Indexed4,
        1 => Depth::Indexed8,
        2 => Depth::Direct15,
        _ => Depth::Direct24,
    };
    let mut pos = 8;
    let mut lengths_match = true;
    let clut = if flags & 8 != 0 {
        let (stated, block, _) = loaded_block(data, pos)?;
        lengths_match &= stated == 12 + block.data.len();
        // ReadTIM advances by the stated length in whole words.
        pos += stated & !3;
        Some(block)
    } else {
        None
    };
    let (stated, pixels, end) = loaded_block(data, pos)?;
    lengths_match &= stated == 12 + pixels.data.len();
    Ok(Loaded {
        image: TimImage {
            depth,
            clut,
            pixels,
            bytes_consumed: end,
        },
        lengths_match,
    })
}

/// One block at `pos` as LoadImage reads it: (stated length, block with the
/// rectangle's `w * h * 2` bytes, end of those bytes).
fn loaded_block(data: &[u8], pos: usize) -> Result<(usize, Block<'_>, usize)> {
    let stated = word(data, pos)? as usize;
    if stated < 12 {
        return Err(Error::Invalid("TIM block is smaller than its header"));
    }
    let header = data
        .get(pos + 4..pos + 12)
        .ok_or(Error::Truncated("TIM block header"))?;
    let half = |offset: usize| u16::from_le_bytes([header[offset], header[offset + 1]]);
    let (x, y, width, height) = (half(0), half(2), half(4), half(6));
    if usize::from(x) + usize::from(width) > 1024 || usize::from(y) + usize::from(height) > 512 {
        return Err(Error::Invalid("TIM block extends outside VRAM"));
    }
    let start = pos + 12;
    let end = start + usize::from(width) * usize::from(height) * 2;
    let bytes = data.get(start..end).ok_or(Error::Truncated("TIM block"))?;
    Ok((
        stated,
        Block {
            x,
            y,
            width,
            height,
            data: bytes,
        },
        end,
    ))
}

/// Every strictly valid TIM ([`parse`]) embedded in `data` at a 4-byte
/// aligned offset, with non-empty rectangles, in order. The scan resumes
/// after each TIM found, so a run of TIMs read by successive ReadTIM calls
/// is found whole. Strictness (exact stated lengths, both rectangles inside
/// VRAM) keeps code and unrelated tables from matching.
pub fn scan(data: &[u8]) -> Vec<(usize, TimImage<'_>)> {
    let mut found = Vec::new();
    let mut pos = 0;
    while pos + 20 <= data.len() {
        if data[pos..pos + 4] == [0x10, 0, 0, 0]
            && let Ok(image) = parse(&data[pos..])
            && image.pixels.width > 0
            && image.pixels.height > 0
            && image
                .clut
                .is_none_or(|clut| clut.width > 0 && clut.height > 0)
        {
            found.push((pos, image));
            pos += (image.bytes_consumed + 3) & !3;
            continue;
        }
        pos += 4;
    }
    found
}

/// The texel indices of a 4- or 8-bit pixel block, row by row
/// ([`Depth::texel_width`] per row); 4-bit words hold their leftmost texel
/// in the low nibble. `None` for direct colour.
pub fn indices(block: &Block<'_>, depth: Depth) -> Option<Vec<u8>> {
    match depth {
        Depth::Indexed4 => Some(
            block
                .data
                .iter()
                .flat_map(|&byte| [byte & 15, byte >> 4])
                .collect(),
        ),
        Depth::Indexed8 => Some(block.data.to_vec()),
        Depth::Direct15 | Depth::Direct24 => None,
    }
}

/// Inverse of [`indices`]: pack 4- or 8-bit indices into the block bytes
/// (4-bit indices above 15 are rejected). `None` for direct colour.
pub fn pack_indices(indices: &[u8], depth: Depth) -> Option<Vec<u8>> {
    match depth {
        Depth::Indexed4 if indices.len().is_multiple_of(2) && indices.iter().all(|&i| i < 16) => {
            Some(
                indices
                    .chunks_exact(2)
                    .map(|pair| pair[0] | (pair[1] << 4))
                    .collect(),
            )
        }
        Depth::Indexed8 => Some(indices.to_vec()),
        _ => None,
    }
}

#[cfg(test)]
mod loaded_tests {
    use super::*;

    fn tim(flags: u32, clut: Option<(u32, [u16; 4])>, pixels: (u32, [u16; 4])) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(0x10u32.to_le_bytes());
        bytes.extend(flags.to_le_bytes());
        for (stated, rect) in clut.into_iter().chain([pixels]) {
            bytes.extend(stated.to_le_bytes());
            rect.iter()
                .for_each(|half| bytes.extend(half.to_le_bytes()));
            let count = usize::from(rect[2]) * usize::from(rect[3]);
            bytes.extend((0..count).flat_map(|i| (i as u16 | 0x100).to_le_bytes()));
        }
        bytes
    }

    #[test]
    fn loaded_view_accepts_short_stated_pixel_lengths() {
        // A 4-bit TIM whose pixel block states 8 bytes fewer than 2x8 words.
        let bytes = tim(8, Some((44, [320, 300, 16, 1])), (36, [1012, 168, 2, 8]));
        assert!(parse(&bytes).is_err());
        let loaded = parse_loaded(&bytes).unwrap();
        assert!(!loaded.lengths_match);
        assert_eq!(loaded.image.depth, Depth::Indexed4);
        assert_eq!(loaded.image.pixels.data.len(), 32);
        assert_eq!(loaded.image.bytes_consumed, bytes.len());
        assert_eq!(loaded.image.clut.unwrap().y, 300);
        // A consistent TIM matches the strict parser.
        let good = tim(9, Some((524, [0, 480, 256, 1])), (44, [640, 0, 16, 1]));
        let loaded = parse_loaded(&good).unwrap();
        assert!(loaded.lengths_match);
        assert_eq!(loaded.image, parse(&good).unwrap());
        assert!(parse_loaded(&good[..good.len() - 1]).is_err());
    }

    #[test]
    fn scan_finds_runs_and_skips_noise() {
        let first = tim(8, Some((44, [320, 300, 16, 1])), (44, [1012, 168, 2, 8]));
        let second = tim(2, None, (20, [0, 0, 2, 2]));
        let mut data = vec![0x10, 0, 0, 0, 8, 0, 0, 0];
        let at = data.len();
        data.extend(&first);
        data.extend(&second);
        let found = scan(&data);
        assert_eq!(
            found.iter().map(|(offset, _)| *offset).collect::<Vec<_>>(),
            [at, at + first.len()]
        );
        assert_eq!(found[1].1.depth, Depth::Direct15);
    }

    #[test]
    fn indices_round_trip_and_widths() {
        let block = Block {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            data: &[0x21, 0x43],
        };
        let unpacked = indices(&block, Depth::Indexed4).unwrap();
        assert_eq!(unpacked, [1, 2, 3, 4]);
        assert_eq!(
            pack_indices(&unpacked, Depth::Indexed4).unwrap(),
            block.data
        );
        assert_eq!(indices(&block, Depth::Indexed8).unwrap(), block.data);
        assert!(indices(&block, Depth::Direct15).is_none());
        assert!(pack_indices(&[16, 0], Depth::Indexed4).is_none());
        assert_eq!(Depth::Indexed4.texel_width(16), 64);
        assert_eq!(Depth::Indexed8.texel_width(16), 32);
        assert_eq!(Depth::Direct24.texel_width(480), 320);
        assert_eq!(Depth::Indexed8.palette_entries(), 256);
    }
}
