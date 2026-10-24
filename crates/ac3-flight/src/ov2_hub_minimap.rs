//! The hub's minimap: the reveal frame (800D3908 with its tile builder
//! 800CF404), the 32x32 reveal-bit propagation (800D3C50) and the cursor
//! and mission markers (800D3DDC). Rows live in the hub state
//! ([`super::HUB`]): +0x18 pending bits, +0x98 visible bits, one word per
//! row; +0x118/+0x11A cursor cell, +0x11C exit offset (added to x).
use super::{CONTEXT, HUB, add_prim, call, mulhi};
use crate::machine::{Machine, SmallData};

/// 800F5B84: one word of mission-marker bits per minimap row.
pub const MARKER_ROWS: u32 = 0x800f_5b84;
/// 800AAC38: SetDrawMode (resident libgpu, owned by `sdk`); its fifth
/// argument (the texture window) is a stack argument.
pub const SET_DRAW_MODE: u32 = 0x800a_ac38;

/// 800CF404: fill the `TILE` (0x10 bytes) at `prim` with position
/// (`x`, `y`), size (`stack[0]`, `stack[1]`) and colour (`stack[2..5]`),
/// link it to `slot`, and return the next primitive. The original takes
/// the last five arguments on the stack (`[sp+0x10..0x20]`).
pub fn tile_cf404(m: &mut Machine, prim: u32, slot: u32, x: u32, y: u32, stack: [u32; 5]) -> u32 {
    let [width, height, red, green, blue] = stack;
    m.sb(prim + 3, 3);
    m.sh(prim + 8, x as u16);
    m.sh(prim + 0xc, width as u16);
    let tag = m.lw(prim);
    m.sb(prim + 7, 0x60);
    m.sh(prim + 0xa, y as u16);
    m.sh(prim + 0xe, height as u16);
    m.sb(prim + 4, red as u8);
    m.sb(prim + 5, green as u8);
    m.sb(prim + 6, blue as u8);
    let link = m.lw(slot);
    m.sw(prim, (tag & super::LENGTH) | (link & super::LINK));
    let link = m.lw(slot);
    m.sw(slot, (link & super::LENGTH) | (prim & super::LINK));
    prim.wrapping_add(0x10)
}

/// Signed `x / 17` as compiled (multiply by 0x78787879, shift 3).
fn div17(x: i32) -> i32 {
    (mulhi(x, 0x7878_7879) >> 3).wrapping_sub(x >> 31)
}

/// Signed `x / 20` as compiled (multiply by 0x66666667, shift 3).
fn div20(x: i32) -> i32 {
    (mulhi(x, 0x6666_6667) >> 3).wrapping_sub(x >> 31)
}

/// 800D3908: draw the minimap frame for reveal timer `reveal` into `slot`
/// (outer bucket 14): 17 horizontal lines (0x102 + 9k), a vertical bar, 16
/// vertical lines (x + 5k) and a closing bar, all grey (0x80). While
/// `reveal < 0x78` the lines grow with the timer. Returns the next
/// primitive; nothing is drawn for `reveal <= 0`.
pub fn grid_d3908(m: &mut Machine, prim: u32, slot: u32, reveal: u32) -> u32 {
    m.push_frame(0x48);
    let state = m.lw(HUB);
    let offset = m.lw(state + 0x11c);
    let reveal = reveal as i32;
    let mut prim = prim;
    let grey = |a: u32, b: u32| [a, b, 0x80, 0x80, 0x80];
    if reveal > 0 {
        let left = offset.wrapping_add(0xa8);
        let right = offset.wrapping_add(0xe8);
        if reveal < 0x78 {
            let mut y = 0x102u32;
            for line in 0..0x11 {
                if reveal < line {
                    break;
                }
                let length = (reveal - line).wrapping_mul(7).min(0x8c);
                prim = tile_cf404(m, prim, slot, left, y, grey(length as u32, 2));
                y = y.wrapping_add(9);
            }
            let height = div17(reveal.wrapping_mul(75)).min(0x4b);
            prim = tile_cf404(m, prim, slot, right, 0xa0, grey(height as u32, 2));
            let mut x = 0u32;
            for line in 0..0x10 {
                if reveal < line {
                    break;
                }
                let length = div20((reveal - line).wrapping_mul(242)).min(0xf2);
                prim = tile_cf404(
                    m,
                    prim,
                    slot,
                    x.wrapping_add(right),
                    0xa0,
                    grey(1, length as u32),
                );
                x = x.wrapping_add(5);
            }
            let length = div17(reveal.wrapping_mul(144)).min(0x90);
            prim = tile_cf404(m, prim, slot, left, 0x102, grey(1, length as u32));
        } else {
            let mut y = 0x102u32;
            for _ in 0..0x11 {
                prim = tile_cf404(m, prim, slot, left, y, grey(0x8c, 2));
                y = y.wrapping_add(9);
            }
            let mut x = right;
            prim = tile_cf404(m, prim, slot, x, 0xa0, grey(0x4b, 2));
            for _ in 0..0x10 {
                prim = tile_cf404(m, prim, slot, x, 0xa0, grey(1, 0xf2));
                x = x.wrapping_add(5);
            }
            prim = tile_cf404(m, prim, slot, left, 0x102, grey(1, 0x90));
        }
    }
    m.pop_frame(0x48);
    prim
}

