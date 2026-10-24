//! JP overlay 2: the bitmap-font renderer (800F33A8..800F4C03), translated
//! from the JP instructions (class `new`, no PAL counterpart).
//!
//! Block `[0x80112E00]` (0xA914 bytes from the arena, 800F3C14). Offsets
//! below are from F = block.at(0x8000), as the code addresses them:
//! * block +0 / +0x2EE0: two buffers of 300 POLY_FT4 (0x28 bytes); block
//!   +0x5DC0 the next free one;
//! * block +0x5DC4 / +0x7534: two buffers of 300 SPRT (0x14 bytes); F+0xCA4
//!   the next free one;
//! * block +0x8CA8 / +0x9AB8: two buffers of DR_MODE packets (0xC bytes);
//!   F+0x28C8 the next free one;
//! * F+0x28CC/D/E colour; F+0x28D0 the length of the string being drawn;
//!   F+0x28D4/+0x28D8 the per-buffer one-entry ordering tables; F+0x28DC
//!   the current table and F+0x28E0 the frame's table;
//! * F+0x28E4/+0x28E6 pen x, y; F+0x28E8/+0x28EA line start x, y;
//! * F+0x28EC tpage, F+0x28EE clut; F+0x28F0 glyphs per texture row,
//!   F+0x28F2 line spacing (halves of the glyph height for the clipped
//!   quads); F+0x28F4/+0x28F6 texture VRAM x, y; F+0x28F8/+0x28FA u, v of
//!   the first glyph; F+0x28FC/+0x28FE clut VRAM x, y;
//! * F+0x2900/+0x2901 glyph width, height; F+0x2902 first character code;
//!   F+0x2903 proportional (widths from the table at F+0x2904);
//!   F+0x2908 semi-transparent; F+0x2909 blend mode; F+0x290A space width;
//!   F+0x290B vertical squeeze on: F+0x290C..F+0x290E the band, F+0x2910
//!   the current line.
use super::ov2_p4::{At, call, div, lh, lt, sext16};
use crate::common::loaded_module;
use crate::machine::{Machine, SmallData};

/// `[0x80112E00]`: the font block.
pub const BLOCK: u32 = 0x8011_2e00;
/// 80027410: arena allocation (resident `arena`).
const ALLOC: u32 = 0x8002_7410;
/// libgpu ClearOTagR, SetDrawMode (resident SDK).
const CLEAR_OTAG_R: u32 = 0x800a_8680;
const SET_DRAW_MODE: u32 = 0x800a_c7f8;
/// libc strlen; the game's vsprintf (resident, 8002877C).
const STRLEN: u32 = 0x800a_def8;
const VSPRINTF: u32 = 0x8002_877c;
/// The proportional-width tables of fonts 3/4, 5/6, 8 and 9.
const WIDTHS_3: u32 = 0x8010_bb38;
const WIDTHS_4: u32 = 0x8010_bad8;
const WIDTHS_5: u32 = 0x8010_bbb0;
const WIDTHS_8: u32 = 0x8010_bb98;
const WIDTHS_9: u32 = 0x8010_bba4;

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x800f_33a8 => set_pen_f33a8(m, a0, a1),
        0x800f_33d0 => set_color_f33d0(m, a0, a1, a2),
        0x800f_33f8 => set_blend_f33f8(m, a0),
        0x800f_34b0 => set_line_f34b0(m, a0),
        0x800f_34c8 => set_squeeze_f34c8(m, a0, a1, a2),
        0x800f_34f4 => select_clut_f34f4(m, a0),
        0x800f_35b8 => select_font_f35b8(m, a0),
        0x800f_3c14 => init_f3c14(m),
        0x800f_3db4 => begin_frame_f3db4(m, a0),
        0x800f_3ef4 => set_table_f3ef4(m, a0),
        0x800f_3f2c => glyph_f3f2c(m, a0),
        0x800f_4124 => glyph_quad_f4124(m, a0),
        0x800f_46e4 => text_sprites_f46e4(m, a2),
        0x800f_4928 => text_quads_f4928(m, a2),
        0x800f_4a78 => text_f4a78(m, a0),
        0x800f_4ae0 => printf_f4ae0(m, args),
        0x800f_4b40 => printf_zeros_f4b40(m, args),
        _ => return None,
    })
}

/// F = `[BLOCK] + 0x8000`, reloaded wherever the original reloads it.
fn f(m: &mut Machine) -> u32 {
    m.lw(BLOCK).wrapping_add(0x8000)
}

/// The tpage halfword at F+0x28EC from the blend mode and texture VRAM
/// position (GetTPage's packing, 4-bit colour). Returns the `(y & 0x100)
/// >> 4` term (the original's last `v0`).
fn store_tpage(m: &mut Machine, b: u32) -> u32 {
    let abr = u32::from(m.lbu(b.at(0x2909)));
    let y = u32::from(m.lhu(b.at(0x28f6)));
    let x = u32::from(m.lhu(b.at(0x28f4)));
    let high = (((y & 0x100) << 16) as i32 >> 20) as u32;
    let tpage = ((abr & 3) << 5) | high | ((x & 0x3ff) >> 6) | ((y & 0x200) << 2);
    m.sh(b.at(0x28ec), tpage as u16);
    high
}

