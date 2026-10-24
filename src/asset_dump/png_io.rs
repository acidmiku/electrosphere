//! PNG encoding of PlayStation texels, with the dump's alpha convention.
//!
//! A PlayStation colour is RGB555 plus the semi-transparency bit (STP,
//! bit 15). Textured primitives never draw the colour 0x0000; a texel with
//! STP set is blended when its primitive is semi-transparent and drawn
//! opaque otherwise; STP clear (and not 0x0000) is always opaque. The dump
//! writes those three cases as alpha 0, [`STP_ALPHA`] and 255, so every
//! colour round-trips exactly: RGB555 channels expand to 8 bits as
//! `(c << 3) | (c >> 2)` and return as `c >> 3`.
//!
//! 4- and 8-bit textures are written as indexed PNGs (`PLTE` + `tRNS`) so
//! the texel indices survive too; 15-bit ones as RGBA, 24-bit ones as RGB.
use crate::Result;
use std::io::Cursor;

/// Alpha written for a colour whose STP bit is set (other than 0x8000's
/// own black, which is written the same way: alpha 128, RGB 0).
pub const STP_ALPHA: u8 = 128;

/// RGBA8 of an RGB555/STP word in the dump's convention.
pub fn rgba(color: u16) -> [u8; 4] {
    let [r, g, b, _] = ac3_graphics::rgba8(color);
    let alpha = if color == 0 {
        0
    } else if color & 0x8000 != 0 {
        STP_ALPHA
    } else {
        255
    };
    [r, g, b, alpha]
}

/// The RGB555/STP word of a dumped RGBA8 pixel; the exact inverse of
/// [`rgba`]. Alpha 0 gives 0x0000 (transparent), 255 a colour with STP
/// clear, anything else STP set. Opaque black (0, 0, 0, 255) therefore
/// becomes 0x0000, which the PlayStation never draws: original art uses
/// 0x8000 or a near-black instead.
pub fn color(rgba: [u8; 4]) -> u16 {
    if rgba[3] == 0 {
        return 0;
    }
    let channel = |value: u8| u16::from(value >> 3);
    let word = channel(rgba[0]) | (channel(rgba[1]) << 5) | (channel(rgba[2]) << 10);
    if rgba[3] == 255 { word } else { word | 0x8000 }
}

/// Texels to encode.
pub enum Pixels<'a> {
    /// 4- or 8-bit indices (one per byte, row-major) and their palette
    /// (RGB555/STP; padded with 0x0000 to 2^bits entries).
    Indexed {
        bits: u8,
        indices: &'a [u8],
        palette: &'a [u16],
    },
    /// RGB555/STP colours, row-major.
    Color15(&'a [u16]),
    /// Packed 24-bit RGB bytes, row-major.
    Rgb24(&'a [u8]),
}

/// A decoded dump PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    Indexed {
        bits: u8,
        width: usize,
        height: usize,
        indices: Vec<u8>,
        palette: Vec<u16>,
    },
    Color15 {
        width: usize,
        height: usize,
        colors: Vec<u16>,
    },
    Rgb24 {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
}

/// Encode `width` x `height` texels as a PNG (deterministic output).
pub fn encode(width: usize, height: usize, pixels: Pixels<'_>) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, u32::try_from(width)?, u32::try_from(height)?);
    let count = width * height;
    let data: Vec<u8> = match pixels {
        Pixels::Indexed {
            bits,
            indices,
            palette,
        } => {
            if !matches!(bits, 4 | 8) || indices.len() != count {
                return Err("indexed PNG needs 4/8-bit indices for every texel".into());
            }
            let entries = 1usize << bits;
            if palette.len() > entries || indices.iter().any(|&i| usize::from(i) >= entries) {
                return Err("palette or index outside the PNG bit depth".into());
            }
            let full: Vec<u16> = (0..entries)
                .map(|i| palette.get(i).copied().unwrap_or(0))
                .collect();
            let colors: Vec<[u8; 4]> = full.iter().map(|&c| rgba(c)).collect();
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(if bits == 4 {
                png::BitDepth::Four
            } else {
                png::BitDepth::Eight
            });
            encoder.set_palette(
                colors
                    .iter()
                    .flat_map(|c| [c[0], c[1], c[2]])
                    .collect::<Vec<_>>(),
            );
            encoder.set_trns(colors.iter().map(|c| c[3]).collect::<Vec<_>>());
            if bits == 8 {
                indices.to_vec()
            } else {
                // PNG packs the leftmost texel in the high nibble; rows are
                // padded to whole bytes.
                indices
                    .chunks(width.max(1))
                    .flat_map(|row| {
                        row.chunks(2)
                            .map(|pair| (pair[0] << 4) | pair.get(1).copied().unwrap_or(0))
                            .collect::<Vec<_>>()
                    })
                    .collect()
            }
        }
        Pixels::Color15(colors) => {
            if colors.len() != count {
                return Err("15-bit PNG needs a colour for every texel".into());
            }
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            colors.iter().flat_map(|&c| rgba(c)).collect()
        }
        Pixels::Rgb24(bytes) => {
            if bytes.len() != count * 3 {
                return Err("24-bit PNG needs three bytes for every texel".into());
            }
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            bytes.to_vec()
        }
    };
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&data)?;
    writer.finish()?;
    Ok(output)
}

