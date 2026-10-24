//! Texel fetch and pixel write rules of the reference software GPU.
//!
//! Single-pixel ("scalar") and two-pixel ("packed") writes are separate,
//! because the reference renderer gives them slightly different arithmetic
//! (notably quarter blending and textured subtraction). Callers choose the
//! same path the reference does: rectangles, sprites and flat polygons write
//! pairs from the clipped span start, Gouraud slow paths write single pixels.

use crate::{BlendMode, Error, Result};

/// 4x4 dither offsets indexed by `(y & 3) * 4 + (x & 3)` in VRAM coordinates.
pub(crate) const DITHER: [i32; 16] = [-4, 0, -3, 1, 2, -2, 3, -1, -3, 1, -4, 0, 3, -1, 2, -2];

/// Blend/mask state shared by every write of one primitive.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WriteState {
    pub abr: BlendMode,
    pub semi: bool,
    pub check_mask: bool,
    pub set_mask: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum TexDepth {
    Clut4,
    Clut8,
    Direct15,
}

/// Texture addressing for one primitive: page base, window and palette row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sampler {
    pub depth: TexDepth,
    pub base_x: i32,
    pub base_y: i32,
    pub mask_x: i32,
    pub mask_y: i32,
    pub window_x: i32,
    pub window_y: i32,
    /// Linear halfword address of the palette, `(clut_y << 10) + clut_x`.
    pub clut: i32,
}

impl Sampler {
    /// Fetch the texel at 16.16 texture coordinates.
    pub fn texel(&self, vram: &[u16], pos_u: i32, pos_v: i32) -> Result<u16> {
        let u = ((pos_u >> 16) & self.mask_x) | self.window_x;
        let v = ((pos_v >> 16) & self.mask_y) | self.window_y;
        let read = |index: i32| -> Result<u16> {
            usize::try_from(index)
                .ok()
                .and_then(|index| vram.get(index).copied())
                .ok_or(Error::Unsupported("texture or palette read outside VRAM"))
        };
        match self.depth {
            TexDepth::Direct15 => read((u + self.base_x) + ((v + self.base_y) << 10)),
            TexDepth::Clut4 | TexDepth::Clut8 => {
                // Byte addressing over little-endian halfwords.
                let byte = (v << 11) + (self.base_y << 11) + (self.base_x << 1);
                let (byte, index_shift, index_mask) = if self.depth == TexDepth::Clut4 {
                    (byte + (u >> 1), (u & 1) << 2, 0xf)
                } else {
                    (byte + u, 0, 0xff)
                };
                let word = read(byte >> 1)?;
                let value = i32::from(word >> ((byte & 1) * 8)) & 0xff;
                read(self.clut + ((value >> index_shift) & index_mask))
            }
        }
    }
}

fn max0(value: i32) -> i32 {
    if value < 0 { 0 } else { value }
}

/// Native-position saturation of the three channels (bits 0, 5 and 10).
fn saturate(c0: i32, c1: i32, c2: i32) -> u16 {
    let c0 = if c0 & 0x7fff_ffe0 != 0 { 0x1f } else { c0 };
    let c1 = if c1 & 0x7fff_fc00 != 0 { 0x3e0 } else { c1 };
    let c2 = if c2 & 0x7fff_8000 != 0 { 0x7c00 } else { c2 };
    ((c2 & 0x7c00) | (c1 & 0x3e0) | (c0 & 0x1f)) as u16
}

