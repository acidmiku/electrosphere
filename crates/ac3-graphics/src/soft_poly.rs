//! Triangle scan conversion of the reference software GPU.
//!
//! Vertices are sorted by Y with three compare-swaps. The long edge (top to
//! bottom) sits left or right according to the sign of the 16.16 horizontal
//! distance from the middle vertex. Edge X and colors are interpolated per row
//! in closed form (`start + diff * k / height`, truncating), edge UVs with a
//! per-row step truncated before multiplying. Horizontal gradients use
//! `((a << 16 + frac * b) << 16) / longest` with `|longest| >= 0x1000`.
//! Spans cover `ceil(left)..=floor(right - 1/65536)`, rows `top..bottom`.

use crate::soft_pixel::{
    Sampler, WriteState, color_scalar, command_color, high_aligned_color, shade_dither, texel_pair,
    texel_scalar,
};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Vertex {
    pub x: i32,
    pub y: i32,
    pub u: i32,
    pub v: i32,
    pub rgb: u32,
}

/// Draw-area bounds; `right`/`bottom` are inclusive.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Shading {
    /// Flat untextured color (24-bit command color).
    Flat(u32),
    /// Flat textured, modulated by `m` (128 = raw).
    FlatTextured(Sampler, [i32; 3]),
    Gouraud,
    GouraudTextured(Sampler),
}

#[derive(Clone, Copy)]
struct Fixed {
    x: i32,
    y: i32,
    u: i32,
    v: i32,
    c: [i32; 3],
}

fn shl10idiv(x: i32, y: i32) -> i32 {
    ((i64::from(x) << 10) / i64::from(y)) as i32
}

struct EdgeValue {
    x: i32,
    u: i32,
    v: i32,
    c: [i32; 3],
}

/// Value of a section (top `a`, bottom `b`) at row `y`.
fn edge_at(a: &Fixed, b: &Fixed, y: i32) -> EdgeValue {
    let height = b.y - a.y;
    let k = y - a.y;
    let exact = |start: i32, end: i32| {
        start.wrapping_add(
            (i64::from(end.wrapping_sub(start)) * i64::from(k) / i64::from(height)) as i32,
        )
    };
    let stepped =
        |start: i32, end: i32| start.wrapping_add(k.wrapping_mul(end.wrapping_sub(start) / height));
    EdgeValue {
        x: exact(a.x, b.x),
        u: stepped(a.u, b.u),
        v: stepped(a.v, b.v),
        c: std::array::from_fn(|i| exact(a.c[i], b.c[i])),
    }
}

pub(crate) struct Target<'a> {
    pub vram: &'a mut [u16],
    pub area: Area,
    pub write: WriteState,
    pub dither: bool,
}

fn pixel_index(x: i32, y: i32) -> Result<usize> {
    if (0..1024).contains(&x) && (0..512).contains(&y) {
        Ok(y as usize * 1024 + x as usize)
    } else {
        Err(Error::Unsupported("drawing outside the 1024x512 VRAM"))
    }
}