/// The tpage, then the clut halfword at F+0x28EE (`y << 6 | (x >> 4) &
/// 0x3F`) from the clut VRAM position. Returns `(x >> 4) & 0x3F`.
fn store_tpage_clut(m: &mut Machine, b: u32) -> u32 {
    let abr = u32::from(m.lbu(b.at(0x2909)));
    let y = u32::from(m.lhu(b.at(0x28f6)));
    let x = u32::from(m.lhu(b.at(0x28f4)));
    let high = (((y & 0x100) << 16) as i32 >> 20) as u32;
    let tpage = ((abr & 3) << 5) | high | ((x & 0x3ff) >> 6) | ((y & 0x200) << 2);
    let clut_y = u32::from(m.lhu(b.at(0x28fe)));
    let clut_x = u32::from(m.lhu(b.at(0x28fc)));
    m.sh(b.at(0x28ec), tpage as u16);
    let low = (clut_x >> 4) & 0x3f;
    m.sh(b.at(0x28ee), ((clut_y << 6) | low) as u16);
    low
}

/// 800F33A8: the pen and the line start: F+0x28E4 = F+0x28E8 = `x | y <<
/// 16` (words).
pub fn set_pen_f33a8(m: &mut Machine, x: u32, y: u32) -> u32 {
    let b = f(m);
    let v = (x & 0xffff) | (y << 16);
    m.sw(b.at(0x28e4), v);
    m.sw(b.at(0x28e8), v);
    0x8000
}

/// 800F33D0: the colour word F+0x28CC = `r | g << 8 | b << 16`.
pub fn set_color_f33d0(m: &mut Machine, r: u32, g: u32, blue: u32) -> u32 {
    let b = f(m);
    m.sw(b.at(0x28cc), r | (g << 8) | (blue << 16));
    0x8000
}

/// 800F33F8: semi-transparency off (`mode` = -1 as a halfword) or on with
/// blend mode `mode`; then the tpage.
pub fn set_blend_f33f8(m: &mut Machine, mode: u32) -> u32 {
    if sext16(mode) == u32::MAX {
        let b = f(m);
        m.sb(b.at(0x2908), 0);
        let b = f(m);
        m.sb(b.at(0x2909), 0);
    } else {
        let b = f(m);
        m.sb(b.at(0x2908), 1);
        let b = f(m);
        m.sb(b.at(0x2909), mode as u8);
    }
    let b = f(m);
    store_tpage(m, b)
}

/// 800F34B0: the current line F+0x2910 (halfword) of the vertical squeeze.
pub fn set_line_f34b0(m: &mut Machine, line: u32) -> u32 {
    let b = f(m);
    m.sh(b.at(0x2910), line as u16);
    b
}

/// 800F34C8: the vertical squeeze switch F+0x290B and its band F+0x290C ..
/// F+0x290E.
pub fn set_squeeze_f34c8(m: &mut Machine, on: u32, top: u32, bottom: u32) -> u32 {
    let b = f(m);
    m.sb(b.at(0x290b), on as u8);
    let b = f(m);
    m.sh(b.at(0x290c), top as u16);
    m.sh(b.at(0x290e), bottom as u16);
    b
}

/// 800F34F4: the highlighted clut (row 0x15, semi-transparent, blend 2)
/// when `on`, else the plain one (row 0x13, opaque); then tpage and clut.
pub fn select_clut_f34f4(m: &mut Machine, on: u32) -> u32 {
    if on != 0 {
        let b = f(m);
        m.sb(b.at(0x2908), 1);
        let again = f(m);
        m.sh(b.at(0x28fe), 0x15);
        m.sb(again.at(0x2909), 2);
    } else {
        let b = f(m);
        m.sh(b.at(0x28fe), 0x13);
        m.sb(b.at(0x2908), 0);
    }
    let b = f(m);
    store_tpage_clut(m, b)
}

/// The geometry of a font: glyph w/h (F+0x2900/1), F+0x28F0 .. F+0x28FE in
/// the original's store order is spelled out per case below.
fn sb(m: &mut Machine, offset: u32, value: u8) {
    let b = f(m);
    m.sb(b.at(offset), value);
}