/// Textured single-pixel write; `m` are the modulation factors (128 = identity).
pub(crate) fn texel_scalar(pixel: &mut u16, texel: u16, m: [i32; 3], state: &WriteState) {
    if texel == 0 {
        return;
    }
    let back = *pixel;
    if state.check_mask && back & 0x8000 != 0 {
        return;
    }
    let flags = state.set_mask | (texel & 0x8000);
    let (t, d) = (i32::from(texel), i32::from(back));
    let color = if state.semi && texel & 0x8000 != 0 {
        match state.abr {
            BlendMode::Average => {
                let half = |shift: i32, m: i32| {
                    (((d >> shift) & 0x1f) + ((((t >> shift) & 0x1f) * m) >> 7)) >> 1
                };
                saturate(half(0, m[0]), half(5, m[1]) << 5, half(10, m[2]) << 10)
            }
            BlendMode::Add => saturate(
                (d & 0x1f) + (((t & 0x1f) * m[0]) >> 7),
                (d & 0x3e0) + (((t & 0x3e0) * m[1]) >> 7),
                (d & 0x7c00) + (((t & 0x7c00) * m[2]) >> 7),
            ),
            BlendMode::Subtract => saturate(
                max0((d & 0x1f) - (((t & 0x1f) * m[0]) >> 7)),
                max0((d & 0x3e0) - (((t & 0x3e0) * m[1]) >> 7)),
                max0((d & 0x7c00) - (((t & 0x7c00) * m[2]) >> 7)),
            ),
            BlendMode::AddQuarter => saturate(
                (d & 0x1f) + ((((t & 0x1f) >> 2) * m[0]) >> 7),
                (d & 0x3e0) + ((((t & 0x3e0) >> 2) * m[1]) >> 7),
                (d & 0x7c00) + ((((t & 0x7c00) >> 2) * m[2]) >> 7),
            ),
        }
    } else {
        saturate(
            ((t & 0x1f) * m[0]) >> 7,
            ((t & 0x3e0) * m[1]) >> 7,
            ((t & 0x7c00) * m[2]) >> 7,
        )
    };
    *pixel = color | flags;
}

fn modulate_pair(texels: u32, shift: u32, m: u32) -> u32 {
    (((texels >> shift) & 0x001f_001f).wrapping_mul(m) & 0xff80_ff80) >> 7
}
fn modulate_quarter_pair(texels: u32, shift: u32, m: u32) -> u32 {
    ((((texels >> shift) & 0x001c_001c) >> 2).wrapping_mul(m) & 0xff80_ff80) >> 7
}
fn subtract_pair(back: u32, front: u32) -> u32 {
    let low = max0((back & 0x1f) as i32 - (front & 0x3f) as i32) as u32;
    let high = max0((back & 0x1f_0000) as i32 - (front & 0x3f_0000) as i32) as u32;
    low | high
}
fn saturate_pair(mut value: u32) -> u32 {
    if value & 0x7fe0 != 0 {
        value = (value & 0xffff_0000) | 0x1f;
    }
    if value & 0x7fe0_0000 != 0 {
        value = (value & 0xffff) | 0x1f_0000;
    }
    value
}

/// Textured two-pixel write; `texels` holds the left pixel in its low half.
pub(crate) fn texel_pair(pixels: &mut [u16], texels: u32, m: [u32; 3], state: &WriteState) {
    if texels == 0 {
        return;
    }
    let back = u32::from(pixels[0]) | (u32::from(pixels[1]) << 16);
    let flags = (u32::from(state.set_mask) * 0x1_0001) | (texels & 0x8000_8000);
    let plain = [0, 5, 10].map(|shift| modulate_pair(texels, shift, m[shift as usize / 5]));
    let mut channels = if state.semi && texels & 0x8000_8000 != 0 {
        let mut blended = [0u32; 3];
        for (i, shift) in [0u32, 5, 10].into_iter().enumerate() {
            let b = (back >> shift) & 0x001f_001f;
            blended[i] = match state.abr {
                BlendMode::Average => {
                    (((b << 7).wrapping_add(((texels >> shift) & 0x001f_001f).wrapping_mul(m[i])))
                        & 0xff00_ff00)
                        >> 8
                }
                BlendMode::Add => b + plain[i],
                BlendMode::Subtract => subtract_pair(b, plain[i]),
                BlendMode::AddQuarter => b + modulate_quarter_pair(texels, shift, m[i]),
            };
        }
        // A texel without STP is never blended, even beside one that is.
        for (value, plain) in blended.iter_mut().zip(plain) {
            if texels & 0x8000 == 0 {
                *value = (*value & 0xffff_0000) | (plain & 0xffff);
            }
            if texels & 0x8000_0000 == 0 {
                *value = (*value & 0xffff) | (plain & 0xffff_0000);
            }
        }
        blended
    } else {
        plain
    };
    channels = channels.map(saturate_pair);
    let fresh = channels[0] | (channels[1] << 5) | (channels[2] << 10) | flags;
    let mut out = fresh;
    if state.check_mask {
        if texels & 0xffff == 0 || back & 0x8000 != 0 {
            out = (out & 0xffff_0000) | (back & 0xffff);
        }
        if texels & 0xffff_0000 == 0 || back & 0x8000_0000 != 0 {
            out = (out & 0xffff) | (back & 0xffff_0000);
        }
    } else if texels & 0xffff == 0 {
        out = (back & 0xffff) | (fresh & 0xffff_0000);
    } else if texels & 0xffff_0000 == 0 {
        out = (fresh & 0xffff) | (back & 0xffff_0000);
    }
    pixels[0] = out as u16;
    pixels[1] = (out >> 16) as u16;
}

