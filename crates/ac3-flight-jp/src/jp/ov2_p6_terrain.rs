//! JP overlay 2: the story scene's terrain (a 17x17 grid of gouraud quads
//! over the 4-bit height map `[0x80112DC8]`, 258 cells a row) and its
//! overlays, translated from the JP instructions.
//!
//! The grid work record (in the scratchpad at 0x1F800000 for 801048F0;
//! any record for 80104218): +0/+2 the cell x/z, +4/+6 the fractions of
//! the looked-at point within its cell (0..0x7F), +8..+0xE the four corner
//! heights, +0x10..+0x1C the four corner colors (the scene's 16 colors
//! `+0x894` by height / 128), +0x20..+0x38 the four corner SVECTORs
//! (x * 128, -height, z * 128). The outermost rows and columns are blended
//! by the fractions so the grid scrolls smoothly.
use super::ov2_p6::{HUB, SCENE, add_prim_masked, call, divide_pow2, framed, half, lwc2, swc2};
use crate::machine::Machine;

/// `[0x80112DC8]`: the height map (two 4-bit heights a byte).
pub const HEIGHTS: u32 = 0x8011_2dc8;
/// 800A8F20: SetDrawArea.
const SET_DRAW_AREA: u32 = 0x800a_8f20;
/// 800A2098: VSync.
const VSYNC: u32 = 0x800a_2098;
/// 8002DB4C: start a sound.
const START_SOUND: u32 = 0x8002_db4c;
/// The terrain's drawing area (a RECT in the overlay).
const AREA: u32 = 0x800c_d774;

fn sext16(value: u32) -> i32 {
    i32::from(value as u16 as i16)
}
fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}

/// 8010417C (not reached): the height (nibble * 128) of the map cell at
/// (`x`, `z`) or one of its neighbours (`corner` 1: x + 1, 2: z + 1, 3:
/// both).
pub fn height_at(m: &mut Machine, x: u32, z: u32, corner: u32) -> u32 {
    let corner = sext16(corner);
    let mut column = x;
    let mut row = z;
    if corner == 1 {
        column = x.wrapping_add(1);
    }
    if corner == 2 {
        row = z.wrapping_add(1);
    }
    if corner == 3 {
        column = column.wrapping_add(1);
        row = row.wrapping_add(1);
    }
    let row = sext16(row).wrapping_add(0x80);
    let index = sext16(column)
        .wrapping_add(row.wrapping_mul(258))
        .wrapping_add(0x80);
    let index = ((index.wrapping_add(((index as u32) >> 31) as i32) as u32) << 15) as i32 >> 16;
    let base = m.lw(HEIGHTS);
    let byte = u32::from(m.lbu(base.wrapping_add(index as u32)));
    let shift = (column & 1) << 2;
    ((byte >> shift) & 0xf) << 7
}

/// One edge blend of a color word: each channel moves from the word at
/// `a` toward the word at `b` by `factor` / 128 (unclamped, as compiled);
/// the fourth byte is `a`'s. Stored at `a`.
fn blend(m: &mut Machine, record: u32, a: u32, b: u32, factor: i32) {
    let mut word = 0u32;
    for channel in 0..3u32 {
        let from = i32::from(m.lbu(record.wrapping_add(a + channel)));
        let to = i32::from(m.lbu(record.wrapping_add(b + channel)));
        let value = from.wrapping_add(divide_pow2(to.wrapping_sub(from).wrapping_mul(factor), 7));
        word |= (value as u32) << (channel * 8);
    }
    let top = u32::from(m.lbu(record.wrapping_add(a + 3)));
    m.sw(record.wrapping_add(a), word | (top << 24));
}