/// 800F35B8: select font `font` (1..9; others keep the current one) and
/// recompute tpage, clut and the glyph origin u, v (F+0x28F8 = 4 * (x % 64)
/// as the texture page's column, F+0x28FA = y % 256). Returns that `y %
/// 256`.
pub fn select_font_f35b8(m: &mut Machine, font: u32) -> u32 {
    match font.wrapping_sub(1) {
        0 => {
            sb(m, 0x2900, 6);
            sb(m, 0x2901, 6);
            let b = f(m);
            m.sh(b.at(0x28f0), 0x10);
            m.sh(b.at(0x28f2), 2);
            m.sh(b.at(0x28f4), 0x140);
            m.sh(b.at(0x28f6), 0x140);
            m.sh(b.at(0x28fc), 0x140);
            m.sb(b.at(0x2902), 0x20);
            let again = f(m);
            m.sh(b.at(0x28fe), 0x158);
            m.sb(again.at(0x2903), 0);
            sb(m, 0x2908, 0);
            sb(m, 0x2909, 0);
        }
        1 => {
            sb(m, 0x2900, 6);
            sb(m, 0x2901, 6);
            let b = f(m);
            m.sh(b.at(0x28f2), 2);
            m.sh(b.at(0x28f4), 0x188);
            m.sh(b.at(0x28f6), 0x118);
            m.sh(b.at(0x28fc), 0x180);
            m.sb(b.at(0x2902), 0x20);
            let again = f(m);
            m.sh(b.at(0x28f0), 0x20);
            m.sh(b.at(0x28fe), 0x1fe);
            m.sb(again.at(0x2903), 0);
            sb(m, 0x2908, 0);
            sb(m, 0x2909, 0);
        }
        2 | 3 => {
            sb(m, 0x2900, 0x10);
            sb(m, 0x2901, 0x15);
            let b = f(m);
            m.sh(b.at(0x28f0), 0x10);
            m.sh(b.at(0x28f4), 0x300);
            m.sh(b.at(0x28f6), if font == 3 { 0x17e } else { 0x100 });
            m.sh(b.at(0x28fc), 0x190);
            m.sb(b.at(0x2902), 0x21);
            let again = f(m);
            m.sh(b.at(0x28f2), 1);
            m.sh(b.at(0x28fe), 0x2d);
            m.sb(again.at(0x2903), 1);
            proportional(m, 4, if font == 3 { WIDTHS_3 } else { WIDTHS_4 });
        }
        4 => {
            sb(m, 0x2900, 0xc);
            sb(m, 0x2901, 0x10);
            let b = f(m);
            m.sh(b.at(0x28f0), 0x15);
            m.sh(b.at(0x28f4), 0x340);
            m.sh(b.at(0x28fc), 0x1a0);
            m.sb(b.at(0x2902), 0x21);
            let again = f(m);
            m.sh(b.at(0x28f2), 1);
            m.sh(b.at(0x28f6), 0);
            m.sh(b.at(0x28fe), 0x13);
            m.sb(again.at(0x2903), 1);
            let b = f(m);
            m.sb(b.at(0x290a), 4);
            let again = f(m);
            m.sw(b.at(0x2904), WIDTHS_5);
            m.sb(again.at(0x2908), 0);
            sb(m, 0x2909, 0);
        }
        5 => {
            sb(m, 0x2900, 0xc);
            sb(m, 0x2901, 0x10);
            let b = f(m);
            m.sh(b.at(0x28f4), 0x340);
            m.sb(b.at(0x2902), 0x21);
            let again = f(m);
            m.sh(b.at(0x28f0), 0x15);
            m.sh(b.at(0x28f2), 1);
            m.sh(b.at(0x28f6), 0);
            m.sh(b.at(0x28fc), 0x1a0);
            m.sh(b.at(0x28fe), 0x15);
            m.sb(again.at(0x2903), 1);
            proportional(m, 4, WIDTHS_5);
        }
        6 => {
            sb(m, 0x2900, 6);
            sb(m, 0x2901, 0xa);
            let b = f(m);
            m.sh(b.at(0x28f0), 0x19);
            m.sh(b.at(0x28f2), 1);
            m.sh(b.at(0x28f4), 0x180);
            m.sh(b.at(0x28f6), 0x180);
            m.sh(b.at(0x28fc), 0x190);
            m.sb(b.at(0x2902), 0x30);
            let again = f(m);
            m.sh(b.at(0x28fe), 0x1ff);
            m.sb(again.at(0x2903), 0);
            sb(m, 0x290a, 6);
            sb(m, 0x2908, 0);
            sb(m, 0x2909, 0);
        }
        7 | 8 => {
            sb(m, 0x2900, 8);
            sb(m, 0x2901, 0x1d);
            if font == 8 {
                let b = f(m);
                m.sh(b.at(0x28f0), 0xc);
                m.sh(b.at(0x28f4), 0x396);
                m.sh(b.at(0x28f6), 0x180);
                m.sh(b.at(0x28fc), 0x1b0);
                m.sb(b.at(0x2902), 0x30);
                let again = f(m);
                m.sh(b.at(0x28f2), 1);
                m.sh(b.at(0x28fe), 0x2f);
                m.sb(again.at(0x2903), 0);
            } else {
                let b = f(m);
                m.sb(b.at(0x2902), 0x30);
                let again = f(m);
                m.sh(b.at(0x28f0), 0xc);
                m.sh(b.at(0x28f4), 0x380);
                m.sh(b.at(0x28f6), 0x180);
                m.sh(b.at(0x28f2), 1);
                m.sh(b.at(0x28fc), 0x1b0);
                m.sh(b.at(0x28fe), 0x30);
                m.sb(again.at(0x2903), 0);
            }
            sb(m, 0x290a, 6);
            sb(m, 0x2908, 0);
            sb(m, 0x2909, 0);
            sb(m, 0x2903, 1);
            let b = f(m);
            m.sw(b.at(0x2904), if font == 8 { WIDTHS_8 } else { WIDTHS_9 });
        }
        _ => {}
    }
    let b = f(m);
    let x = lh(m, b.at(0x28f4));
    store_tpage_clut(m, b);
    let column = ((x as i32 % 64) as u32) << 16;
    let y = lh(m, b.at(0x28f6));
    m.sh(b.at(0x28f8), ((column as i32) >> 14) as u16);
    let v = (y as i32 % 256) as u32;
    m.sh(b.at(0x28fa), v as u16);
    v
}

