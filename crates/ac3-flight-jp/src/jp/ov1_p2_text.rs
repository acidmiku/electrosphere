//! JP overlay 1, part 2: the text block and its glyph drawing, and the
//! kanji-ROM glyph routines (`800D7644`..`800D8AEC`).
//!
//! The text block `[0x800DDA58]` (0x4C bytes, [`init_text`]): +0x0/+0x4 two
//! packet areas of 1000 textured quads (0x28 bytes each), +0x8 the packet
//! cursor, +0xC..+0xE the colour, +0x10 the length of the string to print,
//! +0x14/+0x18 the one-entry ordering tables of the two buffers, +0x1C the
//! current one, +0x20/+0x22 the cursor, +0x24/+0x26 the line start, +0x28
//! texture page, +0x2A CLUT, +0x2C glyphs per texture row, +0x2E line
//! spacing factor, +0x30/+0x32 the font texture and +0x34/+0x36 its CLUT in
//! VRAM, +0x38/+0x39 the glyph cell, +0x3A the first character, +0x3B
//! proportional (spacing table +0x3C, space width +0x42), +0x40/+0x41
//! blending (on, mode), +0x43 the skew flag over lines +0x44..+0x46 and
//! +0x48 the line being drawn.
//!
//! The kanji-ROM routines (`800D7644`, `800D783C`, `800D7868`, `800D7A58`,
//! `800D7C18`, the block `[0x800DDA50]`) are statically unreachable: no
//! `jal`, stored pointer or formed address reaches them (JP_PORT_PLAN
//! §2.3). They are translated faithfully; `Krom2RawAdd` (800AF438) and
//! LoadImage2 (800AA8E0) are called through `dispatch`.
use super::ov1_p2::{
    TEXT, call, context, div_trap, lbu, lh, lhu, loaded, mul, off, sext16, slt, sra,
};
use crate::{arena, machine::Machine, sdk};

/// `[0x800DDA50]`: the kanji glyph block (12 bytes: two sprite areas and
/// the cursor +8).
const GLYPHS: u32 = 0x800d_da50;
/// 800AF438 Krom2RawAdd(sjis): the kanji ROM address of a glyph, or -1.
const KROM2RAWADD: u32 = 0x800a_f438;
/// 800AA8E0 LoadImage2(rect, pixels).
const LOAD_IMAGE2: u32 = 0x800a_a8e0;
/// 8002877C vsprintf(buffer, format, arguments).
const VSPRINTF: u32 = 0x8002_877c;
/// 0x800CC568: the glyph upload rectangle {0x340, 0x100, 4, 16}.
const GLYPH_RECT: u32 = 0x800c_c568;
/// 0x800DD810 / 0x800DD870: the proportional spacing tables of fonts 4 / 3.
const SPACING_4: u32 = 0x800d_d810;
const SPACING_3: u32 = 0x800d_d870;