/// The four edge blends of a quad's colors in the original order:
/// first column (`column` -8) by the x fraction, first row (`row` -8) by
/// the z fraction, last column (8) and last row (8) by the fraction - 0x80.
fn blend_edges(m: &mut Machine, record: u32, column: i32, row: i32) {
    if column == -8 {
        let f = sext16(lhu(m, record.wrapping_add(4)));
        blend(m, record, 0x10, 0x14, f);
        let f = sext16(lhu(m, record.wrapping_add(4)));
        blend(m, record, 0x18, 0x1c, f);
    }
    if row == -8 {
        let f = sext16(lhu(m, record.wrapping_add(6)));
        blend(m, record, 0x10, 0x18, f);
        let f = sext16(lhu(m, record.wrapping_add(6)));
        blend(m, record, 0x14, 0x1c, f);
    }
    if column == 8 {
        let f = sext16(lhu(m, record.wrapping_add(4))).wrapping_sub(0x80);
        blend(m, record, 0x14, 0x10, f);
        let f = sext16(lhu(m, record.wrapping_add(4))).wrapping_sub(0x80);
        blend(m, record, 0x1c, 0x18, f);
    }
    if row == 8 {
        let f = sext16(lhu(m, record.wrapping_add(6))).wrapping_sub(0x80);
        blend(m, record, 0x18, 0x10, f);
        let f = sext16(lhu(m, record.wrapping_add(6))).wrapping_sub(0x80);
        blend(m, record, 0x1c, 0x14, f);
    }
}

/// The corner colors (+0x10..+0x1C) from the heights (+8..+0xE): the
/// scene's color `height / 128`.
fn corner_colors(m: &mut Machine, record: u32) {
    let block = m.lw(SCENE);
    for corner in 0..4u32 {
        let height = sext16(lhu(m, record.wrapping_add(8 + corner * 2)));
        let index = divide_pow2(height, 7) as u32;
        let color = m.lw((index << 2).wrapping_add(block).wrapping_add(0x894));
        m.sw(record.wrapping_add(0x10 + corner * 4), color);
    }
}

/// The depth words after a projection: IR0, FLAG, SZ3 / 4 at `at`.
fn depth_words(m: &mut Machine, at: u32) {
    swc2(m, 8, at);
    let flag = m.gte.cfc2(31);
    m.sw(at + 4, flag);
    let sz = m.gte.mfc2(19) as i32 >> 2;
    m.sw(at + 8, sz as u32);
}

/// 80104218 (not reached): one terrain quad of `record` into the POLY_G4
/// `prim`: the first corner projected (RTPS) in the first column (`column`
/// -8), else the previous quad's second corner (SXY0); the other three by
/// RTPT; colors by height with the edge blends (`column`, `row`); the
/// prim's colors keep its code byte. Returns the fourth color.
pub fn shade_quad(m: &mut Machine, record: u32, prim: u32, column: u32, row: u32) -> u32 {
    framed(m, 0x10, |m, sp| {
        if sext16(column) == -8 {
            lwc2(m, 0, record.wrapping_add(0x20));
            lwc2(m, 1, record.wrapping_add(0x24));
            m.gte.rtps(true, false);
            swc2(m, 14, prim.wrapping_add(8));
            depth_words(m, sp);
        } else {
            swc2(m, 12, prim.wrapping_add(8));
        }
        project_three(m, record, prim);
        let sz = m.gte.mfc2(19) as i32 >> 2;
        m.sw(sp + 8, sz as u32);
        corner_colors(m, record);
        blend_edges(m, record, sext16(column), sext16(row));
        let code = u32::from(m.lbu(prim.wrapping_add(7)));
        let first = m.lw(record.wrapping_add(0x10));
        m.sw(prim.wrapping_add(4), first | (code << 24));
        let second = m.lw(record.wrapping_add(0x14));
        m.sw(prim.wrapping_add(0xc), second);
        let third = m.lw(record.wrapping_add(0x18));
        m.sw(prim.wrapping_add(0x14), third);
        let fourth = m.lw(record.wrapping_add(0x1c));
        m.sw(prim.wrapping_add(0x1c), fourth);
        fourth
    })
}