/// The tail of the proportional fonts 3, 4 and 6 (800F3910 with the table
/// address in `v0`): space width, width table, semi-transparent, blend 2.
fn proportional(m: &mut Machine, space: u8, table: u32) {
    let b = f(m);
    m.sb(b.at(0x290a), space);
    let again = f(m);
    m.sw(b.at(0x2904), table);
    m.sb(again.at(0x2908), 1);
    sb(m, 0x2909, 2);
}

/// 800F3C14: allocate the block (0xA914 bytes) and the two one-entry
/// ordering tables; prepare the 300 sprites of each buffer (code 0x64,
/// length 4, colour 0x808080) with font 4; pen (10, 200), colour 0x80.
pub fn init_f3c14(m: &mut Machine) -> u32 {
    let block = call(m, ALLOC, [0xa914, 0, 0, 0]);
    m.sw(BLOCK, block);
    let mut sprites = 0x5dc4u32;
    for buffer in 0..2u32 {
        let table = call(m, ALLOC, [4, 0, 0, 0]);
        let block = m.lw(BLOCK);
        let b = block.wrapping_add(0x8000);
        m.sw(
            block
                .wrapping_add(buffer << 2)
                .wrapping_add(0x8000)
                .at(0x28d4),
            table,
        );
        m.sb(b.at(0x2908), 0);
        let again = f(m);
        m.sw(b.at(0xca4), block.wrapping_add(sprites));
        m.sb(again.at(0x2909), 0);
        select_font_f35b8(m, 4);
        for _ in 0..300 {
            for (offset, value) in [(3, 4), (7, 0x64), (4, 0x80), (5, 0x80), (6, 0x80)] {
                let b = f(m);
                let sprite = m.lw(b.at(0xca4));
                m.sb(sprite.at(offset), value);
            }
            let b = f(m);
            let sprite = m.lw(b.at(0xca4));
            m.sw(b.at(0xca4), sprite.wrapping_add(0x14));
        }
        sprites = sprites.wrapping_add(0x1770);
    }
    let b = f(m);
    m.sw(b.at(0x28d0), 0);
    m.sb(b.at(0x290b), 0);
    set_pen_f33a8(m, 10, 200);
    set_color_f33d0(m, 0x80, 0x80, 0x80)
}

/// 800F3DB4: start a frame in buffer `[context.at(4)]`: the free quad, sprite
/// and mode pointers, the buffer's table (cleared, ClearOTagR(table, 1))
/// as the current and the frame's table, then the table linked into the
/// caller's ordering-table entry `parent`. Returns the table address
/// (masked to 24 bits).
pub fn begin_frame_f3db4(m: &mut Machine, parent: u32) -> u32 {
    let context = m.gw(crate::common::CONTEXT);
    let index = m.lw(context.at(4));
    let block = m.lw(BLOCK);
    m.sw(
        block.wrapping_add(0x5dc0),
        block.wrapping_add(index.wrapping_mul(0x2ee0)),
    );
    let index = m.lw(context.at(4));
    let b = block.wrapping_add(0x8000);
    m.sw(
        b.at(0xca4),
        block.wrapping_add(index.wrapping_mul(0x1770).wrapping_add(0x5dc4)),
    );
    let index = m.lw(context.at(4));
    m.sw(
        b.at(0x28c8),
        block.wrapping_add(index.wrapping_mul(0xe10).wrapping_add(0x8ca8)),
    );
    let index = m.lw(context.at(4));
    let table = m.lw(block
        .wrapping_add(index << 2)
        .wrapping_add(0x8000)
        .at(0x28d4));
    m.sw(b.at(0x28dc), table);
    m.sw(b.at(0x28e0), table);
    call(m, CLEAR_OTAG_R, [table, 1, 0, 0]);
    let b = f(m);
    let table = m.lw(b.at(0x28dc));
    let up = m.lw(parent);
    let head = m.lw(table);
    m.sw(table, (head & 0xff00_0000) | (up & 0x00ff_ffff));
    let up = m.lw(parent);
    let table = m.lw(b.at(0x28dc)) & 0x00ff_ffff;
    m.sw(parent, (up & 0xff00_0000) | table);
    table
}