fn text(m: &mut Machine) -> u32 {
    m.lw(TEXT)
}
fn glyphs(m: &mut Machine) -> u32 {
    m.lw(GLYPHS)
}
/// The kanji glyph block's packet cursor (+8).
fn glyph_cursor(m: &mut Machine) -> u32 {
    let block = glyphs(m);
    m.lw(off(block, 8))
}
/// The text block's packet cursor (+8).
fn text_cursor(m: &mut Machine) -> u32 {
    let t = text(m);
    m.lw(off(t, 8))
}
/// The length of the string to print (+0x10).
fn text_length(m: &mut Machine) -> u32 {
    let t = text(m);
    m.lw(off(t, 0x10))
}
/// addPrim with word operations: the packet `prim` takes the link of the
/// ordering-table entry `ot`, which then points at the packet.
fn add_prim(m: &mut Machine, ot: u32, prim: u32) {
    let tag = m.lw(prim);
    let next = m.lw(ot);
    m.sw(prim, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let entry = m.lw(ot);
    m.sw(ot, (entry & 0xff00_0000) | (prim & 0x00ff_ffff));
}

// ---- the kanji-ROM glyphs (unreachable) -------------------------------------------------

/// 800D7644 (unreachable): the kanji glyph block: two areas of 100 textured
/// quads (code 0x2C, grey, texture page 0x1D, the CLUT at `x`, `y`) and a
/// two-colour CLUT (transparent, white) uploaded at `x`, `y`.
pub fn init_glyphs(m: &mut Machine, x: u32, y: u32) {
    let frame = m.push_frame(0x58);
    let block = arena::alloc(m, 0xc);
    m.sw(GLYPHS, block);
    let mut area_index = 0u32;
    let mut count: u32;
    loop {
        let area = arena::alloc(m, 0xfa0);
        count = 0;
        let clut = (y << 6) | (sra(x, 4) & 0x3f);
        let block = glyphs(m);
        m.sw(off(block, sra(area_index << 16, 14)), area);
        m.sw(off(block, 8), area);
        loop {
            let prim = glyph_cursor(m);
            m.sb(off(prim, 3), 9);
            let prim = glyph_cursor(m);
            m.sb(off(prim, 7), 0x2c);
            let prim = glyph_cursor(m);
            let code = lbu(m, off(prim, 7)) & 0xfd;
            m.sb(off(prim, 7), code as u8);
            for k in 4..7 {
                let prim = glyph_cursor(m);
                m.sb(off(prim, k), 0x80);
            }
            let block = glyphs(m);
            let prim = m.lw(off(block, 8));
            m.sh(off(prim, 0x16), 0x1d);
            let prim = m.lw(off(block, 8));
            m.sh(off(prim, 0xe), clut as u16);
            count = count.wrapping_add(1);
            let prim = m.lw(off(block, 8));
            m.sw(off(block, 8), prim.wrapping_add(0x28));
            if !slt(sext16(count), 0x64) {
                break;
            }
        }
        area_index = area_index.wrapping_add(1);
        count = 2;
        if !slt(sext16(area_index), 2) {
            break;
        }
    }
    m.sh(off(frame, 0x34), 0x10);
    m.sh(off(frame, 0x36), 1);
    m.sh(off(frame, 0x30), x as u16);
    m.sh(off(frame, 0x32), y as u16);
    m.sh(off(frame, 0x10), 0);
    m.sh(off(frame, 0x12), 0x7fff);
    loop {
        let at = off(frame, 0x10).wrapping_add(sra(count << 16, 15));
        m.sh(at, 0x8000);
        count = count.wrapping_add(1);
        if !slt(sext16(count), 0x10) {
            break;
        }
    }
    call(m, LOAD_IMAGE2, [off(frame, 0x30), off(frame, 0x10), 0, 0]);
    m.pop_frame(0x58);
}

/// 800D783C (unreachable): the kanji ROM address of the Shift-JIS character
/// at `text` (Krom2RawAdd), or -1.
pub fn glyph_address(m: &mut Machine, text: u32) -> u32 {
    m.push_frame(0x18);
    let high = lbu(m, text);
    let low = lbu(m, off(text, 1));
    let address = call(m, KROM2RAWADD, [low | (high << 8), 0, 0, 0]);
    m.pop_frame(0x18);
    address
}

/// 800D7868 (unreachable): upload the 16x15 kanji ROM glyphs of the
/// Shift-JIS string `string` as 4-bit images (16 lines, the last clear; a
/// blank one for a character the ROM lacks), one 4x16 VRAM cell after the
/// other from 0x340, 0x100 (16 cells a row).
pub fn upload_glyphs(m: &mut Machine, string: u32) {
    let frame = m.push_frame(0xa8);
    let rect = super::ov1_p2::load_unaligned(m, GLYPH_RECT);
    let size = super::ov1_p2::load_unaligned(m, off(GLYPH_RECT, 4));
    super::ov1_p2::store_unaligned(m, off(frame, 0x90), rect);
    super::ov1_p2::store_unaligned(m, off(frame, 0x94), size);
    let mut at = string;
    if lbu(m, at) != 0 {
        loop {
            let mut glyph = glyph_address(m, at);
            let pixels = off(frame, 0x10);
            if glyph == u32::MAX {
                for i in 0..0x80 {
                    m.sb(off(pixels, i), 0);
                }
            } else {
                let mut out = pixels;
                for _row in 0..0xf {
                    for bits in [7..=7, 15..=15] {
                        let top = *bits.start();
                        let mut bit = top;
                        loop {
                            let into = out;
                            let value = sra(lhu(m, glyph), bit) & 1;
                            if (bit as i32).wrapping_rem(2) == 1 {
                                m.sb(out, value as u8);
                            } else {
                                let mut byte = lbu(m, out);
                                out = out.wrapping_add(1);
                                if value != 0 {
                                    byte |= 0x10;
                                }
                                m.sb(into, byte as u8);
                            }
                            bit = bit.wrapping_sub(1);
                            let more = if top == 7 {
                                (bit as i32) >= 0
                            } else {
                                !slt(bit, 8)
                            };
                            if !more {
                                break;
                            }
                        }
                    }
                    glyph = glyph.wrapping_add(2);
                }
                for i in 0x78..0x80 {
                    m.sb(off(pixels, i), 0);
                }
            }
            call(m, LOAD_IMAGE2, [off(frame, 0x90), pixels, 0, 0]);
            let x = lhu(m, off(frame, 0x90)).wrapping_add(4);
            m.sh(off(frame, 0x90), x as u16);
            let x = x & 0xffff;
            at = at.wrapping_add(2);
            if x & 0x3f == 0 {
                let y = lhu(m, off(frame, 0x92));
                m.sh(off(frame, 0x90), x.wrapping_sub(0x40) as u16);
                m.sh(off(frame, 0x92), y.wrapping_add(0x10) as u16);
            }
            if lbu(m, at) == 0 {
                break;
            }
        }
    }
    m.pop_frame(0xa8);
}

/// 800D7A58 (unreachable): `count` glyph quads from glyph `first` (16 a
/// texture row, 16x16 pixels) at `x`, `y` (16 apart), linked into the
/// ordering-table entry at `[sp+0x10]`.
pub fn draw_glyphs(m: &mut Machine, x: u32, y: u32, first: u32, count: u32) {
    let sp = m.sp;
    let ot = m.lw(off(sp, 0x10));
    let first = sext16(first);
    let end = first.wrapping_add(sext16(count));
    let mut index = first;
    while slt(index, end) {
        let prim = glyph_cursor(m);
        let rounded = if (index as i32) < 0 {
            index.wrapping_add(0xf)
        } else {
            index
        };
        let v = sra(rounded, 4) << 4;
        let u = index.wrapping_sub(v) << 4;
        m.sb(off(prim, 0xc), u as u8);
        let prim = glyph_cursor(m);
        m.sb(off(prim, 0xd), v as u8);
        let prim = glyph_cursor(m);
        let u1 = u.wrapping_add(0xf);
        m.sb(off(prim, 0x14), u1 as u8);
        let prim = glyph_cursor(m);
        m.sb(off(prim, 0x15), v as u8);
        let prim = glyph_cursor(m);
        m.sb(off(prim, 0x1c), u as u8);
        let prim = glyph_cursor(m);
        let v1 = v.wrapping_add(0xf);
        m.sb(off(prim, 0x1d), v1 as u8);
        let prim = glyph_cursor(m);
        m.sb(off(prim, 0x24), u1 as u8);
        let prim = glyph_cursor(m);
        m.sb(off(prim, 0x25), v1 as u8);
        let left = x.wrapping_add(index.wrapping_sub(first) << 4);
        let block = glyphs(m);
        let right = left.wrapping_add(0x10);
        let bottom = y.wrapping_add(0x20);
        for (offset, value) in [
            (8, left),
            (0xa, y),
            (0x10, right),
            (0x12, y),
            (0x18, left),
            (0x1a, bottom),
            (0x20, right),
            (0x22, bottom),
        ] {
            let prim = m.lw(off(block, 8));
            m.sh(off(prim, offset), value as u16);
        }
        let prim = m.lw(off(block, 8));
        add_prim(m, ot, prim);
        let prim = m.lw(off(block, 8));
        index = index.wrapping_add(1);
        m.sw(off(block, 8), prim.wrapping_add(0x28));
    }
}

/// 800D7C18 (unreachable): select the kanji glyph area of the current
/// buffer (context +4).
pub fn select_glyph_area(m: &mut Machine) {
    let ctx = context(m);
    let buffer = m.lw(off(ctx, 4));
    let block = glyphs(m);
    let area = m.lw(off(block, buffer << 2));
    m.sw(off(block, 8), area);
}

// ---- the text block ----------------------------------------------------------------

/// 800D7C40: move the cursor and the line start to `x`, `y`.
pub fn set_position(m: &mut Machine, x: u32, y: u32) {
    let t = text(m);
    m.sh(off(t, 0x20), x as u16);
    m.sh(off(t, 0x24), x as u16);
    m.sh(off(t, 0x22), y as u16);
    m.sh(off(t, 0x26), y as u16);
}

/// 800D7C60: the text colour.
pub fn set_color(m: &mut Machine, r: u32, g: u32, b: u32) {
    let t = text(m);
    m.sb(off(t, 0xc), r as u8);
    let t = text(m);
    m.sb(off(t, 0xd), g as u8);
    let t = text(m);
    m.sb(off(t, 0xe), b as u8);
}

/// 800D7C88: blending off (`mode` -1) or on with `mode`.
pub fn set_blend(m: &mut Machine, mode: u32) {
    let t = text(m);
    if sext16(mode) == u32::MAX {
        m.sb(off(t, 0x40), 0);
        let t = text(m);
        m.sb(off(t, 0x41), 0);
    } else {
        m.sb(off(t, 0x40), 1);
        let t = text(m);
        m.sb(off(t, 0x41), mode as u8);
    }
}

/// 800D7CD4 (JP; PAL 800D0280, changed): the line being drawn (+0x48).
pub fn set_line(m: &mut Machine, line: u32) {
    let t = text(m);
    m.sh(off(t, 0x48), line as u16);
}

/// 800D7CE4 (JP; PAL 800D0290, changed): the skew flag and its lines.
pub fn set_skew(m: &mut Machine, on: u32, top: u32, bottom: u32) {
    let t = text(m);
    m.sb(off(t, 0x43), on as u8);
    let t = text(m);
    m.sh(off(t, 0x44), top as u16);
    m.sh(off(t, 0x46), bottom as u16);
}

/// The texture page and CLUT words of the font fields +0x30..+0x36.
fn font_texture(m: &mut Machine, t: u32) {
    let ty = lhu(m, off(t, 0x32));
    let tx = lhu(m, off(t, 0x30));
    let page = sra((ty & 0x100) << 16, 20) | ((tx & 0x3ff) >> 6) | ((ty & 0x200) << 2);
    let cy = lhu(m, off(t, 0x36));
    m.sh(off(t, 0x28), page as u16);
    let cx = lhu(m, off(t, 0x34));
    let clut = (cy << 6) | ((cx >> 4) & 0x3f);
    m.sh(off(t, 0x2a), clut as u16);
}

/// 800D7D08: select font 1..7 (5 and 6: nothing): its cell, texture,
/// CLUT, first character and spacing.
pub fn set_font(m: &mut Machine, font: u32) {
    let index = font.wrapping_sub(1);
    if index >= 7 {
        return;
    }
    // (cell w, h, glyphs per row, spacing, texture x, y, CLUT x, y, first)
    let (w, h, columns, spacing, tx, ty, cx, cy, first) = match index {
        0 => (6, 6, 0x10, 2, 0x140, 0x140, 0x140, 0x158, 0x20),
        1 => (6, 6, 0x20, 2, 0x188, 0x119, 0x180, 0x1fe, 0x20),
        2 => (0x10, 0x15, 0x10, 1, 0x300, 0x17e, 0x190, 0x2d, 0x21),
        3 => (0x10, 0x15, 0x10, 1, 0x300, 0x100, 0x190, 0x2d, 0x21),
        4 | 5 => return,
        _ => (6, 0xa, 0x19, 1, 0x180, 0x180, 0x190, 0x1ff, 0x30),
    };
    let t = text(m);
    m.sb(off(t, 0x38), w);
    let t = text(m);
    m.sb(off(t, 0x39), h);
    let t = text(m);
    m.sh(off(t, 0x2c), columns);
    m.sh(off(t, 0x2e), spacing);
    m.sh(off(t, 0x30), tx);
    m.sh(off(t, 0x32), ty);
    m.sh(off(t, 0x34), cx);
    m.sh(off(t, 0x36), cy);
    m.sb(off(t, 0x3a), first);
    font_texture(m, t);
    let t2 = text(m);
    match index {
        2 | 3 => {
            m.sb(off(t2, 0x3b), 1);
            let t = text(m);
            let table = if index == 2 { SPACING_3 } else { SPACING_4 };
            m.sw(off(t, 0x3c), table);
            m.sb(off(t, 0x42), 4);
        }
        6 => {
            m.sb(off(t2, 0x3b), 0);
            let t = text(m);
            m.sb(off(t, 0x42), 6);
            let t = text(m);
            m.sb(off(t, 0x40), 0);
            let t = text(m);
            m.sb(off(t, 0x41), 0);
        }
        _ => m.sb(off(t2, 0x3b), 0),
    }
}

/// 800D8094: the cursor (+0x20, +0x22) into `x` and `y`.
pub fn get_position(m: &mut Machine, x: u32, y: u32) {
    let t = text(m);
    let value = lhu(m, off(t, 0x20));
    m.sh(x, value as u16);
    let t = text(m);
    let value = lhu(m, off(t, 0x22));
    m.sh(y, value as u16);
}

/// 800D80C0: the text block: two areas of 1000 grey textured quads (code
/// 0x2C) with their one-entry ordering tables, font 4, blending and skew
/// off, the cursor at 10, 200 and the colour 0x80 grey.
pub fn init_text(m: &mut Machine) {
    m.push_frame(0x28);
    let block = arena::alloc(m, 0x4c);
    m.sw(TEXT, block);
    for buffer in 0..2u32 {
        let area = arena::alloc(m, 0x9c40);
        let t = text(m);
        m.sw(off(t, buffer << 2), area);
        let ot = arena::alloc(m, 4);
        let t = text(m);
        let entry = off(t, buffer << 2);
        let area = m.lw(entry);
        m.sw(off(entry, 0x14), ot);
        m.sb(off(t, 0x40), 0);
        let t2 = text(m);
        m.sw(off(t, 8), area);
        m.sb(off(t2, 0x41), 0);
        set_font(m, 4);
        for _ in 0..1000 {
            let prim = text_cursor(m);
            m.sb(off(prim, 3), 9);
            let prim = text_cursor(m);
            m.sb(off(prim, 7), 0x2c);
            let t = text(m);
            let prim = m.lw(off(t, 8));
            m.sw(off(t, 8), prim.wrapping_add(0x28));
        }
    }
    let t = text(m);
    m.sw(off(t, 0x10), 0);
    m.sb(off(t, 0x43), 0);
    set_position(m, 0xa, 0xc8);
    set_color(m, 0x80, 0x80, 0x80);
    m.pop_frame(0x28);
}

/// 800D81F0: start the text of the frame: the cursor at the current
/// buffer's area, its ordering table cleared and spliced into the entry
/// `ot`.
pub fn begin_text(m: &mut Machine, ot: u32) {
    m.push_frame(0x20);
    let ctx = context(m);
    let buffer = m.lw(off(ctx, 4));
    let t = text(m);
    let area = m.lw(off(t, buffer << 2));
    m.sw(off(t, 8), area);
    let buffer = m.lw(off(ctx, 4));
    let table = m.lw(off(off(t, buffer << 2), 0x14));
    m.sw(off(t, 0x1c), table);
    sdk::clear_otag_r(m, table, 1);
    let t = text(m);
    let table = m.lw(off(t, 0x1c));
    let next = m.lw(ot);
    let entry = m.lw(table);
    m.sw(table, (entry & 0xff00_0000) | (next & 0x00ff_ffff));
    let entry = m.lw(ot);
    let table = m.lw(off(t, 0x1c));
    m.sw(ot, (entry & 0xff00_0000) | (table & 0x00ff_ffff));
    m.pop_frame(0x20);
}

/// One skewed corner offset: `h * s / 2 * (y46 - y48) / (y46 - y44)` with
/// the division's traps at `zero_break` / `overflow_break`.
fn skew_half(m: &mut Machine, t: u32, zero_break: u32, overflow_break: u32) -> u32 {
    let h = lbu(m, off(t, 0x39));
    let s = lh(m, off(t, 0x2e));
    let product = mul(h, s);
    let bottom = lh(m, off(t, 0x46));
    let half = sra(product.wrapping_add(product >> 31), 1);
    let line = lh(m, off(t, 0x48));
    let numerator = mul(half, bottom.wrapping_sub(line));
    let top = lh(m, off(t, 0x44));
    div_trap(
        m,
        numerator,
        bottom.wrapping_sub(top),
        zero_break,
        overflow_break,
    )
    .0
}

/// The lower corners' offset: [`skew_half`] plus `(h - 1) * s * (y48 -
/// y44) / (y46 - y44)`.
fn skew_full(m: &mut Machine, t: u32, breaks: [u32; 4]) -> u32 {
    let h = lbu(m, off(t, 0x39));
    let s = lh(m, off(t, 0x2e));
    let product = mul(h, s);
    let bottom = lh(m, off(t, 0x46));
    let line = lh(m, off(t, 0x48));
    let half = sra(product.wrapping_add(product >> 31), 1);
    let numerator = mul(half, bottom.wrapping_sub(line));
    let top = lh(m, off(t, 0x44));
    let range = bottom.wrapping_sub(top);
    let first = div_trap(m, numerator, range, breaks[0], breaks[1]).0;
    let lower = mul(mul(h.wrapping_sub(1), s), line.wrapping_sub(top));
    let second = div_trap(m, lower, range, breaks[2], breaks[3]).0;
    let y = lhu(m, off(t, 0x22));
    y.wrapping_add(first).wrapping_add(second)
}

/// 800D82A4: draw character `ch` at the cursor as a textured quad of the
/// current font (skewed between lines +0x44 and +0x46 while +0x43 is 1; not
/// at all on a line above +0x44), link it and advance the cursor.
pub fn draw_glyph(m: &mut Machine, ch: u32) {
    let t = text(m);
    let first = lbu(m, off(t, 0x3a));
    let columns = lh(m, off(t, 0x2c));
    let index = ch.wrapping_sub(first);
    let (row, column) = div_trap(m, index & 0xff, columns, 0x800d_82cc, 0x800d_82e4);
    let w = lbu(m, off(t, 0x38));
    let ux = mul(column, w);
    let tx = lh(m, off(t, 0x30));
    let rounded = if (tx as i32) < 0 {
        tx.wrapping_add(0x3f)
    } else {
        tx
    };
    let within = tx.wrapping_sub(sra(rounded, 6) << 6);
    let u0 = ux.wrapping_add(sra(within << 16, 14));
    let h = lbu(m, off(t, 0x39));
    let vy = mul(row, h);
    let ty = lh(m, off(t, 0x32));
    let rounded = if (ty as i32) < 0 {
        ty.wrapping_add(0xff)
    } else {
        ty
    };
    let page = sra(rounded, 8) << 8;
    let prim = m.lw(off(t, 8));
    m.sb(off(prim, 0xc), u0 as u8);
    let v0 = vy.wrapping_add(ty.wrapping_sub(page));
    let prim = text_cursor(m);
    m.sb(off(prim, 0xd), v0 as u8);
    let u_end = u0.wrapping_add(0xff);
    let v_end = v0.wrapping_add(0xff);
    let t = text(m);
    let w = lbu(m, off(t, 0x38));
    let prim = m.lw(off(t, 8));
    m.sb(off(prim, 0x14), w.wrapping_add(u_end) as u8);
    let prim = text_cursor(m);
    m.sb(off(prim, 0x15), v0 as u8);
    let prim = text_cursor(m);
    m.sb(off(prim, 0x1c), u0 as u8);
    let t = text(m);
    let h = lbu(m, off(t, 0x39));
    let prim = m.lw(off(t, 8));
    m.sb(off(prim, 0x1d), h.wrapping_add(v_end) as u8);
    let t = text(m);
    let w = lbu(m, off(t, 0x38));
    let prim = m.lw(off(t, 8));
    m.sb(off(prim, 0x24), w.wrapping_add(u_end) as u8);
    let t = text(m);
    let h = lbu(m, off(t, 0x39));
    let prim = m.lw(off(t, 8));
    m.sb(off(prim, 0x25), h.wrapping_add(v_end) as u8);
    let t = text(m);
    let blend = lbu(m, off(t, 0x40));
    let prim = m.lw(off(t, 8));
    let code = lbu(m, off(prim, 7));
    let code = if blend != 0 { code | 2 } else { code & 0xfd };
    m.sb(off(prim, 7), code as u8);
    for k in 0..3 {
        let t = text(m);
        let prim = m.lw(off(t, 8));
        let c = lbu(m, off(t, 0xc + k));
        m.sb(off(prim, 4 + k), c as u8);
    }
    let t = text(m);
    let mode = lbu(m, off(t, 0x41));
    let ty = lhu(m, off(t, 0x32));
    let tx = lhu(m, off(t, 0x30));
    let tpage =
        ((mode & 3) << 5) | sra((ty & 0x100) << 16, 20) | ((tx & 0x3ff) >> 6) | ((ty & 0x200) << 2);
    let prim = m.lw(off(t, 8));
    m.sh(off(prim, 0x16), tpage as u16);
    let prim = m.lw(off(t, 8));
    let clut = lhu(m, off(t, 0x2a));
    m.sh(off(prim, 0xe), clut as u16);
    let mut skewed = false;
    if lbu(m, off(t, 0x43)) == 1 {
        let line = lh(m, off(t, 0x48));
        let top = lh(m, off(t, 0x44));
        if slt(line, top) {
            return;
        }
        let bottom = lh(m, off(t, 0x46));
        skewed = slt(line, bottom);
    }
    let x = |m: &mut Machine| lhu(m, off(t, 0x20));
    let y = |m: &mut Machine| lhu(m, off(t, 0x22));
    let right = |m: &mut Machine| {
        let w = lbu(m, off(t, 0x38));
        let x = lhu(m, off(t, 0x20));
        x.wrapping_add(w.wrapping_add(0xffff))
    };
    let prim = m.lw(off(t, 8));
    let x0 = x(m);
    m.sh(off(prim, 8), x0 as u16);
    if skewed {
        let q = skew_half(m, t, 0x800d_8578, 0x800d_8590);
        let y0 = y(m).wrapping_add(q);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0xa), y0 as u16);
        let x1 = right(m);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x10), x1 as u16);
        let q = skew_half(m, t, 0x800d_860c, 0x800d_8624);
        let y1 = y(m).wrapping_add(q);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x12), y1 as u16);
        let prim = m.lw(off(t, 8));
        let x2 = x(m);
        m.sh(off(prim, 0x18), x2 as u16);
        let y2 = skew_full(m, t, [0x800d_8698, 0x800d_86b0, 0x800d_86ec, 0x800d_8704]);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x1a), y2 as u16);
        let x3 = right(m);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x20), x3 as u16);
        let y3 = skew_full(m, t, [0x800d_8788, 0x800d_87a0, 0x800d_87dc, 0x800d_87f4]);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x22), y3 as u16);
    } else {
        let prim = m.lw(off(t, 8));
        let y0 = y(m);
        m.sh(off(prim, 0xa), y0 as u16);
        let x1 = right(m);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x10), x1 as u16);
        let prim = m.lw(off(t, 8));
        let y1 = y(m);
        m.sh(off(prim, 0x12), y1 as u16);
        let prim = m.lw(off(t, 8));
        let x2 = x(m);
        m.sh(off(prim, 0x18), x2 as u16);
        let lower = |m: &mut Machine| {
            let h = lbu(m, off(t, 0x39)).wrapping_sub(1);
            let s = lh(m, off(t, 0x2e));
            y(m).wrapping_add(mul(h, s))
        };
        let y2 = lower(m);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x1a), y2 as u16);
        let x3 = right(m);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x20), x3 as u16);
        let y3 = lower(m);
        let prim = m.lw(off(t, 8));
        m.sh(off(prim, 0x22), y3 as u16);
    }
    let t = text(m);
    let prim = m.lw(off(t, 8));
    let ot = m.lw(off(t, 0x1c));
    let tag = m.lw(prim);
    let next = m.lw(ot);
    m.sw(prim, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let ot = m.lw(off(t, 0x1c));
    let prim = m.lw(off(t, 8));
    let entry = m.lw(ot);
    m.sw(ot, (entry & 0xff00_0000) | (prim & 0x00ff_ffff));
    let advance = if lbu(m, off(t, 0x3b)) != 0 {
        let table = m.lw(off(t, 0x3c));
        lbu(m, off(table, index & 0xff))
    } else {
        lbu(m, off(t, 0x38))
    };
    let x = lhu(m, off(t, 0x20)).wrapping_add(advance);
    m.sh(off(t, 0x20), x as u16);
    let t = text(m);
    let prim = m.lw(off(t, 8));
    m.sw(off(t, 8), prim.wrapping_add(0x28));
}