/// RTPT of the record's corners 1..3 (+0x28, +0x30, +0x38) into the
/// prim's XY words +0x10, +0x18, +0x20.
fn project_three(m: &mut Machine, record: u32, prim: u32) {
    for (reg, offset) in [(0, 0x28u32), (2, 0x30), (4, 0x38)] {
        lwc2(m, reg, record.wrapping_add(offset));
        lwc2(m, reg + 1, record.wrapping_add(offset + 4));
    }
    m.gte.rtpt(true, false);
    swc2(m, 12, prim.wrapping_add(0x10));
    swc2(m, 13, prim.wrapping_add(0x18));
    swc2(m, 14, prim.wrapping_add(0x20));
}

/// The height-map nibble (* 128) of cell (`x` + `dx`, `z` + `dz`) as
/// 801048F0 indexes it (`(x + dx + (z + 0x80 + dz) * 258 + 0x80) / 2`,
/// the nibble of `x + dx`'s parity; `parity` is the value whose low bit
/// selects it).
fn cell_height(m: &mut Machine, base: u32, x: i32, z: i32, dx: i32, dz: i32, parity: i32) -> u32 {
    let row = z.wrapping_add(0x80).wrapping_add(dz);
    let index = half(
        x.wrapping_add(row.wrapping_mul(258))
            .wrapping_add(0x80)
            .wrapping_add(dx),
    );
    let byte = u32::from(m.lbu(base.wrapping_add(index as u32)));
    let shift = ((parity as u32) & 1) << 2;
    ((byte >> shift) & 0xf) << 7
}

/// The cell (`+0` / `+2`) and fraction (`+4` / `+6`) of a looked-at
/// coordinate on axis `axis` (0 x, 1 z), offset by `cell`.
fn locate(m: &mut Machine, record: u32, axis: u32, value: u32, cell: u32) {
    let quotient = divide_pow2(value as i32, 7) as u32;
    m.sh(
        record.wrapping_add(axis * 2),
        cell.wrapping_add(quotient) as u16,
    );
    m.sh(
        record.wrapping_add(4 + axis * 2),
        value.wrapping_sub(quotient << 7) as u16,
    );
    if (value as i32) < 0 {
        let c = lhu(m, record.wrapping_add(axis * 2));
        let f = lhu(m, record.wrapping_add(4 + axis * 2));
        m.sh(record.wrapping_add(axis * 2), c.wrapping_sub(1) as u16);
        m.sh(
            record.wrapping_add(4 + axis * 2),
            f.wrapping_add(0x80) as u16,
        );
    }
}

