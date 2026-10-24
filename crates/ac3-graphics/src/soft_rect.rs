//! Rectangles, sprites, fills and VRAM-to-VRAM copies of the reference GPU.

use crate::soft_pixel::{Sampler, WriteState, color_scalar, texel_pair, texel_scalar};
use crate::soft_poly::Area;
use crate::{Error, Result};

const WIDTH: i32 = 1024;
const HEIGHT: i32 = 512;

/// Untextured rectangle; `x1`/`y1` are exclusive. `cheat` is the toggle the
/// reference adds to a lone 1x1 dot at (1020,511) for interlace detection.
pub(crate) fn draw_rect(
    vram: &mut [u16],
    area: Area,
    write: &WriteState,
    [mut x0, mut y0, mut x1, mut y1]: [i32; 4],
    mut color: u16,
    cheat: &mut u16,
) {
    if y0 > y1 || x0 > x1 {
        return;
    }
    if x1 < area.left || y1 < area.top {
        return;
    }
    if x0 > area.right || y0 > area.bottom {
        return;
    }
    x1 = x1.min(area.right + 1);
    y1 = y1.min(area.bottom + 1);
    x0 = x0.max(area.left);
    y0 = y0.max(area.top);
    if y0 >= HEIGHT || x0 >= WIDTH {
        return;
    }
    y1 = y1.min(HEIGHT);
    x1 = x1.min(WIDTH);
    if x1 - x0 == 1 && y1 - y0 == 1 && x0 == 1020 && y0 == 511 {
        color = color.wrapping_add(*cheat);
        *cheat ^= 1;
    }
    // Pair writes of one flat color are identical to single-pixel writes.
    for y in y0..y1 {
        let row = (y * WIDTH) as usize;
        for pixel in &mut vram[row + x0 as usize..row + x1 as usize] {
            color_scalar(pixel, color, write);
        }
    }
}

/// Textured sprite at already offset `(x, y)`; pairs start at the clipped left edge.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_sprite(
    vram: &mut [u16],
    area: Area,
    write: &WriteState,
    sampler: &Sampler,
    m: [i32; 3],
    [x, y, w, h]: [i32; 4],
    u: i32,
    v: i32,
) -> Result<()> {
    if w <= 0 || h <= 0 {
        return Ok(());
    }
    let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
    let (mut u0, mut v0) = (u, v);
    if x0 < area.left {
        u0 += area.left - x0;
        x0 = area.left;
    }
    if y0 < area.top {
        v0 += area.top - y0;
        y0 = area.top;
    }
    x1 = x1.min(area.right + 1);
    y1 = y1.min(area.bottom + 1);
    if x0 >= x1 || y0 >= y1 {
        return Ok(());
    }
    if x0 < 0 || y0 < 0 || x1 > WIDTH || y1 > HEIGHT {
        return Err(Error::Unsupported("drawing outside the 1024x512 VRAM"));
    }
    let pair_m = m.map(|m| m as u32);
    for row in y0..y1 {
        let pos_v = (v0 + (row - y0)) << 16;
        let mut pos_u = u0 << 16;
        let base = (row * WIDTH) as usize;
        let mut col = x0;
        while col + 1 < x1 {
            let first = sampler.texel(vram, pos_u, pos_v)?;
            let second = sampler.texel(vram, pos_u.wrapping_add(0x10000), pos_v)?;
            let at = base + col as usize;
            texel_pair(
                &mut vram[at..at + 2],
                u32::from(first) | (u32::from(second) << 16),
                pair_m,
                write,
            );
            pos_u = pos_u.wrapping_add(0x20000);
            col += 2;
        }
        if col < x1 {
            let texel = sampler.texel(vram, pos_u, pos_v)?;
            texel_scalar(&mut vram[base + col as usize], texel, m, write);
        }
    }
    Ok(())
}

/// GP0 02 body after decoding: no draw area, no mask, clipped to VRAM.
pub(crate) fn fill_area(vram: &mut [u16], [x0, y0, x1, y1]: [i32; 4], color: u16) {
    if y0 > y1 || x0 > x1 || !(0..WIDTH).contains(&x0) || !(0..HEIGHT).contains(&y0) {
        return;
    }
    let (x1, y1) = (x1.min(WIDTH), y1.min(HEIGHT));
    for y in y0..y1 {
        let row = (y * WIDTH) as usize;
        vram[row + x0 as usize..row + x1 as usize].fill(color);
    }
}

/// GP0 80 body: forward row-major copy; masks force the per-pixel path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_rect(
    vram: &mut [u16],
    [sx, sy]: [i32; 2],
    [dx, dy]: [i32; 2],
    [w, h]: [i32; 2],
    set_mask: u16,
    check_mask: bool,
) {
    if sx == dx && sy == dy && set_mask == 0 {
        return;
    }
    if w <= 0 || h <= 0 {
        return;
    }
    if sy + h > HEIGHT
        || sx + w > WIDTH
        || dy + h > HEIGHT
        || dx + w > WIDTH
        || check_mask
        || set_mask != 0
    {
        for j in 0..h {
            let drow = ((dy + j) & (HEIGHT - 1)) * WIDTH;
            let srow = ((sy + j) & (HEIGHT - 1)) * WIDTH;
            for i in 0..w {
                let to = (drow + ((dx + i) & (WIDTH - 1))) as usize;
                if check_mask && vram[to] & 0x8000 != 0 {
                    continue;
                }
                vram[to] = vram[(srow + ((sx + i) & (WIDTH - 1))) as usize] | set_mask;
            }
        }
        return;
    }
    for j in 0..h {
        let from = ((sy + j) * WIDTH + sx) as usize;
        let to = ((dy + j) * WIDTH + dx) as usize;
        if w & 1 != 0 {
            for i in 0..w as usize {
                vram[to + i] = vram[from + i];
            }
        } else {
            // The reference moves aligned-or-not 32-bit pairs; overlap smears per pair.
            for i in (0..w as usize).step_by(2) {
                let pair = [vram[from + i], vram[from + i + 1]];
                vram[to + i] = pair[0];
                vram[to + i + 1] = pair[1];
            }
        }
    }
}