/// Decode a PNG written by [`encode`] (indexed 4/8-bit, RGBA8 or RGB8).
pub fn decode(bytes: &[u8]) -> Result<Decoded> {
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or("PNG output buffer too large")?;
    let mut buffer = vec![0; size];
    let frame = reader.next_frame(&mut buffer)?;
    let (width, height) = (frame.width as usize, frame.height as usize);
    let stride = frame.line_size;
    let info = reader.info();
    match (info.color_type, info.bit_depth) {
        (png::ColorType::Indexed, depth @ (png::BitDepth::Four | png::BitDepth::Eight)) => {
            let bits = if depth == png::BitDepth::Four { 4 } else { 8 };
            let plte = info.palette.as_deref().ok_or("indexed PNG without PLTE")?;
            let trns = info.trns.as_deref().unwrap_or(&[]);
            let palette = plte
                .chunks_exact(3)
                .enumerate()
                .map(|(i, c)| color([c[0], c[1], c[2], trns.get(i).copied().unwrap_or(255)]))
                .collect();
            let mut indices = Vec::with_capacity(width * height);
            for row in buffer.chunks(stride).take(height) {
                if bits == 8 {
                    indices.extend_from_slice(&row[..width]);
                } else {
                    indices.extend(
                        (0..width)
                            .map(|x| (row[x / 2] >> if x.is_multiple_of(2) { 4 } else { 0 }) & 15),
                    );
                }
            }
            Ok(Decoded::Indexed {
                bits,
                width,
                height,
                indices,
                palette,
            })
        }
        (png::ColorType::Rgba, png::BitDepth::Eight) => Ok(Decoded::Color15 {
            width,
            height,
            colors: buffer
                .chunks(stride)
                .take(height)
                .flat_map(|row| {
                    row[..width * 4]
                        .chunks_exact(4)
                        .map(|p| color([p[0], p[1], p[2], p[3]]))
                        .collect::<Vec<_>>()
                })
                .collect(),
        }),
        (png::ColorType::Rgb, png::BitDepth::Eight) => Ok(Decoded::Rgb24 {
            width,
            height,
            bytes: buffer
                .chunks(stride)
                .take(height)
                .flat_map(|row| row[..width * 3].to_vec())
                .collect(),
        }),
        other => Err(format!("unsupported dump PNG layout {other:?}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_convention_round_trips_every_color() {
        assert_eq!(rgba(0), [0, 0, 0, 0]);
        assert_eq!(rgba(0x8000), [0, 0, 0, STP_ALPHA]);
        assert_eq!(rgba(0x7fff), [255, 255, 255, 255]);
        assert_eq!(rgba(0x801f), [255, 0, 0, STP_ALPHA]);
        for word in 0..=u16::MAX {
            assert_eq!(color(rgba(word)), word, "{word:#06x}");
        }
    }

    #[test]
    fn indexed_and_direct_pngs_round_trip() {
        let palette: Vec<u16> = (0..16).map(|i| (i * 0x0421) | ((i & 1) << 15)).collect();
        let indices: Vec<u8> = (0..6 * 3).map(|i| (i * 7 % 16) as u8).collect();
        let png = encode(
            6,
            3,
            Pixels::Indexed {
                bits: 4,
                indices: &indices,
                palette: &palette,
            },
        )
        .unwrap();
        assert_eq!(
            decode(&png).unwrap(),
            Decoded::Indexed {
                bits: 4,
                width: 6,
                height: 3,
                indices: indices.clone(),
                palette: palette.clone(),
            }
        );
        let wide: Vec<u8> = (0..=255).collect();
        let png = encode(
            16,
            16,
            Pixels::Indexed {
                bits: 8,
                indices: &wide,
                palette: &palette,
            },
        )
        .unwrap();
        let Decoded::Indexed { palette: read, .. } = decode(&png).unwrap() else {
            panic!("indexed");
        };
        assert_eq!(&read[..16], &palette[..]);
        assert!(read[16..].iter().all(|&c| c == 0), "padded transparent");
        let colors = [0u16, 0x8000, 0x7fff, 0x1234];
        let png = encode(2, 2, Pixels::Color15(&colors)).unwrap();
        assert_eq!(
            decode(&png).unwrap(),
            Decoded::Color15 {
                width: 2,
                height: 2,
                colors: colors.to_vec()
            }
        );
        assert!(encode(2, 2, Pixels::Color15(&colors[..3])).is_err());
    }
}