/// The edge correction of a quad's corners on the first and last column
/// and row (801049F4..80105070): the corner positions move by the fraction
/// and the heights interpolate toward the inner corners.
fn correct_edges(m: &mut Machine, r: u32, column: i32, row: i32) {
    // one height correction: h[at] += (h[to] - h[at]) * factor / 128 on
    // the first column and row (`at < to`); on the last ones (`at > to`)
    // the original takes (h[at] - h[to]) with the factor fraction - 0x80
    let lerp = |m: &mut Machine, at: u32, to: u32, factor: i32| {
        let from = lhu(m, r.wrapping_add(at));
        let target = sext16(lhu(m, r.wrapping_add(to)));
        let difference = if at > to {
            sext16(from).wrapping_sub(target)
        } else {
            target.wrapping_sub(sext16(from))
        };
        (from, divide_pow2(difference.wrapping_mul(factor), 7) as u32)
    };
    if column == -8 {
        let f = sext16(lhu(m, r.wrapping_add(4)));
        let (h0, d0) = lerp(m, 8, 0xa, f);
        let fx = lhu(m, r.wrapping_add(4));
        let x0 = lhu(m, r.wrapping_add(0x20));
        let x2 = lhu(m, r.wrapping_add(0x30));
        m.sh(r.wrapping_add(0x20), x0.wrapping_add(fx) as u16);
        m.sh(r.wrapping_add(0x30), x2.wrapping_add(fx) as u16);
        let f = sext16(lhu(m, r.wrapping_add(4)));
        let (h2, d2) = lerp(m, 0xc, 0xe, f);
        m.sh(r.wrapping_add(8), h0.wrapping_add(d0) as u16);
        m.sh(r.wrapping_add(0xc), h2.wrapping_add(d2) as u16);
        let h0 = lhu(m, r.wrapping_add(8));
        let h2 = lhu(m, r.wrapping_add(0xc));
        m.sh(r.wrapping_add(0x22), h0.wrapping_neg() as u16);
        m.sh(r.wrapping_add(0x32), h2.wrapping_neg() as u16);
    }
    if column == 8 {
        let f = sext16(lhu(m, r.wrapping_add(4))).wrapping_sub(0x80);
        let (h1, d1) = lerp(m, 0xa, 8, f);
        let x1 = lhu(m, r.wrapping_add(0x28)).wrapping_sub(0x80);
        let fx = lhu(m, r.wrapping_add(4));
        m.sh(r.wrapping_add(0x28), fx.wrapping_add(x1) as u16);
        let x3 = lhu(m, r.wrapping_add(0x38)).wrapping_sub(0x80);
        let fx = lhu(m, r.wrapping_add(4));
        m.sh(r.wrapping_add(0x38), fx.wrapping_add(x3) as u16);
        let f = sext16(lhu(m, r.wrapping_add(4))).wrapping_sub(0x80);
        let (h3, d3) = lerp(m, 0xe, 0xc, f);
        m.sh(r.wrapping_add(0xa), h1.wrapping_add(d1) as u16);
        m.sh(r.wrapping_add(0xe), h3.wrapping_add(d3) as u16);
        let h1 = lhu(m, r.wrapping_add(0xa));
        let h3 = lhu(m, r.wrapping_add(0xe));
        m.sh(r.wrapping_add(0x2a), h1.wrapping_neg() as u16);
        m.sh(r.wrapping_add(0x3a), h3.wrapping_neg() as u16);
    }
    if row == -8 {
        let f = sext16(lhu(m, r.wrapping_add(6)));
        let (h0, d0) = lerp(m, 8, 0xc, f);
        let fz = lhu(m, r.wrapping_add(6));
        let z0 = lhu(m, r.wrapping_add(0x24));
        let z1 = lhu(m, r.wrapping_add(0x2c));
        m.sh(r.wrapping_add(0x24), z0.wrapping_add(fz) as u16);
        m.sh(r.wrapping_add(0x2c), z1.wrapping_add(fz) as u16);
        let f = sext16(lhu(m, r.wrapping_add(6)));
        let (h1, d1) = lerp(m, 0xa, 0xe, f);
        m.sh(r.wrapping_add(8), h0.wrapping_add(d0) as u16);
        m.sh(r.wrapping_add(0xa), h1.wrapping_add(d1) as u16);
        let h0 = lhu(m, r.wrapping_add(8));
        let h1 = lhu(m, r.wrapping_add(0xa));
        m.sh(r.wrapping_add(0x22), h0.wrapping_neg() as u16);
        m.sh(r.wrapping_add(0x2a), h1.wrapping_neg() as u16);
    }
    if row == 8 {
        let f = sext16(lhu(m, r.wrapping_add(6))).wrapping_sub(0x80);
        let (h2, d2) = lerp(m, 0xc, 8, f);
        let z2 = lhu(m, r.wrapping_add(0x34)).wrapping_sub(0x80);
        let fz = lhu(m, r.wrapping_add(6));
        m.sh(r.wrapping_add(0x34), fz.wrapping_add(z2) as u16);
        let z3 = lhu(m, r.wrapping_add(0x3c)).wrapping_sub(0x80);
        let fz = lhu(m, r.wrapping_add(6));
        m.sh(r.wrapping_add(0x3c), fz.wrapping_add(z3) as u16);
        let f = sext16(lhu(m, r.wrapping_add(6))).wrapping_sub(0x80);
        let (h3, d3) = lerp(m, 0xe, 0xa, f);
        m.sh(r.wrapping_add(0xc), h2.wrapping_add(d2) as u16);
        m.sh(r.wrapping_add(0xe), h3.wrapping_add(d3) as u16);
        let h2 = lhu(m, r.wrapping_add(0xc));
        let h3 = lhu(m, r.wrapping_add(0xe));
        m.sh(r.wrapping_add(0x32), h2.wrapping_neg() as u16);
        m.sh(r.wrapping_add(0x3a), h3.wrapping_neg() as u16);
    }
}

