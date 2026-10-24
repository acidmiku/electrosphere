//! JP overlay 2, part 2: the hub's gray primitives and its 32 x 32 reveal
//! grid, translated from the JP instructions.
//!
//! The grid lives in the work block: +0x70 + 4 * row pending bits,
//! +0xF0 + 4 * row visible bits (one bit per column), +0x170 the reveal
//! timer, +0x172 the reveal mode (1 spreading, 2 done, 3 fading), +0x174 /
//! +0x176 the cursor cell. [`start_reveal`] seeds it, [`step_reveal`]
//! times it, [`spread_reveal`] grows it and [`draw_grid`] draws the cursor
//! and every visible mission cell (mask table 0x8010AD2C) with the quads
//! of [`init_quads`](super::ov2_p2::init_quads).
use super::ov2_p2::{MODULE, framed, lh, mul, visual, work};
use crate::common::{CONTEXT, loaded_module};
use crate::machine::{Machine, SmallData};
use crate::{dispatch, sdk};

/// Start cells (x, y halfwords) by the context selector +0x38C.
pub const START_CELLS: u32 = 0x8010_adac;
/// Mission cell masks, one word per grid row.
pub const MISSION_CELLS: u32 = 0x8010_ad2c;
/// 8010097C (overlay 2, another part): the routine [`step_reveal`] calls
/// when the spreading is done.
const REVEAL_DONE: u32 = 0x8010_097c;

/// `addPrim` of `prim` into the slot `slot`, both upper bytes kept.
/// Returns the slot's new word.
fn add_prim(m: &mut Machine, prim: u32, slot: u32) -> u32 {
    let tag = m.lw(prim);
    let head = m.lw(slot);
    m.sw(prim, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let head = m.lw(slot);
    let word = (head & 0xff00_0000) | (prim & 0x00ff_ffff);
    m.sw(slot, word);
    word
}

/// The ordering-table slot of the current animation record (+0xE) in
/// `ot`, as the original re-reads it before each access.
fn record_slot(m: &mut Machine, block: u32, ot: u32) -> u32 {
    let record = m.lw(block.wrapping_add(0x3c));
    (lh(m, record.wrapping_add(0xe)) << 2).wrapping_add(ot)
}

/// `addPrim` into the current record's slot, re-reading the slot for the
/// second access as the original does.
fn link(m: &mut Machine, block: u32, ot: u32, prim: u32) -> u32 {
    let slot = record_slot(m, block, ot);
    let tag = m.lw(prim);
    let head = m.lw(slot);
    m.sw(prim, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let slot = record_slot(m, block, ot);
    let head = m.lw(slot);
    let word = (head & 0xff00_0000) | (prim & 0x00ff_ffff);
    m.sw(slot, word);
    word
}

/// 800E0F88: a gray (0x60) line from (`x0`, `y0`) to (`x1`, `y1`)
/// (LINE_F2 from the visual block's packet pointer +0x274) into the
/// current record's slot of ordering table [visual + 8] + 0x24. Returns
/// the slot's new word.
pub fn gray_line(m: &mut Machine, x0: u32, y0: u32, x1: u32, y1: u32) -> u32 {
    let block = visual(m);
    let prim = m.lw(block.wrapping_add(0x274));
    let ot = m.lw(block.wrapping_add(8));
    m.sw(block.wrapping_add(0x274), prim.wrapping_add(0x10));
    m.sb(prim.wrapping_add(3), 3);
    m.sb(prim.wrapping_add(7), 0x40);
    for at in 4..7 {
        m.sb(prim.wrapping_add(at), 0x60);
    }
    let work = work(m);
    let ot = ot.wrapping_add(0x20);
    for (at, value) in [(8, x0), (0xa, y0), (0xc, x1), (0xe, y1)] {
        m.sh(prim.wrapping_add(at), value as u16);
    }
    link(m, work, ot.wrapping_add(4), prim)
}

/// 800E1040: a gray (0x60) rectangle at (`x`, `y`) of `w` x `h` (TILE;
/// semi-transparent when the fifth argument's low byte is nonzero), as
/// [`gray_line`]. Returns the slot's new word.
pub fn gray_tile(m: &mut Machine, x: u32, y: u32, w: u32, h: u32, translucent: u32) -> u32 {
    let block = visual(m);
    let prim = m.lw(block.wrapping_add(0x274));
    m.sw(block.wrapping_add(0x274), prim.wrapping_add(0x10));
    let ot = m.lw(block.wrapping_add(8));
    m.sb(prim.wrapping_add(3), 3);
    m.sb(prim.wrapping_add(7), 0x60);
    let ot = ot.wrapping_add(0x20);
    let code = if translucent & 0xff != 0 { 0x62 } else { 0x60 };
    m.sb(prim.wrapping_add(7), code);
    for at in 4..7 {
        m.sb(prim.wrapping_add(at), 0x60);
    }
    let work = work(m);
    for (at, value) in [(8, x), (0xa, y), (0xc, w), (0xe, h)] {
        m.sh(prim.wrapping_add(at), value as u16);
    }
    link(m, work, ot.wrapping_add(4), prim)
}

/// 800E1118: a draw-mode primitive (SetDrawMode with texture page
/// `(abr & 3) << 5`, no dithering, no texture window) from the work
/// block's packet pointer into the current record's slot of [visual + 8].
/// Returns the slot's new word.
pub fn draw_mode_abr(m: &mut Machine, abr: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        let page = (abr & 3) << 5;
        let block = work(m);
        let vis = visual(m);
        let prim = m.lw(block.wrapping_add(4));
        let ot = m.lw(vis.wrapping_add(8));
        m.sw(block.wrapping_add(4), prim.wrapping_add(0xc));
        m.sw(sp + 0x10, 0);
        sdk::set_draw_mode(m, prim, 0, 0, page, 0);
        let block = work(m);
        link(m, block, ot, prim)
    })
}