/// 800F3EF4: the current ordering table F+0x28DC: `table`, or the frame's
/// table (F+0x28E0) for -1.
pub fn set_table_f3ef4(m: &mut Machine, table: u32) -> u32 {
    let b = f(m);
    let value = if table == u32::MAX {
        m.lw(b.at(0x28e0))
    } else {
        table
    };
    m.sw(b.at(0x28dc), value);
    b
}

/// Link primitive `p` (length `words`) into the current table and advance
/// the pen by the glyph's width (the proportional table, or width.at(1)).
fn link_and_advance(m: &mut Machine, p: u32, words: u32, code: u32) {
    let b = f(m);
    let table = m.lw(b.at(0x28dc));
    let head = m.lw(table);
    m.sw(p, head | (words << 24));
    let table = m.lw(b.at(0x28dc));
    m.sw(table, p & 0x00ff_ffff);
    let b = f(m);
    let proportional = m.lbu(b.at(0x2903));
    if proportional != 0 {
        let widths = m.lw(b.at(0x2904));
        let width = m.lbu(widths.wrapping_add(code & 0xff));
        let x = m.lhu(b.at(0x28e4));
        m.sh(b.at(0x28e4), x.wrapping_add(u16::from(width)));
    } else {
        let width = m.lbu(b.at(0x2900));
        let x = m.lhu(b.at(0x28e4));
        m.sh(b.at(0x28e4), x.wrapping_add(u16::from(width) + 1));
    }
}

/// The glyph's cell in the font texture: (column, row) of `code - first`
/// (as a byte) over F+0x28F0 glyphs per row.
fn cell(m: &mut Machine, b: u32, code: u32, zero: u32, overflow: u32) -> (u32, u32, u32) {
    let first = u32::from(m.lbu(b.at(0x2902)));
    let index = code.wrapping_sub(first);
    let per_row = lh(m, b.at(0x28f0));
    let (row, column) = div(m, index & 0xff, per_row, zero, overflow);
    (index, row, column)
}

/// 800F3F2C: one glyph as a sprite (SPRT, 0x14 bytes) at the pen, linked
/// into the current table; the pen advances. Returns the next free sprite.
pub fn glyph_f3f2c(m: &mut Machine, code: u32) -> u32 {
    let b = f(m);
    let (index, row, column) = cell(m, b, code, 0x800f_3f5c, 0x800f_3f74);
    let w = u32::from(m.lbu(b.at(0x2900)));
    let du = column.wrapping_mul(w);
    let h = u32::from(m.lbu(b.at(0x2901)));
    let dv = row.wrapping_mul(h);
    let u = u32::from(m.lhu(b.at(0x28f8))).wrapping_add(du);
    let v = u32::from(m.lhu(b.at(0x28fa)));
    let sprite = m.lw(b.at(0xca4));
    m.sb(sprite.at(0xc), u as u8);
    m.sb(sprite.at(0xd), v.wrapping_add(dv) as u8);
    let b = f(m);
    let h = u32::from(m.lbu(b.at(0x2901)));
    let w = u32::from(m.lbu(b.at(0x2900)));
    m.sw(sprite.at(0x10), w | (h << 16));
    let y = lh(m, b.at(0x28e6));
    let x = u32::from(m.lhu(b.at(0x28e4)));
    m.sw(sprite.at(8), x | (y << 16));
    let clut = m.lhu(b.at(0x28ee));
    m.sh(sprite.at(0xe), clut);
    let r = m.lbu(b.at(0x28cc));
    m.sb(sprite.at(4), r);
    let b = f(m);
    let g = m.lbu(b.at(0x28cd));
    m.sb(sprite.at(5), g);
    let b = f(m);
    let blue = m.lbu(b.at(0x28ce));
    m.sb(sprite.at(6), blue);
    let b = f(m);
    let code_byte = if m.lbu(b.at(0x2908)) != 0 {
        m.lbu(sprite.at(7)) | 2
    } else {
        m.lbu(sprite.at(7)) & 0xfd
    };
    m.sb(sprite.at(7), code_byte);
    link_and_advance(m, sprite, 4, index);
    let b = f(m);
    let next = m.lw(b.at(0xca4)).wrapping_add(0x14);
    m.sw(b.at(0xca4), next);
    next
}

/// Signed `x / 2` as compiled (`(x + (x >> 31 logical)) >> 1`).
fn half(x: u32) -> u32 {
    (x.wrapping_add(x >> 31) as i32 >> 1) as u32
}