/// The four corners of the quad at the record's cell (801049F4..80104D18):
/// in the first column corner 0 from the map, else the previous quad's
/// corner 1; corners 1..3 from the map.
fn corners(m: &mut Machine, r: u32, first: bool) {
    if first {
        let z = sext16(lhu(m, r.wrapping_add(2)));
        let x = i32::from(m.lh(r));
        let base = m.lw(HEIGHTS);
        let parity = lhu(m, r) as i32;
        let h0 = cell_height(m, base, x, z, 0, 0, parity);
        let xs = (i32::from(m.lh(r)) << 7) as u32;
        m.sh(r.wrapping_add(0x20), xs as u16);
        m.sh(r.wrapping_add(8), h0 as u16);
        let z = lhu(m, r.wrapping_add(2));
        m.sh(r.wrapping_add(0x22), h0.wrapping_neg() as u16);
        m.sh(r.wrapping_add(0x24), (sext16(z) << 7) as u16);
    } else {
        let v1 = m.lw(r.wrapping_add(0x28));
        let v1z = m.lw(r.wrapping_add(0x2c));
        let h1 = m.lhu(r.wrapping_add(0xa));
        m.sw(r.wrapping_add(0x20), v1);
        m.sw(r.wrapping_add(0x24), v1z);
        m.sh(r.wrapping_add(8), h1);
    }
    let z = sext16(lhu(m, r.wrapping_add(2)));
    let x = i32::from(m.lh(r));
    let base = m.lw(HEIGHTS);
    let h1 = cell_height(m, base, x, z, 1, 0, x.wrapping_add(1));
    m.sh(r.wrapping_add(0xa), h1 as u16);
    let z = sext16(lhu(m, r.wrapping_add(2)));
    let x2 = i32::from(m.lh(r));
    let parity = lhu(m, r) as i32;
    let h2 = cell_height(m, base, x2, z, 0, 1, parity);
    m.sh(r.wrapping_add(0xc), h2 as u16);
    let h3 = cell_height(m, base, x, z, 1, 1, x.wrapping_add(1));
    m.sh(r.wrapping_add(0x28), (x.wrapping_add(1) << 7) as u16);
    let h1 = lhu(m, r.wrapping_add(0xa));
    let zs = lhu(m, r.wrapping_add(2));
    m.sh(r.wrapping_add(0x2a), h1.wrapping_neg() as u16);
    m.sh(r.wrapping_add(0x30), (x << 7) as u16);
    m.sh(r.wrapping_add(0x2c), (sext16(zs) << 7) as u16);
    let zs = lhu(m, r.wrapping_add(2));
    m.sh(r.wrapping_add(0x38), (x.wrapping_add(1) << 7) as u16);
    m.sh(r.wrapping_add(0xe), h3 as u16);
    m.sh(r.wrapping_add(0x34), ((sext16(zs) << 7) + 0x80) as u16);
    let zs = lhu(m, r.wrapping_add(2));
    m.sh(r.wrapping_add(0x3c), ((sext16(zs) << 7) + 0x80) as u16);
    let h2 = lhu(m, r.wrapping_add(0xc));
    m.sh(r.wrapping_add(0x3a), h3.wrapping_neg() as u16);
    m.sh(r.wrapping_add(0x32), h2.wrapping_neg() as u16);
}