/// 800E11F4: start the reveal: mode `mode` and timer `timer`, the start
/// cell of context selector +0x38C ([`START_CELLS`]) made visible and the
/// cursor put on it. Returns the visible row's new word.
pub fn start_reveal(m: &mut Machine, mode: u32, timer: u32) -> u32 {
    let block = work(m);
    let context = m.gw(CONTEXT);
    m.sh(block.wrapping_add(0x172), mode as u16);
    m.sh(block.wrapping_add(0x170), timer as u16);
    let selector = lh(m, context.wrapping_add(0x38c));
    let cell = (selector << 2).wrapping_add(START_CELLS);
    let x = lh(m, cell);
    let y = lh(m, cell.wrapping_add(2));
    let x_raw = m.lhu(cell);
    let row = (x << 2).wrapping_add(block);
    let bit = 1u32 << (y & 31);
    let visible = m.lw(row.wrapping_add(0xf0)) | bit;
    let y_raw = m.lhu(cell.wrapping_add(2));
    m.sw(row.wrapping_add(0xf0), visible);
    m.sh(block.wrapping_add(0x174), x_raw);
    m.sh(block.wrapping_add(0x176), y_raw);
    visible
}

/// 800E1258: time the reveal: while spreading (mode 1) the timer counts up
/// to 0x78, then the mode becomes 2 and 8010097C runs; while fading (mode
/// 3) it counts down to 0, then the mode becomes 0. Flag byte 2 = timer >=
/// 0x1F. Returns 0 when off or done, else the flag outcome (1, or the flag
/// table pointer).
pub fn step_reveal(m: &mut Machine, a2: u32, a3: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = work(m);
        let mode = lh(m, block.wrapping_add(0x172));
        if mode == 0 || mode == 2 {
            return 0;
        }
        if mode == 1 {
            let timer = lh(m, block.wrapping_add(0x170));
            let raw = m.lhu(block.wrapping_add(0x170));
            if (timer as i32) < 0x78 {
                m.sh(block.wrapping_add(0x170), raw.wrapping_add(1));
            } else {
                m.sh(block.wrapping_add(0x172), 2);
                dispatch::call(m, REVEAL_DONE, [block, 2, a2, a3]);
            }
        }
        let block = work(m);
        if lh(m, block.wrapping_add(0x172)) == 3 {
            let timer = lh(m, block.wrapping_add(0x170));
            let raw = m.lhu(block.wrapping_add(0x170));
            if (timer as i32) > 0 {
                m.sh(block.wrapping_add(0x170), raw.wrapping_sub(1));
            } else {
                m.sh(block.wrapping_add(0x172), 0);
            }
        }
        let block = work(m);
        let timer = lh(m, block.wrapping_add(0x170));
        let flags = m.lw(block.wrapping_add(0x5c));
        if (timer as i32) < 0x1f {
            m.sb(flags.wrapping_add(2), 0);
            flags
        } else {
            m.sb(flags.wrapping_add(2), 1);
            1
        }
    })
}

