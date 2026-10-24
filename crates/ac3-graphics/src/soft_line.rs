//! Line segments of the reference software GPU.
//!
//! Axis-aligned lines walk an inclusive range clamped to the inclusive draw
//! area. Other lines step a Bresenham walker (initial error `3|dy|-|dx|` for
//! X-major, `2|dx|-|dy|` for Y-major, minor step when the error is positive)
//! and plot only inside the draw area with an exclusive right/bottom bound.
//! Endpoint swaps keep the color order, as the reference does.

use crate::soft_pixel::{WriteState, color_scalar, command_color, high_aligned_color};
use crate::soft_poly::Area;

struct Colors {
    gouraud: bool,
    start: [i32; 3],
    delta: [i32; 3],
    steps: i32,
    flat: u16,
}

impl Colors {
    fn new(gouraud: bool, rgb0: u32, rgb1: u32, steps: i32) -> Self {
        let high = |rgb: u32| std::array::from_fn(|i| (((rgb >> (i * 8)) & 0xff) << 16) as i32);
        let (start, end): ([i32; 3], [i32; 3]) = (high(rgb0), high(rgb1));
        Self {
            gouraud,
            start,
            delta: std::array::from_fn(|i| end[i] - start[i]),
            steps,
            flat: command_color(rgb0),
        }
    }
    fn at(&self, step: i32) -> u16 {
        if !self.gouraud {
            return self.flat;
        }
        let value = std::array::from_fn(|i| {
            if self.steps == 0 {
                self.start[i]
            } else {
                self.start[i]
                    + (i64::from(self.delta[i]) * i64::from(step) / i64::from(self.steps)) as i32
            }
        });
        high_aligned_color(value)
    }
}

fn plot(vram: &mut [u16], write: &WriteState, x: i32, y: i32, color: u16) {
    if (0..1024).contains(&x) && (0..512).contains(&y) {
        color_scalar(&mut vram[(y * 1024 + x) as usize], color, write);
    }
}

/// One segment with already offset endpoints.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_line(
    vram: &mut [u16],
    area: Area,
    write: &WriteState,
    [mut x0, mut y0, mut x1, mut y1]: [i32; 4],
    rgb0: u32,
    rgb1: u32,
    gouraud: bool,
) {
    if (x0 > area.right && x1 > area.right)
        || (y0 > area.bottom && y1 > area.bottom)
        || (x0 < area.left && x1 < area.left)
        || (y0 < area.top && y1 < area.top)
        || area.top >= area.bottom
        || area.left >= area.right
    {
        return;
    }
    let inside =
        |x: i32, y: i32| x >= area.left && x < area.right && y >= area.top && y < area.bottom;
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx == 0 && dy == 0 {
        if inside(x0, y0) {
            plot(vram, write, x0, y0, command_color(rgb0));
        }
        return;
    }
    if dx == 0 || dy == 0 {
        let vertical = dx == 0;
        let (constant, from, to) = if vertical {
            (x0, y0.min(y1), y0.max(y1))
        } else {
            (y0, x0.min(x1), x0.max(x1))
        };
        let colors = Colors::new(gouraud, rgb0, rgb1, to - from);
        let (low, high) = if vertical {
            (area.top, area.bottom)
        } else {
            (area.left, area.right)
        };
        for value in from.max(low)..=to.min(high) {
            let color = colors.at(value - from);
            if vertical {
                plot(vram, write, constant, value, color);
            } else {
                plot(vram, write, value, constant, color);
            }
        }
        return;
    }
    if dx < 0 {
        std::mem::swap(&mut x0, &mut x1);
        std::mem::swap(&mut y0, &mut y1);
    }
    let (adx, ady) = ((x1 - x0).abs(), (y1 - y0).abs());
    let sy = (y1 - y0).signum();
    let x_major = adx >= ady;
    let steps = if x_major { adx } else { ady };
    let colors = Colors::new(gouraud, rgb0, rgb1, steps);
    let (mut x, mut y) = (x0, y0);
    let mut error = if x_major {
        3 * ady - adx
    } else {
        2 * adx - ady
    };
    for step in 0..=steps {
        if inside(x, y) {
            plot(vram, write, x, y, colors.at(step));
        }
        if x_major {
            if error > 0 {
                y += sy;
                error += 2 * (ady - adx);
            } else {
                error += 2 * ady;
            }
            x += 1;
        } else {
            if error > 0 {
                x += 1;
                error += 2 * (adx - ady);
            } else {
                error += 2 * adx;
            }
            y += sy;
        }
    }
}