/// 801048F0: the scene terrain and its overlays into `packet`: a drawing
/// area (SetDrawArea of 0x800CD774) at the hub's ordering-table entry
/// `[[0x80112DC4]+0x14] + 0x3FC`, VSync(1); then, once the hub frame
/// (`[0x80112DC4]+0x50`) reaches 0x53 or the camera is overridden
/// (+0x9D4), the 17x17 grid around the looked-at point (view matrix +0x50)
/// as POLY_G4s (code 0x38) linked by depth / 64; then, without the
/// override and from hub frame 0x3C, the frame's sprites: sounds 100 at
/// frame 0x3C and 0x66 at 0x52, a fading TILE over the map position during
/// frames 0x52..0x71, and the sliding POLY_FT4 panel (CLUT 0x5714, tpage
/// 0x25). Returns the packet pointer after them.
pub fn draw_terrain(m: &mut Machine, packet: u32) -> u32 {
    framed(m, 0x48, |m, sp| {
        call(m, SET_DRAW_AREA, [packet, AREA, 0, 0]);
        let hub = m.lw(HUB);
        let entry = m.lw(hub.wrapping_add(0x14)).wrapping_add(0x3fc);
        add_prim_masked(m, entry, packet);
        let mut prim = packet.wrapping_add(0xc);
        call(m, VSYNC, [1, 0, 0, 0]);
        let block = m.lw(SCENE);
        let draw = m.lbu(block.wrapping_add(0x9d4)) != 0 || {
            let hub = m.lw(HUB);
            (m.lw(hub.wrapping_add(0x50)) as i32) >= 0x53
        };
        if draw {
            prim = draw_grid(m, block, prim, sp);
        }
        let block = m.lw(SCENE);
        if m.lbu(block.wrapping_add(0x9d4)) != 0 {
            return prim;
        }
        let hub = m.lw(HUB);
        let frame = m.lw(hub.wrapping_add(0x50)) as i32;
        if frame < 0x3c {
            return prim;
        }
        panel_corners(m, hub, frame, sp);
        let hub = m.lw(HUB);
        if m.lw(hub.wrapping_add(0x50)) == 0x3c {
            call(m, START_SOUND, [0x64, 0, 0, 0]);
        }
        let hub = m.lw(HUB);
        if m.lw(hub.wrapping_add(0x50)) == 0x52 {
            call(m, START_SOUND, [0x66, 0, 0, 0]);
        }
        let hub = m.lw(HUB);
        if m.lw(hub.wrapping_add(0x50)).wrapping_sub(0x52) < 0x20 {
            prim = fade_tile(m, prim);
        }
        panel(m, prim, sp)
    })
}

/// 801049A0..801057AC: the grid.
fn draw_grid(m: &mut Machine, block: u32, packet: u32, sp: u32) -> u32 {
    let r = 0x1f80_0000u32;
    super::ov2_p6::load_rot_trans(m, block.wrapping_add(0x50));
    let mut prim = packet;
    for row in -8i32..9 {
        let block = m.lw(SCENE);
        let z = m.lw(block.wrapping_add(0x28));
        locate(m, r, 1, z, row as u32);
        for column in -8i32..9 {
            let block = m.lw(SCENE);
            let x = m.lw(block.wrapping_add(0x20));
            m.sw(sp + 0x30, x);
            locate(m, r, 0, x, column as u32);
            let quad = prim;
            prim = prim.wrapping_add(0x24);
            corners(m, r, column == -8);
            correct_edges(m, r, column, row);
            if column == -8 {
                lwc2(m, 0, r + 0x20);
                lwc2(m, 1, r + 0x24);
                m.gte.rtps(true, false);
                swc2(m, 14, quad.wrapping_add(8));
            } else {
                swc2(m, 12, quad.wrapping_add(8));
            }
            for (reg, offset) in [(0, 0x28u32), (2, 0x30), (4, 0x38)] {
                lwc2(m, reg, r + offset);
                lwc2(m, reg + 1, r + offset + 4);
            }
            m.gte.rtpt(true, false);
            corner_colors(m, r);
            swc2(m, 12, quad.wrapping_add(0x10));
            swc2(m, 13, quad.wrapping_add(0x18));
            swc2(m, 14, quad.wrapping_add(0x20));
            let sz = m.gte.mfc2(19) as i32 >> 2;
            m.sw(sp + 0x30, sz as u32);
            blend_edges(m, r, column, row);
            let first = m.lw(r + 0x10);
            m.sw(quad.wrapping_add(4), first | 0x3800_0000);
            for k in 1..4u32 {
                let color = m.lw(r + 0x10 + k * 4);
                m.sw(quad.wrapping_add(4 + k * 8), color);
            }
            let depth = ((m.lw(sp + 0x30) as i32) >> 6) as u32;
            let hub = m.lw(HUB);
            let entry = (depth << 2).wrapping_add(m.lw(hub.wrapping_add(0x14)));
            let link = m.lw(entry);
            m.sw(quad, link | 0x0800_0000);
            let entry = (depth << 2).wrapping_add(m.lw(hub.wrapping_add(0x14)));
            m.sw(entry, (quad << 8) >> 8);
        }
    }
    prim
}