/// 800F4124: one glyph as a textured quad (POLY_FT4, 0x28 bytes) at the
/// pen. With the vertical squeeze on (F+0x290B = 1), a glyph above the band
/// (line F+0x2910 < F+0x290C) is not drawn (returns 0), one inside the band
/// is scaled vertically by (bottom - line) / (bottom - top). Returns the
/// next free quad.
pub fn glyph_quad_f4124(m: &mut Machine, code: u32) -> u32 {
    let block = m.lw(BLOCK);
    let t1 = block.wrapping_add(0x8000);
    let (index, row, column) = cell(m, t1, code, 0x800f_4150, 0x800f_4168);
    let w = u32::from(m.lbu(t1.at(0x2900)));
    let du = column.wrapping_mul(w);
    let h = u32::from(m.lbu(t1.at(0x2901)));
    let dv = row.wrapping_mul(h);
    let u = u32::from(m.lhu(t1.at(0x28f8)));
    let quad = m.lw(block.wrapping_add(0x5dc0));
    let v = u32::from(m.lhu(t1.at(0x28fa)));
    let u = sext16(u.wrapping_add(du));
    let clut = lh(m, t1.at(0x28ee)) << 16;
    let u1 = u.wrapping_sub(1);
    let v = sext16(v.wrapping_add(dv));
    let v8 = v << 8;
    m.sw(quad.at(0xc), u | v8 | clut);
    let w = u32::from(m.lbu(t1.at(0x2900)));
    let tpage = lh(m, t1.at(0x28ec));
    m.sw(quad.at(0x14), u1.wrapping_add(w) | v8 | (tpage << 16));
    let h = u32::from(m.lbu(t1.at(0x2901)));
    let v1 = v.wrapping_sub(1);
    m.sw(quad.at(0x1c), u | (v1.wrapping_add(h) << 8));
    let w = u32::from(m.lbu(t1.at(0x2900)));
    let h = u32::from(m.lbu(t1.at(0x2901)));
    m.sw(
        quad.at(0x24),
        u1.wrapping_add(w) | (v1.wrapping_add(h) << 8),
    );
    let g = u32::from(m.lbu(t1.at(0x28cd)));
    let r = u32::from(m.lbu(t1.at(0x28cc)));
    let blue = u32::from(m.lbu(t1.at(0x28ce)));
    m.sw(quad.at(4), r | (g << 8) | (blue << 16));
    let squeeze = m.lbu(t1.at(0x290b));
    let mut scaled = false;
    if squeeze == 1 {
        let line = lh(m, t1.at(0x2910));
        let top = lh(m, t1.at(0x290c));
        if lt(line, top) {
            return 0;
        }
        let bottom = lh(m, t1.at(0x290e));
        if lt(line, bottom) {
            scaled = true;
            squeezed_corners(m, t1, quad, bottom, line, top);
        }
    }
    if !scaled {
        let y = lh(m, t1.at(0x28e6));
        let x = u32::from(m.lhu(t1.at(0x28e4)));
        m.sw(quad.at(8), (y << 16) | x);
        let y = lh(m, t1.at(0x28e6));
        let x = lh(m, t1.at(0x28e4));
        let w = u32::from(m.lbu(t1.at(0x2900)));
        m.sw(
            quad.at(0x10),
            (y << 16) | (x.wrapping_sub(1).wrapping_add(w) & 0xffff),
        );
        let h = u32::from(m.lbu(t1.at(0x2901)));
        let spacing = lh(m, t1.at(0x28f2));
        let dy = h.wrapping_sub(1).wrapping_mul(spacing);
        let y = lh(m, t1.at(0x28e6));
        let x = u32::from(m.lhu(t1.at(0x28e4)));
        m.sw(quad.at(0x18), (y.wrapping_add(dy) << 16) | x);
        let h = u32::from(m.lbu(t1.at(0x2901)));
        let spacing = lh(m, t1.at(0x28f2));
        let dy = h.wrapping_sub(1).wrapping_mul(spacing);
        let w = u32::from(m.lbu(t1.at(0x2900)));
        let x = lh(m, t1.at(0x28e4));
        let y = lh(m, t1.at(0x28e6));
        let x = x.wrapping_sub(1).wrapping_add(w) & 0xffff;
        m.sw(quad.at(0x20), (y.wrapping_add(dy) << 16) | x);
    }
    m.sb(quad.at(3), 9);
    m.sb(quad.at(7), 0x2c);
    let b = f(m);
    let code_byte = if m.lbu(b.at(0x2908)) != 0 { 0x2e } else { 0x2c };
    m.sb(quad.at(7), code_byte);
    link_and_advance(m, quad, 9, index);
    let block = m.lw(BLOCK);
    let next = m.lw(block.wrapping_add(0x5dc0)).wrapping_add(0x28);
    m.sw(block.wrapping_add(0x5dc0), next);
    next
}