/// 800E1334 (JP; PAL 800D3C50, near): while spreading (mode 1), spread
/// the visible bits of every row to the pending bits of its neighbours in
/// a hexagonal pattern (odd rows reach the same column below and three
/// columns above, even rows the reverse), then make the pending bits
/// visible and clear them. Row 0's "row above" is the word at +0x6C and
/// row 31's "row below" is visible row 0, read and written back. Returns
/// 0.
pub fn spread_reveal(m: &mut Machine) -> u32 {
    let block = work(m);
    if lh(m, block.wrapping_add(0x172)) != 1 {
        return 0;
    }
    let bit = |shift: u32| 1u32 << (shift & 31);
    for row in 0..0x20u32 {
        let this_at = block.wrapping_add(0x70 + 4 * row);
        let below_at = block.wrapping_add(0x74 + 4 * row);
        let above_at = below_at.wrapping_sub(8);
        let has_below = row < 0x1f;
        let visible = m.lw(block.wrapping_add(0xf0 + 4 * row));
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
            let three = match column {
                0 => 3,
                0x1f => 0xc000_0000,
                _ => left | own | right,
            };
            if row & 1 != 0 {
                if has_below {
                    below |= own;
                }
                if row > 0 {
                    above |= three;
                }
            } else {
                if has_below {
                    below |= three;
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
    let block = work(m);
    for row in 0..0x20u32 {
        let visible_at = block.wrapping_add(0xf0 + 4 * row);
        let pending_at = block.wrapping_add(0x70 + 4 * row);
        let visible = m.lw(visible_at);
        let pending = m.lw(pending_at);
        m.sw(visible_at, visible | pending);
        m.sw(pending_at, 0);
    }
    0
}

/// `x % 2` (signed, truncated) as compiled.
fn parity(x: u32) -> u32 {
    x.wrapping_sub((x.wrapping_add(x >> 31) as i32 >> 1 << 1) as u32)
}

/// 800E14D0: draw the grid of the current buffer (context +4): the
/// cursor quad (+0x56C8 + 0x28 * buffer) at the cursor cell, pulsing
/// (|0x40 - frame % 0x80| * 2, context +0x20), then a quad (from +0x178 +
/// 0x2AA8 * buffer) for every cell that is both visible and a mission
/// cell, fading out once the timer passes 0x1E; all into the current
/// record's slot of [visual + 8]. Returns 0.
pub fn draw_grid(m: &mut Machine) -> u32 {
    framed(m, 0x10, |m, _| {
        let context = m.gw(CONTEXT);
        let buffer = m.lw(context.wrapping_add(4));
        let block = work(m);
        let quad = block.wrapping_add(mul(buffer, 0x28).wrapping_add(0x56c8));
        for (at_x, at_y, dx, dy) in [
            (8, 0xa, 0x96, 0xbe),
            (0x10, 0x12, 0xa2, 0xbe),
            (0x18, 0x1a, 0x96, 0xd2),
            (0x20, 0x22, 0xa2, 0xd2),
        ] {
            let x = lh(m, block.wrapping_add(0x174));
            m.sh(quad.wrapping_add(at_x), mul(x, 5).wrapping_add(dx) as u16);
            let y = lh(m, block.wrapping_add(0x176));
            let x = lh(m, block.wrapping_add(0x174));
            let odd = mul(parity(x.wrapping_add(1)), 5);
            let value = mul(y, 10).wrapping_add(odd).wrapping_add(dy);
            m.sh(quad.wrapping_add(at_y), value as u16);
        }
        let frame = m.lw(context.wrapping_add(0x20)) as i32;
        let biased = if frame < 0 {
            frame.wrapping_add(0x7f)
        } else {
            frame
        };
        let phase = frame.wrapping_sub((biased >> 7) << 7);
        let level = 0x40i32.wrapping_sub(phase);
        let level = (if level < 0 {
            level.wrapping_neg()
        } else {
            level
        } as u32)
            << 1;
        for at in 4..7 {
            m.sb(quad.wrapping_add(at), level as u8);
        }
        let block = work(m);
        let vis = visual(m);
        let ot = m.lw(vis.wrapping_add(8));
        let slot = record_slot(m, block, ot);
        let tag = m.lw(quad);
        let head = m.lw(slot);
        m.sw(quad, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
        let ot = m.lw(vis.wrapping_add(8));
        let slot = record_slot(m, block, ot);
        let head = m.lw(slot);
        m.sw(slot, (head & 0xff00_0000) | (quad & 0x00ff_ffff));
        let context = m.gw(CONTEXT);
        let buffer = m.lw(context.wrapping_add(4));
        let timer = lh(m, block.wrapping_add(0x170));
        let mut quad = block.wrapping_add(mul(buffer, 0x2aa8).wrapping_add(0x178));
        let fade = timer.wrapping_sub(0x1e) << 7;
        let sign = ((fade as i32) >> 31) as u32;
        let ot = m.lw(vis.wrapping_add(8));
        let slot = record_slot(m, block, ot);
        let (mut left, mut right) = (0x96u32, 0x9eu32);
        for row in 0..0x20u32 {
            let mut top = mul(parity(row + 1), 5).wrapping_add(0xbe);
            let mut corner = quad.wrapping_add(6);
            let block = work(m);
            let missions = m.lw(MISSION_CELLS + 4 * row);
            let visible = m.lw(block.wrapping_add(4 * row).wrapping_add(0xf0));
            for column in 0..0x20u32 {
                let own = 1u32 << column;
                if missions & own != 0 && visible & own != 0 {
                    let bottom = top.wrapping_add(0xc);
                    m.sh(corner.wrapping_add(0x14), bottom as u16);
                    m.sh(corner.wrapping_add(0x1c), bottom as u16);
                    for (at, value) in [
                        (2, left),
                        (4, top),
                        (0xa, right),
                        (0xc, top),
                        (0x12, left),
                        (0x1a, right),
                    ] {
                        m.sh(corner.wrapping_add(at), value as u16);
                    }
                    if (timer as i32) >= 0x1f {
                        let high = ((i64::from(fade as i32) * i64::from(0xb60b_60b7_u32 as i32))
                            >> 32) as u32;
                        let tenth = (high.wrapping_add(fade) as i32 >> 6) as u32;
                        let level = 0x80u32.wrapping_sub(tenth.wrapping_sub(sign));
                        for at in [-2i32, -1, 0] {
                            m.sb(corner.wrapping_add(at as u32), level as u8);
                        }
                    }
                    corner = corner.wrapping_add(0x28);
                    add_prim(m, quad, slot);
                    quad = quad.wrapping_add(0x28);
                }
                top = top.wrapping_add(0xa);
            }
            right = right.wrapping_add(5);
            left = left.wrapping_add(5);
        }
        0
    })
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800e_0f88 => gray_line(m, a0, a1, a2, a3),
        0x800e_1040 => {
            let sp = m.sp;
            let translucent = u32::from(m.lbu(sp.wrapping_add(0x10)));
            gray_tile(m, a0, a1, a2, a3, translucent)
        }
        0x800e_1118 => draw_mode_abr(m, a0),
        0x800e_11f4 => start_reveal(m, a0, a1),
        0x800e_1258 => step_reveal(m, a2, a3),
        0x800e_1334 => spread_reveal(m),
        0x800e_14d0 => draw_grid(m),
        _ => return None,
    })
}