/// 801057C8..80105900: the panel's corners on the stack (+0x10..+0x2A:
/// x0, y0, x1, y1, x2, y2, x3, y3): sliding in from the left during hub
/// frames 0x3C..0x41, then rising with the hub's current object
/// (`[+0x16C]+0x42`) until 0x51, then still.
fn panel_corners(m: &mut Machine, hub: u32, frame: i32, sp: u32) {
    let bottom = if frame < 0x42 {
        let x = 0x9au32.wrapping_sub(((frame - 0x3c) as u32) << 3);
        m.sh(sp + 0x20, x as u16);
        m.sh(sp + 0x10, x as u16);
        let frame = m.lw(hub.wrapping_add(0x50));
        let x = (frame << 3).wrapping_sub(0x146);
        m.sh(sp + 0x28, x as u16);
        m.sh(sp + 0x18, x as u16);
        let current = m.lw(hub.wrapping_add(0x16c));
        let y = lhu(m, current.wrapping_add(0x42)).wrapping_add(0x99);
        m.sh(sp + 0x1a, y as u16);
        m.sh(sp + 0x12, y as u16);
        let current = m.lw(hub.wrapping_add(0x16c));
        lhu(m, current.wrapping_add(0x42)).wrapping_add(0x9b)
    } else if frame < 0x52 {
        m.sh(sp + 0x20, 0x60);
        m.sh(sp + 0x10, 0x60);
        m.sh(sp + 0x28, 0xe0);
        m.sh(sp + 0x18, 0xe0);
        let current = m.lw(hub.wrapping_add(0x16c));
        let base = lhu(m, current.wrapping_add(0x42));
        let frame = m.lw(hub.wrapping_add(0x50));
        let y = base
            .wrapping_add(0x9a)
            .wrapping_sub(frame.wrapping_sub(0x42) << 3);
        m.sh(sp + 0x1a, y as u16);
        m.sh(sp + 0x12, y as u16);
        let frame = m.lw(hub.wrapping_add(0x50));
        let current = m.lw(hub.wrapping_add(0x16c));
        let base = lhu(m, current.wrapping_add(0x42));
        (frame.wrapping_sub(0x42) << 3).wrapping_add(base.wrapping_add(0x9a))
    } else {
        m.sh(sp + 0x20, 0x60);
        m.sh(sp + 0x10, 0x60);
        m.sh(sp + 0x28, 0xe0);
        m.sh(sp + 0x18, 0xe0);
        let current = m.lw(hub.wrapping_add(0x16c));
        let y = lhu(m, current.wrapping_add(0x42)).wrapping_add(0x1a);
        m.sh(sp + 0x1a, y as u16);
        m.sh(sp + 0x12, y as u16);
        let current = m.lw(hub.wrapping_add(0x16c));
        lhu(m, current.wrapping_add(0x42)).wrapping_add(0x11a)
    };
    m.sh(sp + 0x2a, bottom as u16);
    m.sh(sp + 0x22, bottom as u16);
}