/// The four vertices of a squeezed glyph (800F4280..800F45F4): each row
/// offset `h * spacing / 2 * (bottom - line) / (bottom - top)` with the
/// compiler's division checks.
fn squeezed_corners(m: &mut Machine, t1: u32, quad: u32, bottom: u32, line: u32, top: u32) {
    // vertex 0: y + (h*s/2) * (bottom - line) / (bottom - top)
    let h = u32::from(m.lbu(t1.at(0x2901)));
    let s = lh(m, t1.at(0x28f2));
    let n = half(h.wrapping_mul(s)).wrapping_mul(bottom.wrapping_sub(line));
    let (q, _) = div(m, n, bottom.wrapping_sub(top), 0x800f_42c0, 0x800f_42d8);
    let y = lh(m, t1.at(0x28e6));
    let x = u32::from(m.lhu(t1.at(0x28e4)));
    m.sw(quad.at(8), (y.wrapping_add(q) << 16) | x);
    // vertex 1: x + w - 1, the same y
    let h = u32::from(m.lbu(t1.at(0x2901)));
    let s = lh(m, t1.at(0x28f2));
    let line = lh(m, t1.at(0x2910));
    let bottom = lh(m, t1.at(0x290e));
    let n = half(h.wrapping_mul(s)).wrapping_mul(bottom.wrapping_sub(line));
    let top = lh(m, t1.at(0x290c));
    let (q, _) = div(m, n, bottom.wrapping_sub(top), 0x800f_4348, 0x800f_4360);
    let x = lh(m, t1.at(0x28e4));
    let y = lh(m, t1.at(0x28e6));
    let w = u32::from(m.lbu(t1.at(0x2900)));
    m.sw(
        quad.at(0x10),
        (y.wrapping_add(q) << 16) | (x.wrapping_sub(1).wrapping_add(w) & 0xffff),
    );
    // vertex 2: the second row, + ((h-1)*s) * (line - top) / (bottom - top)
    let h = u32::from(m.lbu(t1.at(0x2901)));
    let s = lh(m, t1.at(0x28f2));
    let line = lh(m, t1.at(0x2910));
    let bottom = lh(m, t1.at(0x290e));
    let n = half(h.wrapping_mul(s)).wrapping_mul(bottom.wrapping_sub(line));
    let top = lh(m, t1.at(0x290c));
    let range = bottom.wrapping_sub(top);
    let (q0, _) = div(m, n, range, 0x800f_43dc, 0x800f_43f4);
    let n = h
        .wrapping_sub(1)
        .wrapping_mul(s)
        .wrapping_mul(line.wrapping_sub(top));
    let (q1, _) = div(m, n, range, 0x800f_4430, 0x800f_4448);
    let y = lh(m, t1.at(0x28e6));
    let x = u32::from(m.lhu(t1.at(0x28e4)));
    m.sw(
        quad.at(0x18),
        (y.wrapping_add(q0).wrapping_add(q1) << 16) | x,
    );
    // vertex 3
    let h = u32::from(m.lbu(t1.at(0x2901)));
    let s = lh(m, t1.at(0x28f2));
    let bottom = lh(m, t1.at(0x290e));
    let line = lh(m, t1.at(0x2910));
    let n = half(h.wrapping_mul(s)).wrapping_mul(bottom.wrapping_sub(line));
    let top = lh(m, t1.at(0x290c));
    let range = bottom.wrapping_sub(top);
    let (q0, _) = div(m, n, range, 0x800f_44bc, 0x800f_44d4);
    let n = h
        .wrapping_sub(1)
        .wrapping_mul(s)
        .wrapping_mul(line.wrapping_sub(top));
    let (q1, _) = div(m, n, range, 0x800f_4510, 0x800f_4528);
    let x = lh(m, t1.at(0x28e4));
    let y = lh(m, t1.at(0x28e6));
    let w = u32::from(m.lbu(t1.at(0x2900)));
    let y = y.wrapping_add(q0).wrapping_add(q1) << 16;
    m.sw(
        quad.at(0x20),
        y | (x.wrapping_sub(1).wrapping_add(w) & 0xffff),
    );
}