/// 800D3C50: spread the visible bits (+0x98) of every row to the pending
/// bits (+0x18) of its neighbours in a hexagonal pattern (odd rows reach
/// the same column below and three columns above, even rows the reverse),
/// then make the pending bits visible and clear them. Row 0's "row above"
/// is the reveal timer word at +0x14 and row 31's "row below" is visible
/// row 0, both read and written back unchanged.
pub fn propagate_d3c50(m: &mut Machine) {
    let state = m.lw(HUB);
    let bit = |shift: u32| 1u32 << (shift & 31);
    for row in 0..0x20u32 {
        let above_at = state + 0x14 + row * 4;
        let this_at = state + 0x18 + row * 4;
        let below_at = state + 0x1c + row * 4;
        let has_below = row < 0x1f;
        let visible = m.lw(state + 0x98 + row * 4);
        let mut above = m.lw(above_at);
        let mut this = m.lw(this_at);
        let mut below = m.lw(below_at);
        for column in 0..0x20u32 {
            let own = bit(column);
            if visible & own == 0 {
                continue;
            }
            let left = bit(column.wrapping_sub(1));
            let right = bit(column + 1);
            if column > 0 {
                this |= left;
            }
            if column < 0x1f {
                this |= right;
            }
            let three = || match column {
                0 => 3,
                0x1f => 0xc000_0000,
                _ => left | own | right,
            };
            if row & 1 != 0 {
                if has_below {
                    below |= own;
                }
                if row > 0 {
                    above |= three();
                }
            } else {
                if has_below {
                    below |= three();
                }
                if row > 0 {
                    above |= own;
                }
            }
        }
        m.sw(above_at, above);
        m.sw(this_at, this);
        m.sw(below_at, below);
    }
    let state = m.lw(HUB);
    for row in 0..0x20u32 {
        let visible = m.lw(state + 0x98 + row * 4);
        let pending = m.lw(state + 0x18 + row * 4);
        m.sw(state + 0x98 + row * 4, visible | pending);
        m.sw(state + 0x18 + row * 4, 0);
    }
}

/// `x % 2` (signed, truncated) as compiled.
fn parity(x: i32) -> i32 {
    x.wrapping_sub(((x.wrapping_add((x as u32 >> 31) as i32)) >> 1) << 1)
}