/// 80105968..80105A78: the fading TILE (code 0x62, 16x32) over the map
/// position of the looked-at point during hub frames 0x52..0x71, added at
/// the entry `[[0x80112DC4]+0x14] + 0x3FC`. Returns the packet after it.
fn fade_tile(m: &mut Machine, tile: u32) -> u32 {
    m.sb(tile.wrapping_add(3), 3);
    m.sb(tile.wrapping_add(7), 0x62);
    let block = m.lw(SCENE);
    let x = divide_pow2(m.lw(block.wrapping_add(0x20)) as i32, 8).wrapping_add(0x92);
    m.sh(tile.wrapping_add(8), x as u16);
    let z = divide_pow2(m.lw(block.wrapping_add(0x28)) as i32, 8).wrapping_add(8);
    let y = 0x9ai32.wrapping_sub(z << 1);
    let hub = m.lw(HUB);
    m.sh(tile.wrapping_add(0xa), y as u16);
    m.sh(tile.wrapping_add(0xc), 0x10);
    m.sh(tile.wrapping_add(0xe), 0x20);
    for channel in 4..7u32 {
        let t = m.lw(hub.wrapping_add(0x50)).wrapping_sub(0x52);
        let level = (-0x80i32 as u32).wrapping_sub(t << 1);
        m.sb(tile.wrapping_add(channel), level as u8);
    }
    let hub = m.lw(HUB);
    let entry = m.lw(hub.wrapping_add(0x14)).wrapping_add(0x3fc);
    add_prim_masked(m, entry, tile);
    tile.wrapping_add(0x10)
}

/// 80105A7C..80105C04: the panel POLY_FT4 at the stack corners: gray 0x80
/// before hub frame 0x52, fading to black over 0x52..0x71, then black;
/// texture (0, 0)..(0x7F, 0x7F) of tpage 0x25, CLUT 0x5714; added at the
/// entry `[[0x80112DC4]+0x14] + 0x3FC`. Returns the packet after it.
fn panel(m: &mut Machine, quad: u32, sp: u32) -> u32 {
    m.sb(quad.wrapping_add(3), 9);
    m.sb(quad.wrapping_add(7), 0x2c);
    let hub = m.lw(HUB);
    for (to, from) in [
        (8u32, 0x10u32),
        (0xa, 0x12),
        (0x10, 0x18),
        (0x12, 0x1a),
        (0x18, 0x20),
        (0x1a, 0x22),
        (0x20, 0x28),
        (0x22, 0x2a),
    ] {
        let value = m.lhu(sp + from);
        m.sh(quad.wrapping_add(to), value);
    }
    let frame = m.lw(hub.wrapping_add(0x50)) as i32;
    if frame < 0x52 {
        for channel in 4..7u32 {
            m.sb(quad.wrapping_add(channel), 0x80);
        }
    } else if frame < 0x72 {
        let mut t = (frame - 0x52) as u32;
        for channel in 4..7u32 {
            if channel > 4 {
                t = m.lw(hub.wrapping_add(0x50)).wrapping_sub(0x52);
            }
            let level = (-0x80i32 as u32).wrapping_sub(t << 2);
            m.sb(quad.wrapping_add(channel), level as u8);
        }
    } else {
        for channel in 4..7u32 {
            m.sb(quad.wrapping_add(channel), 0);
        }
    }
    m.sh(quad.wrapping_add(0x16), 0x25);
    m.sh(quad.wrapping_add(0xe), 0x5714);
    for offset in [0x14u32, 0x1d, 0x24, 0x25] {
        m.sb(quad.wrapping_add(offset), 0x7f);
    }
    for offset in [0xcu32, 0xd, 0x15, 0x1c] {
        m.sb(quad.wrapping_add(offset), 0);
    }
    let hub = m.lw(HUB);
    let entry = m.lw(hub.wrapping_add(0x14)).wrapping_add(0x3fc);
    add_prim_masked(m, entry, quad);
    quad.wrapping_add(0x28)
}