/// Rasterize one triangle exactly as the reference `drawPoly3Raster`.
pub(crate) fn draw_triangle(
    target: &mut Target<'_>,
    vertices: [Vertex; 3],
    shading: Shading,
) -> Result<()> {
    let area = target.area;
    if area.top >= area.bottom || area.left >= area.right {
        return Ok(());
    }
    if vertices.iter().all(|v| v.x > area.right)
        || vertices.iter().all(|v| v.y > area.bottom)
        || vertices.iter().all(|v| v.x < area.left)
        || vertices.iter().all(|v| v.y < area.top)
    {
        return Ok(());
    }
    let mut p = vertices.map(|v| Fixed {
        x: v.x << 16,
        y: v.y,
        u: v.u << 16,
        v: v.v << 16,
        c: std::array::from_fn(|i| (((v.rgb >> (i * 8)) & 0xff) << 16) as i32),
    });
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
        return Ok(());
    }
    let temp = ((p[1].y - p[0].y) << 16) / height;
    let mut longest = temp
        .wrapping_mul((p[2].x - p[0].x) >> 16)
        .wrapping_add(p[0].x - p[1].x);
    if longest == 0 {
        return Ok(());
    }
    // Long edge on the left when the middle vertex lies to its right.
    let long_left = longest < 0;
    let interpolated = !matches!(shading, Shading::Flat(_));
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
    let du = gradient([p[0].u, p[1].u, p[2].u]);
    let dv = gradient([p[0].v, p[1].v, p[2].v]);
    let dc: [i32; 3] = std::array::from_fn(|i| gradient([p[0].c[i], p[1].c[i], p[2].c[i]]));
    let short_at = |y: i32| {
        if y < p[1].y {
            edge_at(&p[0], &p[1], y)
        } else {
            edge_at(&p[1], &p[2], y)
        }
    };
    let y_end = (p[2].y - 1).min(area.bottom);
    for y in p[0].y.max(area.top)..=y_end {
        let long = edge_at(&p[0], &p[2], y);
        let short = short_at(y);
        let (left, right) = if long_left {
            (long, short)
        } else {
            (short, long)
        };
        let mut x0 = (left.x.wrapping_add(0xffff)) >> 16;
        let x1 = ((right.x.wrapping_sub(1)) >> 16).min(area.right);
        if x1 < x0 {
            continue;
        }
        let seed = |start: i32, delta: i32| {
            let distance = (i64::from(x0) << 16) - i64::from(left.x);
            start
                .wrapping_add(((distance * i64::from(delta)) >> 16) as i32)
                .wrapping_add(0x8000)
        };
        let mut u = seed(left.u, du);
        let mut v = seed(left.v, dv);
        let mut c: [i32; 3] = std::array::from_fn(|i| seed(left.c[i], dc[i]));
        if x0 < area.left {
            let skip = area.left - x0;
            x0 = area.left;
            u = u.wrapping_add(skip.wrapping_mul(du));
            v = v.wrapping_add(skip.wrapping_mul(dv));
            for i in 0..3 {
                c[i] = c[i].wrapping_add(skip.wrapping_mul(dc[i]));
            }
        }
        if x0 > x1 {
            continue;
        }
        let row = pixel_index(x0, y)?;
        pixel_index(x1, y)?;
        let write = target.write;
        match shading {
            Shading::Flat(rgb) => {
                let color = command_color(rgb);
                for pixel in &mut target.vram[row..=row + (x1 - x0) as usize] {
                    color_scalar(pixel, color, &write);
                }
            }
            Shading::FlatTextured(sampler, m) => {
                let pair_m = m.map(|m| m as u32);
                let mut x = x0;
                while x < x1 {
                    let first = sampler.texel(target.vram, u, v)?;
                    let second =
                        sampler.texel(target.vram, u.wrapping_add(du), v.wrapping_add(dv))?;
                    let at = row + (x - x0) as usize;
                    texel_pair(
                        &mut target.vram[at..at + 2],
                        u32::from(first) | (u32::from(second) << 16),
                        pair_m,
                        &write,
                    );
                    u = u.wrapping_add(du.wrapping_mul(2));
                    v = v.wrapping_add(dv.wrapping_mul(2));
                    x += 2;
                }
                if x == x1 {
                    let texel = sampler.texel(target.vram, u, v)?;
                    texel_scalar(&mut target.vram[row + (x - x0) as usize], texel, m, &write);
                }
            }
            Shading::Gouraud => {
                for x in x0..=x1 {
                    let at = row + (x - x0) as usize;
                    let pixel = &mut target.vram[at];
                    if target.dither {
                        if !(write.check_mask && *pixel & 0x8000 != 0) {
                            shade_dither(
                                pixel,
                                at,
                                c.map(|c| c >> 16),
                                write.semi,
                                write.abr,
                                write.set_mask,
                            );
                        }
                    } else {
                        color_scalar(pixel, high_aligned_color(c), &write);
                    }
                    for i in 0..3 {
                        c[i] = c[i].wrapping_add(dc[i]);
                    }
                }
            }
            Shading::GouraudTextured(sampler)
                if !(target.dither || write.semi || write.check_mask) =>
            {
                // Fast path: pixel pairs, both modulated by the first pixel's
                // shade (the reference truncates the packed shade to 16 bits).
                let mut x = x0;
                while x < x1 {
                    let first = sampler.texel(target.vram, u, v)?;
                    let second =
                        sampler.texel(target.vram, u.wrapping_add(du), v.wrapping_add(dv))?;
                    let at = row + (x - x0) as usize;
                    let shade = c.map(|c| i32::from((c >> 16) as i16) as u32);
                    texel_pair(
                        &mut target.vram[at..at + 2],
                        u32::from(first) | (u32::from(second) << 16),
                        shade,
                        &write,
                    );
                    u = u.wrapping_add(du.wrapping_mul(2));
                    v = v.wrapping_add(dv.wrapping_mul(2));
                    for i in 0..3 {
                        c[i] = c[i].wrapping_add(dc[i].wrapping_mul(2));
                    }
                    x += 2;
                }
                if x == x1 {
                    let texel = sampler.texel(target.vram, u, v)?;
                    let shade = c.map(|c| i32::from((c >> 16) as i16));
                    texel_scalar(
                        &mut target.vram[row + (x - x0) as usize],
                        texel,
                        shade,
                        &write,
                    );
                }
            }
            Shading::GouraudTextured(sampler) => {
                for x in x0..=x1 {
                    let at = row + (x - x0) as usize;
                    let texel = sampler.texel(target.vram, u, v)?;
                    let shade = c.map(|c| c >> 16);
                    let pixel = &mut target.vram[at];
                    if target.dither {
                        if texel != 0 && !(write.check_mask && *pixel & 0x8000 != 0) {
                            let t = i32::from(texel);
                            let modulated: [i32; 3] =
                                std::array::from_fn(|i| (((t >> (i * 5)) & 0x1f) * shade[i]) >> 4);
                            shade_dither(
                                pixel,
                                at,
                                modulated,
                                write.semi && texel & 0x8000 != 0,
                                write.abr,
                                write.set_mask | (texel & 0x8000),
                            );
                        }
                    } else {
                        // The non-dithered writer takes 16-bit shade factors.
                        let shade = shade.map(|s| i32::from(s as i16));
                        texel_scalar(pixel, texel, shade, &write);
                    }
                    u = u.wrapping_add(du);
                    v = v.wrapping_add(dv);
                    for i in 0..3 {
                        c[i] = c[i].wrapping_add(dc[i]);
                    }
                }
            }
        }
    }
    Ok(())
}