/// 800D3DDC: draw the pulsing cursor sprite (`SPRT`, 0x14 bytes) at the
/// cursor cell, a draw-mode packet, a 8x12 marker sprite for every cell
/// that is both visible and a mission marker (fading out for reveal
/// timers 31..60, black after), and a closing draw-mode packet, all into
/// `slot`. Returns the next primitive.
pub fn markers_d3ddc(m: &mut Machine, prim: u32, slot: u32, reveal: u32) -> u32 {
    let frame = m.push_frame(0x40);
    let state = m.lw(HUB);
    let cursor = prim;
    let offset = m.lw(state + 0x11c);
    m.sb(cursor + 3, 4);
    m.sb(cursor + 7, 0x64);
    let state = m.lw(HUB);
    let column = m.lh(state + 0x118) as i32;
    let left = offset.wrapping_add(0x96);
    m.sh(
        cursor + 8,
        (column.wrapping_mul(5) as u32).wrapping_add(left) as u16,
    );
    let row = m.lh(state + 0x11a) as i32;
    let column = m.lh(state + 0x118) as i32;
    m.sh(cursor + 0x10, 8);
    m.sh(cursor + 0x12, 0xc);
    m.sb(cursor + 0xc, 0xf8);
    m.sb(cursor + 0xd, 0xc);
    let y = row.wrapping_mul(10).wrapping_add(
        parity(column.wrapping_add(1))
            .wrapping_mul(5)
            .wrapping_add(0xbe),
    );
    let ctx = m.gw(CONTEXT);
    m.sh(cursor + 0xa, y as u16);
    let clock = m.lw(ctx + 0x20) as i32;
    let phase = clock.wrapping_sub(((if clock < 0 { clock + 0x7f } else { clock }) >> 7) << 7);
    let level = (0x40 - phase).wrapping_abs().wrapping_shl(1) as u8;
    let mode = cursor + 0x14;
    let mut next = cursor + 0x20;
    m.sb(cursor + 4, level);
    m.sb(cursor + 5, level);
    m.sb(cursor + 6, level);
    let tag = m.lw(cursor);
    m.sh(cursor + 0xe, 0x298);
    let link = m.lw(slot);
    m.sw(cursor, (tag & super::LENGTH) | (link & super::LINK));
    let link = m.lw(slot);
    m.sw(slot, (link & super::LENGTH) | (cursor & super::LINK));
    m.sw(frame + 0x10, 0);
    call(m, SET_DRAW_MODE, [mode, 0, 0, 7]);
    let fade = (reveal as i32).wrapping_sub(0x1e).wrapping_shl(7);
    add_prim(m, slot, mode);
    let mut x = left;
    for row in 0..0x20u32 {
        let y0 = parity(row as i32 + 1).wrapping_mul(5).wrapping_add(0xbe);
        let state = m.lw(HUB);
        let markers = m.lw(MARKER_ROWS + row * 4);
        let visible = m.lw(state + row * 4 + 0x98);
        let mut y = y0;
        for column in 0..0x20u32 {
            let bit = 1u32 << column;
            if markers & bit != 0 && visible & bit != 0 {
                let p = next;
                next = next.wrapping_add(0x14);
                m.sb(p + 3, 4);
                m.sb(p + 7, 0x66);
                m.sh(p + 0x10, 8);
                m.sh(p + 0x12, 0xc);
                m.sb(p + 0xc, 0xf8);
                m.sh(p + 8, x as u16);
                m.sh(p + 0xa, y as u16);
                m.sb(p + 0xd, 0);
                if (reveal as i32) >= 0x3d {
                    m.sb(p + 4, 0);
                    m.sb(p + 5, 0);
                    m.sb(p + 6, 0);
                } else {
                    let level = if (reveal as i32) >= 0x1f {
                        let quotient = (mulhi(fade, 0x8888_8889).wrapping_add(fade) >> 4)
                            .wrapping_sub(fade >> 31);
                        0x80i32.wrapping_sub(quotient)
                    } else {
                        0x80
                    } as u8;
                    m.sb(p + 4, level);
                    m.sb(p + 5, level);
                    m.sb(p + 6, level);
                }
                m.sh(p + 0xe, 0x258);
                add_prim(m, slot, p);
            }
            y = y.wrapping_add(0xa);
        }
        x = x.wrapping_add(5);
    }
    let mode = next;
    next = next.wrapping_add(0xc);
    m.sw(frame + 0x10, 0);
    call(m, SET_DRAW_MODE, [mode, 0, 0, 0x47]);
    add_prim(m, slot, mode);
    m.pop_frame(0x40);
    next
}