/// Untextured single-pixel write of an RGB555 color.
pub(crate) fn color_scalar(pixel: &mut u16, color: u16, state: &WriteState) {
    let back = *pixel;
    if state.check_mask && back & 0x8000 != 0 {
        return;
    }
    if !state.semi {
        *pixel = color | state.set_mask;
        return;
    }
    let (c, d) = (i32::from(color), i32::from(back));
    let value = match state.abr {
        BlendMode::Average => {
            let half = |shift: i32| (((d >> shift) & 0x1f) + ((c >> shift) & 0x1f)) >> 1;
            saturate(half(0), half(5) << 5, half(10) << 10)
        }
        BlendMode::Add => saturate(
            (d & 0x1f) + (c & 0x1f),
            (d & 0x3e0) + (c & 0x3e0),
            (d & 0x7c00) + (c & 0x7c00),
        ),
        BlendMode::Subtract => saturate(
            max0((d & 0x1f) - (c & 0x1f)),
            max0((d & 0x3e0) - (c & 0x3e0)),
            max0((d & 0x7c00) - (c & 0x7c00)),
        ),
        BlendMode::AddQuarter => saturate(
            (d & 0x1f) + ((c & 0x1f) >> 2),
            (d & 0x3e0) + ((c & 0x3e0) >> 2),
            (d & 0x7c00) + ((c & 0x7c00) >> 2),
        ),
    };
    *pixel = value | state.set_mask;
}

/// Eight-bit shaded write with blending, saturation and ordered dithering.
/// `index` is the linear VRAM halfword index, which selects the dither cell.
pub(crate) fn shade_dither(
    pixel: &mut u16,
    index: usize,
    shade: [i32; 3],
    semi: bool,
    abr: BlendMode,
    flags: u16,
) {
    let back = *pixel;
    let mut c = shade;
    if semi {
        for (i, value) in c.iter_mut().enumerate() {
            let b = i32::from((back >> (i * 5)) & 0x1f) << 3;
            *value = match abr {
                BlendMode::Average => (b + *value) >> 1,
                BlendMode::Add => b + *value,
                BlendMode::Subtract => max0(b - *value),
                BlendMode::AddQuarter => b + (*value >> 2),
            };
        }
    }
    let offset = DITHER[((index >> 10) & 3) * 4 + (index & 3)];
    let c = c.map(|value| {
        let value = if value & 0x7fff_ff00 != 0 {
            0xff
        } else {
            value
        };
        ((value + offset).clamp(0, 255) >> 3) as u16
    });
    *pixel = (c[2] << 10) | (c[1] << 5) | c[0] | flags;
}

/// 24-bit command color to RGB555 without a mask bit.
pub(crate) fn command_color(rgb: u32) -> u16 {
    (((rgb >> 3) & 0x1f) | ((rgb >> 6) & 0x3e0) | ((rgb >> 9) & 0x7c00)) as u16
}

/// Three 16.16 color accumulators (8-bit value in bits 16..23) to RGB555.
pub(crate) fn high_aligned_color(c: [i32; 3]) -> u16 {
    (((c[0] >> 19) & 0x1f) | ((c[1] >> 14) & 0x3e0) | ((c[2] >> 9) & 0x7c00)) as u16
}