/// 800D8980: print the +0x10 characters of `string` (10 a new line, 9 the
/// line start, 32 a space of the font's width).
pub fn print_string(m: &mut Machine, string: u32) {
    m.push_frame(0x20);
    let mut i = 0u32;
    while slt(i, text_length(m)) {
        let at = string.wrapping_add(i);
        match lbu(m, at) {
            0xa => {
                let t = text(m);
                let h = lbu(m, off(t, 0x39));
                let s = lh(m, off(t, 0x2e));
                let start = lhu(m, off(t, 0x24));
                let y = lhu(m, off(t, 0x22));
                m.sh(off(t, 0x20), start as u16);
                m.sh(off(t, 0x22), y.wrapping_add(mul(h, s)) as u16);
            }
            9 => {
                let t = text(m);
                let start = lhu(m, off(t, 0x24));
                m.sh(off(t, 0x20), start as u16);
            }
            0x20 => {
                let t = text(m);
                let advance = if lbu(m, off(t, 0x3b)) != 0 {
                    lbu(m, off(t, 0x42))
                } else {
                    lbu(m, off(t, 0x38))
                };
                let x = lhu(m, off(t, 0x20)).wrapping_add(advance);
                m.sh(off(t, 0x20), x as u16);
            }
            ch => draw_glyph(m, ch),
        }
        i = i.wrapping_add(1);
    }
    m.pop_frame(0x20);
}