/// A DR_MODE packet at F+0x28C8 (SetDrawMode(p, 0, 0, `tpage`, NULL),
/// the fifth argument in the caller's frame) linked into the current table;
/// the packet pointer advances by 0xC. `frame` is the original's frame.
fn draw_mode(m: &mut Machine, frame: u32, tpage: u32) -> u32 {
    let b = f(m);
    m.sw(frame.at(0x10), 0);
    let packet = m.lw(b.at(0x28c8));
    call(m, SET_DRAW_MODE, [packet, 0, 0, tpage]);
    let b = f(m);
    let packet = m.lw(b.at(0x28c8));
    let table = m.lw(b.at(0x28dc));
    let tag = m.lw(packet);
    let head = m.lw(table);
    m.sw(packet, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let table = m.lw(b.at(0x28dc));
    let packet = m.lw(b.at(0x28c8));
    let head = m.lw(table);
    m.sw(table, (head & 0xff00_0000) | (packet & 0x00ff_ffff));
    m.lw(b.at(0x28c8)).wrapping_add(0xc)
}

/// One character of a string: newline (0xA: next line, pen to the line
/// start), tab (9: pen x to the line start), space (0x20: advance), else a
/// glyph (`glyph`).
fn character(m: &mut Machine, c: u8, glyph: fn(&mut Machine, u32) -> u32) {
    match c {
        0xa => {
            let b = f(m);
            let h = u32::from(m.lbu(b.at(0x2901)));
            let s = lh(m, b.at(0x28f2));
            let start = m.lhu(b.at(0x28e8));
            let y = u32::from(m.lhu(b.at(0x28e6)));
            m.sh(b.at(0x28e4), start);
            m.sh(b.at(0x28e6), y.wrapping_add(h.wrapping_mul(s)) as u16);
        }
        9 => {
            let b = f(m);
            let start = m.lhu(b.at(0x28e8));
            m.sh(b.at(0x28e4), start);
        }
        0x20 => {
            let b = f(m);
            let advance = if m.lbu(b.at(0x2903)) != 0 {
                m.lbu(b.at(0x290a))
            } else {
                m.lbu(b.at(0x2900)).wrapping_add(1)
            };
            let x = m.lhu(b.at(0x28e4));
            m.sh(b.at(0x28e4), x.wrapping_add(u16::from(advance)));
        }
        _ => {
            glyph(m, u32::from(c));
        }
    }
}

/// 800F46E4: draw the F+0x28D0 characters at `text` as sprites, between a
/// DR_MODE with tpage 0 and one with the font's tpage (F+0x28EC). Returns
/// the next free packet.
pub fn text_sprites_f46e4(m: &mut Machine, text: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let next = draw_mode(m, frame, 0);
    let b = f(m);
    m.sw(b.at(0x28c8), next);
    let count = m.lw(b.at(0x28d0));
    if !lt(count, 1) {
        let mut i = 0u32;
        loop {
            let c = m.lbu(text.wrapping_add(i));
            character(m, c, glyph_f3f2c);
            let b = f(m);
            let count = m.lw(b.at(0x28d0));
            i = i.wrapping_add(1);
            if !lt(i, count) {
                break;
            }
        }
    }
    let b = f(m);
    let tpage = lh(m, b.at(0x28ec));
    let next = draw_mode(m, frame, tpage);
    let b = f(m);
    m.sw(b.at(0x28c8), next);
    m.pop_frame(0x30);
    next
}

/// 800F4928: draw the F+0x28D0 characters at `text` as quads (800F4124).
/// Returns 0, or the count when it is not positive.
pub fn text_quads_f4928(m: &mut Machine, text: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let _ = frame;
    let b = f(m);
    let count = m.lw(b.at(0x28d0));
    let mut result = count;
    if !lt(count, 1) {
        let mut i = 0u32;
        loop {
            let c = m.lbu(text.wrapping_add(i));
            character(m, c, glyph_quad_f4124);
            let b = f(m);
            let count = m.lw(b.at(0x28d0));
            i = i.wrapping_add(1);
            if !lt(i, count) {
                break;
            }
        }
        result = 0;
    }
    m.pop_frame(0x28);
    result
}

/// 800F4A78: draw `text` (its strlen as the count) as sprites at the pen.
pub fn text_f4a78(m: &mut Machine, text: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let _ = frame;
    let b = f(m);
    let length = call(m, STRLEN, [text, 0, 0, 0]);
    m.sw(b.at(0x28d0), length);
    let v = text_sprites_f46e4(m, text);
    m.pop_frame(0x28);
    v
}

/// Store the four register arguments in the caller's frame (`sw a0..a3,
/// 0..0xC(sp)`) and vsprintf into the routine's 0x200-byte buffer at
/// frame.at(0x10) (arguments from frame + 0x21C). Returns (frame, length).
fn format(m: &mut Machine, args: [u32; 4]) -> (u32, u32) {
    let caller = m.sp;
    for (i, a) in args.iter().enumerate() {
        m.sw(caller.wrapping_add(4 * i as u32), *a);
    }
    let frame = m.push_frame(0x218);
    m.sw(frame.at(0x218), args[0]);
    let length = call(m, VSPRINTF, [frame.at(0x10), args[0], frame.at(0x21c), 0]);
    (frame, length)
}

/// 800F4AE0: printf(format, ...) as quads at the pen.
pub fn printf_f4ae0(m: &mut Machine, args: [u32; 4]) -> u32 {
    let (frame, length) = format(m, args);
    let b = f(m);
    m.sw(b.at(0x28d0), length);
    let v = text_quads_f4928(m, frame.at(0x10));
    m.pop_frame(0x218);
    v
}

/// 800F4B40: printf(format, ...) as quads at the pen, spaces drawn as '0'.
pub fn printf_zeros_f4b40(m: &mut Machine, args: [u32; 4]) -> u32 {
    let (frame, length) = format(m, args);
    let b = f(m);
    m.sw(b.at(0x28d0), length);
    if !lt(length, 1) {
        let mut i = 0u32;
        loop {
            let at = frame.wrapping_add(0x10).wrapping_add(i);
            if m.lbu(at) == 0x20 {
                m.sb(at, 0x30);
            }
            let b = f(m);
            let count = m.lw(b.at(0x28d0));
            i = i.wrapping_add(1);
            if !lt(i, count) {
                break;
            }
        }
    }
    let v = text_quads_f4928(m, frame.at(0x10));
    m.pop_frame(0x218);
    v
}