/// 800D8AA0: printf to the text: format `format` and its arguments (a1..
/// and the caller's stack words) with vsprintf (8002877C) into a 0x200-byte
/// buffer, then [`print_string`] its length.
pub fn print(m: &mut Machine, args: [u32; 4]) {
    let home = m.sp;
    for (i, &arg) in args.iter().enumerate() {
        m.sw(off(home, 4 * i as u32), arg);
    }
    let frame = m.push_frame(0x218);
    m.sw(off(frame, 0x218), args[0]);
    let length = call(
        m,
        VSPRINTF,
        [off(frame, 0x10), args[0], off(frame, 0x21c), 0],
    );
    let t = text(m);
    m.sw(off(t, 0x10), length);
    print_string(m, off(frame, 0x10));
    m.pop_frame(0x218);
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !loaded(m) {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_7644 => {
            init_glyphs(m, a0, a1);
            0
        }
        0x800d_783c => glyph_address(m, a0),
        0x800d_7868 => {
            upload_glyphs(m, a0);
            0
        }
        0x800d_7a58 => {
            draw_glyphs(m, a0, a1, a2, a3);
            0
        }
        0x800d_7c18 => {
            select_glyph_area(m);
            0
        }
        0x800d_7c40 => {
            set_position(m, a0, a1);
            0
        }
        0x800d_7c60 => {
            set_color(m, a0, a1, a2);
            0
        }
        0x800d_7c88 => {
            set_blend(m, a0);
            0
        }
        0x800d_7cd4 => {
            set_line(m, a0);
            0
        }
        0x800d_7ce4 => {
            set_skew(m, a0, a1, a2);
            0
        }
        0x800d_7d08 => {
            set_font(m, a0);
            0
        }
        0x800d_8094 => {
            get_position(m, a0, a1);
            0
        }
        0x800d_80c0 => {
            init_text(m);
            0
        }
        0x800d_81f0 => {
            begin_text(m, a0);
            0
        }
        0x800d_82a4 => {
            draw_glyph(m, a0);
            0
        }
        0x800d_8980 => {
            print_string(m, a0);
            0
        }
        0x800d_8aa0 => {
            print(m, args);
            0
        }
        _ => return None,
    })
}
