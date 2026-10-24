//! Subsystem initialization tree under 80039A68 (called by gameplay
//! initialization 80032510).
//!
//! 80039A68 resets a few state blocks and builds six double-buffered GPU
//! packet sets in the arena: one buffer per frame buffer, a pointer table in
//! one small-data slot and the last buffer built in another. The libgpu
//! packet helpers it reaches (`SetDrawArea`, `CatPrim`, `SetDrawTPage`) only
//! write RAM and are translated here; nothing in this tree touches a device.
//!
//! Every routine follows its original instructions and store order. The
//! original reloads the current buffer pointer from its gp slot before most
//! stores; the stores in between only reach arena buffers, so the
//! translation keeps the pointer in a local. Primitive comments use libgpu
//! terms: length byte at +3, command code at +7 (`| 2` is semi-transparency).
use crate::{
    arena,
    common::CONTEXT,
    dispatch,
    machine::{GP, Machine, SmallData},
    sdk,
};

/// 800C4B28: state block reset first by 80039A68.
const STATE_4B28: u32 = 0x800c_4b28;
/// 800C4C90: word set to 0xC80 by 80039A68.
const WORD_4C90: u32 = 0x800c_4c90;
/// `[gp+0x4C0]` (800BAEB4): byte value 0x1E selects [`copy_name_upper`].
const NAME_MODE: u32 = 0x800b_aeb4;
/// 800C4F18: 16-byte uppercase copy of the context name (context +0x94C).
const NAME_UPPER: u32 = 0x800c_4f18;
/// 800C4568: 64 record pointers cleared by [`clear_records_4568`].
const RECORDS_4568: u32 = 0x800c_4568;
/// gp slots of the packet sets: pointer table, then current buffer.
const SET_508_TABLE: u32 = 0x508;
const SET_508_BUFFER: u32 = 0x50c;
const SET_4F4_TABLE: u32 = 0x4f4;
const SET_424_TABLE: u32 = 0x424;
const SET_424_BUFFER: u32 = 0x428;
const SET_434_TABLE: u32 = 0x434;
const SET_434_BUFFER: u32 = 0x438;
const SET_440_TABLE: u32 = 0x440;
const SET_440_BUFFER: u32 = 0x444;
const SET_45C_TABLE: u32 = 0x45c;
const SET_45C_BUFFER: u32 = 0x460;

/// Byte tables of 8003B180 in the small-data area.
const QUAD_X: u32 = GP + 0x1a0;
const QUAD_Y: u32 = GP + 0x1a8;
const QUAD_W: u32 = GP + 0x1b0;
const QUAD_H: u32 = GP + 0x1b8;
const ICON_X: u32 = GP + 0x1c0;
const ICON_Y: u32 = GP + 0x1c4;
/// Halfword x/y and byte u tables of the four 8003B180 sprites.
const SPRITE_X: u32 = GP + 0x1c8;
const SPRITE_Y: u32 = GP + 0x1d0;
const SPRITE_U: u32 = GP + 0x1d8;
/// 8003BA98 tables: four (x, _, y, _) halfword quads, four u and four v
/// byte quads.
const TILE_XY: u32 = 0x800a_fa94;
const TILE_U: u32 = 0x800a_fab4;
const TILE_V: u32 = 0x800a_fac4;

/// Polyline terminator word.
const LINE_END: u32 = 0x5555_5555;

fn put8(m: &mut Machine, base: u32, offset: u32, value: u8) {
    m.sb(base.wrapping_add(offset), value);
}
fn put16(m: &mut Machine, base: u32, offset: u32, value: u16) {
    m.sh(base.wrapping_add(offset), value);
}
fn put32(m: &mut Machine, base: u32, offset: u32, value: u32) {
    m.sw(base.wrapping_add(offset), value);
}
fn get8(m: &mut Machine, base: u32, offset: u32) -> u8 {
    m.lbu(base.wrapping_add(offset))
}
fn get32(m: &mut Machine, base: u32, offset: u32) -> u32 {
    m.lw(base.wrapping_add(offset))
}
/// `lbu/ori 2/sb` on the code byte: libgpu `SetSemiTrans(p, 1)`.
fn semi_trans(m: &mut Machine, prim: u32) {
    let code = get8(m, prim, 7);
    put8(m, prim, 7, code | 2);
}
/// Element `index` of an array at `base + first` with `stride`.
fn element(base: u32, first: u32, stride: u32, index: u32) -> u32 {
    base.wrapping_add(first)
        .wrapping_add(stride.wrapping_mul(index))
}

/// 80039A68: reset the 800C4B28 state block, build the six packet sets in
/// original order, then (when `[gp+0x4C0]` is 0x1E) copy the context name in
/// upper case.
pub fn initialize_39a68(m: &mut Machine) {
    m.sw(STATE_4B28, 0);
    m.sb(STATE_4B28 + 0xe, 0xff);
    m.sb(STATE_4B28 + 0x12, 0xff);
    m.sb(STATE_4B28 + 0x13, 0xff);
    m.sb(STATE_4B28 + 0xf, 0);
    m.sh(STATE_4B28 + 6, 0);
    m.sw(WORD_4C90, 0xc80);
    packets_508(m);
    packets_4f4(m);
    packets_424(m);
    packets_434(m);
    packets_440(m);
    packets_45c(m);
    if m.lbu(NAME_MODE) == 0x1e {
        copy_name_upper(m);
    }
}

/// 80039B30: copy the 16 bytes at context +0x94C to 800C4F18, mapping
/// `a..z` to upper case.
pub fn copy_name_upper(m: &mut Machine) {
    for i in 0..16 {
        let context = m.gw(CONTEXT);
        let mut c = u32::from(get8(m, context.wrapping_add(i), 0x94c));
        if c.wrapping_sub(0x61) < 0x1a {
            c -= 0x20;
        }
        m.sb(NAME_UPPER + i, c as u8);
    }
}

/// 80062324: packet set `[gp+0x508]`, two 0x9D4-byte buffers (the last in
/// `[gp+0x50C]`).
pub fn packets_508(m: &mut Machine) {
    let table = arena::alloc(m, 0x28);
    m.set_gw(SET_508_TABLE, table);
    for index in 0..2 {
        let buf = arena::alloc(m, 0x9d4);
        let table = m.gw(SET_508_TABLE);
        m.set_gw(SET_508_BUFFER, buf);
        put32(m, table, index * 4, buf);
        // 16 POLY_F4 (len 5, code 0x2A) from +0x008.
        for i in 0..16 {
            let p = element(buf, 0x8, 0x18, i);
            put8(m, p, 3, 5);
            put8(m, p, 7, 0x2a);
        }
        // 4 POLY_G3 (len 6, code 0x32) from +0x188.
        for i in 0..4 {
            let p = element(buf, 0x188, 0x1c, i);
            put8(m, p, 3, 6);
            put8(m, p, 7, 0x32);
        }
        // 12 POLY_FT4 (len 9, code 0x2E) from +0x1F8.
        for i in 0..12 {
            let p = element(buf, 0x1f8, 0x28, i);
            put8(m, p, 3, 9);
            put8(m, p, 7, 0x2e);
        }
        // 64 LINE_F2 (len 3, code 0x40, semi) from +0x3D8.
        for i in 0..64 {
            let p = element(buf, 0x3d8, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x40);
            semi_trans(m, p);
        }
        // 8 LINE_G2 (len 4, code 0x50, semi) from +0x7D8.
        for i in 0..8 {
            let p = element(buf, 0x7d8, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x50);
            semi_trans(m, p);
        }
        // One LINE_G4 (len 9, code 0x5C, semi) at +0x878 (a one-pass loop).
        let p = buf.wrapping_add(0x878);
        put8(m, p, 3, 9);
        put8(m, p, 7, 0x5c);
        put8(m, p, 0x17, 0);
        put32(m, p, 0x24, LINE_END);
        put8(m, p, 0x1f, 0);
        semi_trans(m, p);
        // 4 POLY_FT3 (len 7, code 0x24, semi) from +0x954, linked in order.
        for i in 0..4 {
            let p = element(buf, 0x954, 0x20, i);
            put8(m, p, 3, 7);
            put8(m, p, 7, 0x24);
            let code = get8(m, p, 7);
            put8(m, p, 4, 0x43);
            put8(m, p, 5, 0x13);
            put16(m, p, 0x16, 0x36);
            put16(m, p, 0xe, 0x7f98);
            put8(m, p, 0xc, 0x8a);
            put8(m, p, 0x1d, 0x35);
            put8(m, p, 6, 0);
            put8(m, p, 0xd, 0x21);
            put8(m, p, 0x14, 0x9d);
            put8(m, p, 0x15, 0x21);
            put8(m, p, 0x1c, 0x9d);
            put8(m, p, 7, code | 2);
            if i != 3 {
                sdk::cat_prim(m, p, p.wrapping_add(0x20));
            }
        }
        // 8 SPRT (len 4, code 0x66, clut 0x7F98) from +0x8A0.
        for i in 0..8 {
            let p = element(buf, 0x8a0, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x66);
            put16(m, p, 0xe, 0x7f98);
        }
        // SPRT (code 0x64, semi) at +0x940: uv (EC, 8C), 20x20.
        let p = buf.wrapping_add(0x940);
        put8(m, p, 3, 4);
        put8(m, p, 7, 0x64);
        put8(m, p, 0xc, 0xec);
        put8(m, p, 0xd, 0x8c);
        let code = get8(m, p, 7);
        put16(m, p, 0x12, 0x14);
        put16(m, p, 0x10, 0x14);
        put8(m, p, 7, code | 2);
        sdk::set_draw_tpage(m, buf, 0, 1, 0x36);
    }
}

/// 8005CB98: packet set `[gp+0x4F4]`, two 0x424-byte blocks built by
/// [`packet_block_4f4`].
pub fn packets_4f4(m: &mut Machine) {
    let table = arena::alloc(m, 8);
    m.set_gw(SET_4F4_TABLE, table);
    for index in 0..2 {
        let block = arena::alloc(m, 0x424);
        let table = m.gw(SET_4F4_TABLE);
        put32(m, table, index * 4, block);
        packet_block_4f4(m, block);
    }
}

/// 8005C9E4: one 0x424-byte block of set `[gp+0x4F4]`: seven primitive
/// groups and a DR_TPAGE (0x36, dither) at +0.
pub fn packet_block_4f4(m: &mut Machine, block: u32) {
    block_4f4_008(m, block.wrapping_add(0x8));
    block_4f4_09c(m, block.wrapping_add(0x9c));
    block_4f4_1b8(m, block.wrapping_add(0x1b8));
    block_4f4_30c(m, block.wrapping_add(0x30c));
    block_4f4_244(m, block.wrapping_add(0x244));
    block_4f4_384(m, block.wrapping_add(0x384));
    block_4f4_3dc(m, block.wrapping_add(0x3dc));
    sdk::set_draw_tpage(m, block, 0, 1, 0x36);
}

/// 8005AEB8 (block +0x008): five SPRT from +0, then two POLY_F4 (code 0x2A)
/// at +0x64/+0x7C; +0x64 links to +0x7C and +0x7C back to +0.
pub fn block_4f4_008(m: &mut Machine, p: u32) {
    for i in 0..2 {
        let q = element(p, 0x64, 0x18, i);
        put8(m, q, 3, 5);
        put8(m, q, 7, 0x2a);
    }
    for i in 0..5 {
        let q = element(p, 0, 0x14, i);
        put8(m, q, 3, 4);
        put16(m, q, 0xe, 0x7f98);
        put8(m, q, 7, 0x66);
    }
    put8(m, p, 0xd, 0x19);
    put16(m, p, 0x10, 0x44);
    put16(m, p, 0x12, 0x14);
    put16(m, p, 8, 0xa);
    put16(m, p, 0xa, 0x88);
    let first = get32(m, p, 0x64);
    let second = get32(m, p, 0x7c);
    put8(m, p, 0xc, 0);
    let next = p.wrapping_add(0x7c) & 0x00ff_ffff;
    put32(m, p, 0x64, (first & 0xff00_0000) | next);
    put32(m, p, 0x7c, (second & 0xff00_0000) | (p & 0x00ff_ffff));
}

/// 8005B298 (block +0x09C): thirteen SPRT. +0 links to +0x14; the five from
/// +0x28 (6x10 at x 0x12E) link in a chain; six bare ones from +0x8C.
pub fn block_4f4_09c(m: &mut Machine, p: u32) {
    put8(m, p, 0xc, 0x82);
    put16(m, p, 0x10, 0x36);
    put16(m, p, 0x12, 0x10);
    put16(m, p, 8, 0xef);
    put8(m, p, 3, 4);
    put16(m, p, 0xe, 0x7f98);
    put8(m, p, 0xd, 0xf);
    put16(m, p, 0xa, 0x68);
    put8(m, p, 7, 0x66);
    sdk::cat_prim(m, p, p.wrapping_add(0x14));
    let q = p.wrapping_add(0x14);
    put8(m, q, 0xc, 0xb8);
    put16(m, q, 0x10, 0x13);
    put16(m, q, 0x12, 0x29);
    put8(m, q, 3, 4);
    put16(m, q, 0xe, 0x7f98);
    put8(m, q, 0xd, 0xf);
    put16(m, q, 8, 0x125);
    put16(m, q, 0xa, 0x68);
    put8(m, q, 7, 0x66);
    for i in 2..7 {
        let q = element(p, 0x28, 0x14, i - 2);
        put8(m, q, 7, 0x66);
        put8(m, q, 3, 4);
        put16(m, q, 0xe, 0x7f98);
        put8(m, q, 0xc, 0x24);
        put8(m, q, 0xd, 0x2d);
        put16(m, q, 0x10, 6);
        put16(m, q, 0x12, 0xa);
        put16(m, q, 8, 0x12e);
        if i < 6 {
            sdk::cat_prim(m, q, q.wrapping_add(0x14));
        }
    }
    for i in 0..6 {
        let q = element(p, 0x8c, 0x14, i);
        put8(m, q, 3, 4);
        put16(m, q, 0xe, 0x7f98);
        put8(m, q, 7, 0x66);
    }
}

/// 8005B7E4 (block +0x1B8): seven SPRT, the first six linked in order.
pub fn block_4f4_1b8(m: &mut Machine, p: u32) {
    put8(m, p, 0xc, 0x44);
    put8(m, p, 0xd, 0x19);
    put16(m, p, 0x10, 0x3c);
    put16(m, p, 8, 0xc);
    put8(m, p, 3, 4);
    put16(m, p, 0xe, 0x7f98);
    put16(m, p, 0x12, 0xe);
    put16(m, p, 0xa, 0xe2);
    put8(m, p, 7, 0x66);
    let q = p.wrapping_add(0x14);
    put8(m, q, 0xc, 0x32);
    put8(m, q, 0xd, 0x31);
    put16(m, q, 0x10, 5);
    put16(m, q, 0x12, 6);
    put8(m, q, 3, 4);
    put16(m, q, 0xe, 0x7f98);
    put16(m, q, 8, 0xe);
    put16(m, q, 0xa, 0xe6);
    put8(m, q, 7, 0x66);
    for i in 0..5 {
        let q = element(p, 0x28, 0x14, i);
        put8(m, q, 3, 4);
        put16(m, q, 0xe, 0x7f98);
        put8(m, q, 7, 0x66);
    }
    for i in 0..6 {
        let q = element(p, 0, 0x14, i);
        sdk::cat_prim(m, q, q.wrapping_add(0x14));
    }
}

/// 8005BC1C (block +0x30C): six SPRT; +0 links to +0x14.
pub fn block_4f4_30c(m: &mut Machine, p: u32) {
    put16(m, p, 0xe, 0x7f98);
    put8(m, p, 0xc, 0xdc);
    put16(m, p, 0x10, 0x24);
    put16(m, p, 0x12, 0x11);
    put16(m, p, 8, 0xee);
    put8(m, p, 3, 4);
    put16(m, p, 0xa, 0xe6);
    let tag = get32(m, p, 0);
    put8(m, p, 7, 0x66);
    put8(m, p, 0xd, 0);
    let next = p.wrapping_add(0x14) & 0x00ff_ffff;
    put32(m, p, 0, (tag & 0xff00_0000) | next);
    let q = p.wrapping_add(0x14);
    put16(m, q, 0xe, 0x7e18);
    put8(m, q, 0xd, 0xa0);
    put16(m, q, 0x10, 0x25);
    put16(m, q, 0x12, 0x30);
    put16(m, q, 8, 0x112);
    put8(m, q, 3, 4);
    put8(m, q, 7, 0x66);
    put8(m, q, 0xc, 0xd8);
    put16(m, q, 0xa, 0xca);
    let q = p.wrapping_add(0x28);
    put16(m, q, 0xe, 0x7f5b);
    put8(m, q, 0xd, 0xd0);
    put16(m, q, 0x10, 0x28);
    put16(m, q, 0x12, 0x28);
    put16(m, q, 8, 0x107);
    put8(m, q, 3, 4);
    put8(m, q, 7, 0x66);
    put8(m, q, 0xc, 0xd8);
    put16(m, q, 0xa, 0xc0);
    for i in 0..3 {
        let q = element(p, 0x3c, 0x14, i);
        put8(m, q, 3, 4);
        put8(m, q, 7, 0x66);
        put16(m, q, 0xe, 0x7f98);
    }
}

/// One 5x7 SPRT of 8005C11C at `q`: uv (0x38, 0x30), position (`x`, `y`).
fn small_sprite(m: &mut Machine, q: u32, x: u16, y: u16) {
    put8(m, q, 3, 4);
    put8(m, q, 7, 0x66);
    put16(m, q, 0xe, 0x7f98);
    put8(m, q, 0xc, 0x38);
    put8(m, q, 0xd, 0x30);
    put16(m, q, 0x10, 5);
    put16(m, q, 0x12, 7);
    put16(m, q, 8, x);
    put16(m, q, 0xa, y);
}

/// 8005C11C (block +0x244): ten SPRT: one at +0, four 5x7 at y 0xCA (x from
/// 0xF2, step 5), two at y 0xC1 (x from 0xFC), three bare ones.
pub fn block_4f4_244(m: &mut Machine, p: u32) {
    put8(m, p, 3, 4);
    put8(m, p, 7, 0x66);
    put16(m, p, 0xe, 0x7f98);
    put8(m, p, 0xc, 0x9e);
    put8(m, p, 0xd, 0x21);
    put16(m, p, 0x10, 0x19);
    put16(m, p, 0x12, 0x14);
    put16(m, p, 8, 0xee);
    put16(m, p, 0xa, 0xd2);
    let mut x = 0xf2;
    for i in 0..4 {
        small_sprite(m, element(p, 0x14, 0x14, i), x, 0xca);
        x += 5;
    }
    let mut x = 0xfc;
    for i in 0..2 {
        small_sprite(m, element(p, 0x64, 0x14, i), x, 0xc1);
        x += 5;
    }
    for i in 0..3 {
        let q = element(p, 0x8c, 0x14, i);
        put8(m, q, 3, 4);
        put8(m, q, 7, 0x66);
        put16(m, q, 0xe, 0x7f98);
    }
}

/// One LINE_F2 (len 3, code 0x42) from (`x0`, `y0`) to (`x1`, `y1`).
fn line(m: &mut Machine, q: u32, x0: u16, y0: u16, x1: u16, y1: u16) {
    put8(m, q, 3, 3);
    put16(m, q, 8, x0);
    put16(m, q, 0xa, y0);
    put16(m, q, 0xc, x1);
    put16(m, q, 0xe, y1);
    put8(m, q, 7, 0x42);
}

/// 8005C580 (block +0x384): two SPRT (clut 0x7F18) and three LINE_F2,
/// linked in order.
pub fn block_4f4_384(m: &mut Machine, p: u32) {
    put8(m, p, 0xc, 0xda);
    put8(m, p, 0xd, 0x11);
    put16(m, p, 0x10, 0x26);
    put16(m, p, 0x12, 0x27);
    put8(m, p, 7, 0x66);
    put8(m, p, 3, 4);
    put16(m, p, 0xe, 0x7f18);
    put16(m, p, 8, 0xe6);
    put16(m, p, 0xa, 0xa4);
    let q = p.wrapping_add(0x14);
    put8(m, q, 0xc, 0x2a);
    put8(m, q, 0xd, 0x2f);
    put16(m, q, 0x10, 8);
    put16(m, q, 0x12, 8);
    put16(m, q, 8, 0xdf);
    put16(m, q, 0xe, 0x7f18);
    put8(m, q, 3, 4);
    put16(m, q, 0xa, 0xef);
    put8(m, q, 7, 0x66);
    line(m, p.wrapping_add(0x28), 0, 0xf6, 0xde, 0xf6);
    line(m, p.wrapping_add(0x38), 0xe6, 0xee, 0xe6, 0xcb);
    line(m, p.wrapping_add(0x48), 0x10c, 0xa4, 0x140, 0xa4);
    sdk::cat_prim(m, p, p.wrapping_add(0x14));
    sdk::cat_prim(m, p.wrapping_add(0x14), p.wrapping_add(0x28));
    sdk::cat_prim(m, p.wrapping_add(0x28), p.wrapping_add(0x38));
    sdk::cat_prim(m, p.wrapping_add(0x38), p.wrapping_add(0x48));
}

/// 8005C7D4 (block +0x3DC): two SPRT and two LINE_F2, linked +0x28, +0x38,
/// +0, +0x14.
pub fn block_4f4_3dc(m: &mut Machine, p: u32) {
    let q = p.wrapping_add(0x28);
    put8(m, q, 3, 3);
    put8(m, q, 7, 0x42);
    put16(m, q, 8, 0);
    put16(m, q, 0xa, 0x2b);
    put16(m, q, 0xc, 4);
    put16(m, q, 0xe, 0x2b);
    let q = p.wrapping_add(0x38);
    put8(m, q, 3, 3);
    put16(m, q, 8, 0x55);
    put16(m, q, 0xc, 0x140);
    put8(m, q, 7, 0x42);
    put16(m, q, 0xa, 0xc);
    put16(m, q, 0xe, 0xc);
    put16(m, p, 8, 5);
    put8(m, p, 0xd, 0x37);
    put16(m, p, 0x10, 0x50);
    put16(m, p, 0x12, 0x49);
    put8(m, p, 3, 4);
    put8(m, p, 7, 0x66);
    put16(m, p, 0xe, 0x7f98);
    put16(m, p, 0xa, 0xa);
    put8(m, p, 0xc, 0);
    let q = p.wrapping_add(0x14);
    put16(m, q, 8, 9);
    put8(m, q, 0xc, 0x18);
    put8(m, q, 3, 4);
    put8(m, q, 7, 0x66);
    put16(m, q, 0xe, 0x7f98);
    put16(m, q, 0xa, 0xa);
    put8(m, q, 0xd, 0x2d);
    put16(m, q, 0x10, 0xc);
    put16(m, q, 0x12, 0xa);
    sdk::cat_prim(m, p.wrapping_add(0x28), p.wrapping_add(0x38));
    sdk::cat_prim(m, p.wrapping_add(0x38), p);
    sdk::cat_prim(m, p, p.wrapping_add(0x14));
}

/// Texture coordinates of the four 16x16 quads at 8003B180 +0xDEC: the same
/// cell mirrored horizontally and vertically.
const ICON_UV: [[(u8, u8); 4]; 4] = [
    [(0x70, 0x37), (0x80, 0x37), (0x70, 0x47), (0x80, 0x47)],
    [(0x7f, 0x37), (0x6f, 0x37), (0x7f, 0x47), (0x6f, 0x47)],
    [(0x70, 0x46), (0x80, 0x46), (0x70, 0x36), (0x80, 0x36)],
    [(0x7f, 0x46), (0x6f, 0x46), (0x7f, 0x36), (0x6f, 0x36)],
];
/// 8003B180 halfword coordinates of the four LINE_F3 at +0xECC, in the
/// original store order.
const FRAME_LINES: [(u32, u16); 24] = [
    (0xed6, 0x60),
    (0xeda, 0x60),
    (0xeee, 0x60),
    (0xef2, 0x60),
    (0xed8, 0x80),
    (0xedc, 0x80),
    (0xf08, 0x80),
    (0xf0c, 0x80),
    (0xed4, 0x88),
    (0xede, 0x68),
    (0xeec, 0xb8),
    (0xef0, 0xc0),
    (0xef4, 0xc0),
    (0xef6, 0x68),
    (0xf04, 0x88),
    (0xf06, 0xa0),
    (0xf0a, 0xa0),
    (0xf0e, 0x98),
    (0xf1c, 0xb8),
    (0xf1e, 0xa0),
    (0xf20, 0xc0),
    (0xf22, 0xa0),
    (0xf24, 0xc0),
    (0xf26, 0x98),
];
/// 8003B180 halfword coordinates of the ten LINE_F2 at +0xF2C, in the
/// original store order.
const SCALE_LINES: [(u32, u16); 40] = [
    (0xf38, 0xa4),
    (0xf48, 0xa4),
    (0xf58, 0xa4),
    (0xf74, 0xbc),
    (0xf86, 0x7c),
    (0xf96, 0x7c),
    (0xfa6, 0x7c),
    (0xf34, 0x9c),
    (0xf36, 0x60),
    (0xf3a, 0x60),
    (0xf44, 0x9c),
    (0xf46, 0x80),
    (0xf4a, 0x80),
    (0xf54, 0x9c),
    (0xf56, 0xa0),
    (0xf5a, 0xa0),
    (0xf64, 0x80),
    (0xf66, 0x80),
    (0xf68, 0x84),
    (0xf6a, 0x80),
    (0xf76, 0x80),
    (0xf78, 0xc0),
    (0xf7a, 0x80),
    (0xf84, 0x80),
    (0xf88, 0x80),
    (0xf8a, 0x84),
    (0xf94, 0xa0),
    (0xf98, 0xa0),
    (0xf9a, 0x84),
    (0xfa4, 0xc0),
    (0xfa8, 0xc0),
    (0xfaa, 0x84),
    (0xfb4, 0xa0),
    (0xfb6, 0x60),
    (0xfb8, 0xa0),
    (0xfba, 0x64),
    (0xfc4, 0xa0),
    (0xfc6, 0xa0),
    (0xfc8, 0xa0),
    (0xfca, 0x9c),
];
/// DR_TPAGE command words (libgpu `getTPage` results with draw-mode bits).
const TPAGE_205: u32 = 0xe100_0205;
const TPAGE_236: u32 = 0xe100_0236;
const TPAGE_616: u32 = 0xe100_0616;
const TPAGE_636: u32 = 0xe100_0636;

/// 8003B180: packet set `[gp+0x424]`, two 0x1080-byte buffers (the last in
/// `[gp+0x428]`): two DR_AREA (a 64x64 window at (12, 12) or (12, 268) and
/// the full 320x256 frame buffer), sprites, tiles, lines and textured quads
/// placed from the gp+0x1A0.. tables. Ends with [`packets_424_quads`]. The
/// RECT passed to [`sdk::set_draw_area`] lives in the original 0x30-byte frame on
/// the guest stack (`sp+0x10`).
pub fn packets_424(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    let table = arena::alloc(m, 0x1c);
    m.set_gw(SET_424_TABLE, table);
    for index in 0..2 {
        let buf = arena::alloc(m, 0x1080);
        let table = m.gw(SET_424_TABLE);
        m.set_gw(SET_424_BUFFER, buf);
        put32(m, table, index * 4, buf);
        // 64 SPRT (code 0x66) from +0x18.
        for i in 0..64 {
            let p = element(buf, 0x18, 0x14, i);
            put8(m, p, 3, 4);
            put16(m, p, 0xe, 0x7f98);
            put8(m, p, 7, 0x66);
        }
        // 64 TILE (len 3, code 0x62) from +0x518.
        for i in 0..64 {
            let p = element(buf, 0x518, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x62);
        }
        // 8 LINE_F2 (code 0x42, colour 0/80/0) from +0x918.
        for i in 0..8 {
            let p = element(buf, 0x918, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0x80);
            put8(m, p, 6, 0);
            put8(m, p, 7, 0x42);
        }
        // 8 LINE_F3 (len 5, code 0x4A, colour 0/80/0) from +0x998.
        for i in 0..8 {
            let p = element(buf, 0x998, 0x18, i);
            put8(m, p, 3, 5);
            put32(m, p, 0x14, LINE_END);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0x80);
            put8(m, p, 6, 0);
            put8(m, p, 7, 0x4a);
        }
        // RECT local at sp+0x10 (x, y, w, h), passed by address.
        let rect = frame + 0x10;
        m.sh(rect, 0xc);
        m.sh(rect + 2, 0xc);
        m.sh(rect + 4, 0x40);
        m.sh(rect + 6, 0x40);
        if index == 1 {
            m.sh(rect + 2, 0x10c);
        }
        sdk::set_draw_area(m, buf, rect);
        m.sh(rect + 4, 0x140);
        m.sh(rect, 0);
        m.sh(rect + 2, 0);
        m.sh(rect + 6, 0x100);
        if index == 1 {
            m.sh(rect + 2, 0x100);
        }
        sdk::set_draw_area(m, buf.wrapping_add(0xc), rect);
        // 2 LINE_F2 (code 0x42) at +0xC54.
        for i in 0..2 {
            let p = element(buf, 0xc54, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x42);
        }
        // 3 POLY_F3 (len 4, code 0x20, semi) from +0xC74.
        for i in 0..3 {
            let p = element(buf, 0xc74, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x20);
            semi_trans(m, p);
        }
        // 7 POLY_FT4 (code 0x2E, clut 0x5714, tpage 5) from +0xCD4.
        for i in 0..7 {
            let p = element(buf, 0xcd4, 0x28, i);
            put16(m, p, 0x16, 5);
            put8(m, p, 3, 9);
            put8(m, p, 7, 0x2e);
            put8(m, p, 4, 0x83);
            put8(m, p, 5, 0x83);
            put8(m, p, 6, 0x83);
            put16(m, p, 0xe, 0x5714);
            let x = u16::from(m.lbu(QUAD_X + i));
            let y = u16::from(m.lbu(QUAD_Y + i));
            let w = u16::from(m.lbu(QUAD_W + i));
            let h = u16::from(m.lbu(QUAD_H + i));
            put16(m, p, 8, x);
            put16(m, p, 0xa, y);
            put16(m, p, 0x10, x + w);
            put16(m, p, 0x12, y);
            put16(m, p, 0x18, x);
            put16(m, p, 0x1a, y + h);
            put16(m, p, 0x20, x + w);
            put16(m, p, 0x22, y + h);
        }
        // 4 POLY_FT4 16x16 (clut 0x7F99, tpage 0x36) from +0xDEC and a black
        // 16x16 TILE (code 0x62) from +0xE8C at each position.
        for i in 0..4 {
            let p = element(buf, 0xdec, 0x28, i);
            put8(m, p, 3, 9);
            put8(m, p, 7, 0x2e);
            put8(m, p, 4, 0x83);
            put8(m, p, 5, 0x83);
            put8(m, p, 6, 0x83);
            put16(m, p, 0x16, 0x36);
            put16(m, p, 0xe, 0x7f99);
            let x = u16::from(m.lbu(ICON_X + i));
            let y = u16::from(m.lbu(ICON_Y + i));
            put16(m, p, 8, x);
            put16(m, p, 0xa, y);
            put16(m, p, 0x10, x + 0x10);
            put16(m, p, 0x12, y);
            put16(m, p, 0x18, x);
            put16(m, p, 0x1a, y + 0x10);
            put16(m, p, 0x20, x + 0x10);
            put16(m, p, 0x22, y + 0x10);
            let t = element(buf, 0xe8c, 0x10, i);
            put8(m, t, 3, 3);
            put8(m, t, 7, 0x62);
            put8(m, t, 4, 0);
            put8(m, t, 5, 0);
            put8(m, t, 6, 0);
            put16(m, t, 8, x);
            put16(m, t, 0xc, 0x10);
            put16(m, t, 0xa, y);
            put16(m, t, 0xe, 0x10);
        }
        for (i, corners) in (0..).zip(ICON_UV) {
            let p = element(buf, 0xdec, 0x28, i);
            for (corner, (u, v)) in (0..).zip(corners) {
                put8(m, p, 0xc + corner * 8, u);
                put8(m, p, 0xd + corner * 8, v);
            }
        }
        // 4 LINE_F3 (code 0x4A, colour 0/FF/0) from +0xECC.
        for i in 0..4 {
            let p = element(buf, 0xecc, 0x18, i);
            put8(m, p, 7, 0x48);
            put8(m, p, 3, 5);
            put32(m, p, 0x14, LINE_END);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0xff);
            put8(m, p, 6, 0);
            put8(m, p, 7, 0x4a);
        }
        for (offset, value) in FRAME_LINES {
            put16(m, buf, offset, value);
        }
        // 10 LINE_F2 (code 0x42, colour 0/FF/0) from +0xF2C.
        for i in 0..10 {
            let p = element(buf, 0xf2c, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x42);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0xff);
            put8(m, p, 6, 0);
        }
        for (offset, value) in SCALE_LINES {
            put16(m, buf, offset, value);
        }
        // 2 POLY_F4 (len 5, code 0x2A, colour 10/20/10) from +0xFCC.
        for i in 0..2 {
            let p = element(buf, 0xfcc, 0x18, i);
            put8(m, p, 3, 5);
            put8(m, p, 7, 0x2a);
            put8(m, p, 4, 0x10);
            put8(m, p, 5, 0x20);
            put8(m, p, 6, 0x10);
        }
        // SPRT 8x9 at (0x9C, 0x7C), colour 83/83/83.
        let p = buf.wrapping_add(0xffc);
        put8(m, p, 7, 0x66);
        put8(m, p, 3, 4);
        put8(m, p, 4, 0x83);
        put8(m, p, 5, 0x83);
        put8(m, p, 6, 0x83);
        put16(m, p, 0xe, 0x7f98);
        put16(m, p, 8, 0x9c);
        put16(m, p, 0xa, 0x7c);
        put16(m, p, 0x10, 8);
        put16(m, p, 0x12, 9);
        // 4 SPRT 12x7 (colour 0/83/0) from +0x1010 placed from gp tables.
        for i in 0..4 {
            let p = element(buf, 0x1010, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x66);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0x83);
            put8(m, p, 6, 0);
            put16(m, p, 0xe, 0x7f98);
            let x = m.lhu(SPRITE_X + i * 2);
            put16(m, p, 8, x);
            let y = m.lhu(SPRITE_Y + i * 2);
            put16(m, p, 0xa, y);
            let u = m.lbu(SPRITE_U + i);
            put8(m, p, 0xd, 0x67);
            put16(m, p, 0x10, 0xc);
            put16(m, p, 0x12, 7);
            put8(m, p, 0xc, u);
        }
        // DR_TPAGE x4 from +0x1060 and three E6 (mask bit) command words.
        put8(m, buf, 0x1063, 1);
        put8(m, buf, 0x106b, 1);
        put32(m, buf, 0x1064, TPAGE_205);
        put8(m, buf, 0x1073, 1);
        put32(m, buf, 0x106c, TPAGE_236);
        put8(m, buf, 0x107b, 1);
        put32(m, buf, 0x1074, TPAGE_236);
        put32(m, buf, 0x107c, TPAGE_236);
        put32(m, buf, 0xcb4, 0xe600_0000);
        put32(m, buf, 0xcb8, 0);
        put32(m, buf, 0xcc0, 0xe600_0002);
        put32(m, buf, 0xcc4, 0);
        put32(m, buf, 0xccc, 0xe600_0000);
        put32(m, buf, 0xcd0, 0);
    }
    packets_424_quads(m);
    m.pop_frame(0x30);
}

/// 8003BA98: second pass over both `[gp+0x424]` buffers (leaving the last in
/// `[gp+0x428]`): textured quads from the 800AFA94 tables (+0xAA4, eight
/// chained by four from +0xACC), two sprites, two DR_TPAGE, two tiles and
/// five E6 words.
pub fn packets_424_quads(m: &mut Machine) {
    for index in 0..2 {
        let table = m.gw(SET_424_TABLE);
        let buf = get32(m, table, index * 4);
        m.set_gw(SET_424_BUFFER, buf);
        let p = buf.wrapping_add(0xaa4);
        put8(m, p, 7, 0x2e);
        put8(m, p, 4, 0x63);
        put8(m, p, 5, 0x63);
        put8(m, p, 6, 0x63);
        put16(m, p, 0x16, 5);
        put8(m, p, 3, 9);
        put16(m, p, 0xe, 0x5714);
        for i in 0..8 {
            let slot = i & 3;
            let p = element(buf, 0xacc, 0x28, i);
            put8(m, p, 7, 0x2e);
            put16(m, p, 0x16, 0x36);
            put8(m, p, 3, 9);
            put8(m, p, 4, 0x80);
            put8(m, p, 5, 0x80);
            put8(m, p, 6, 0x80);
            put16(m, p, 0xe, 0x7f99);
            let x = m.lhu(TILE_XY + slot * 8);
            let y = m.lhu(TILE_XY + slot * 8 + 4);
            put16(m, p, 8, x);
            put16(m, p, 0xa, y);
            put16(m, p, 0x10, x.wrapping_add(0x20));
            put16(m, p, 0x12, y);
            put16(m, p, 0x18, x);
            put16(m, p, 0x1a, y.wrapping_add(0x20));
            put16(m, p, 0x20, x.wrapping_add(0x20));
            put16(m, p, 0x22, y.wrapping_add(0x20));
            for corner in 0..4 {
                let u = m.lbu(TILE_U + slot * 4 + corner);
                put8(m, p, 0xc + corner * 8, u);
                let v = m.lbu(TILE_V + slot * 4 + corner);
                put8(m, p, 0xd + corner * 8, v);
            }
        }
        for i in 0..3 {
            let p = element(buf, 0xacc, 0x28, i);
            sdk::cat_prim(m, p, p.wrapping_add(0x28));
            let q = element(buf, 0xb6c, 0x28, i);
            sdk::cat_prim(m, q, q.wrapping_add(0x28));
        }
        let p = buf.wrapping_add(0xc0c);
        put16(m, p, 0xe, 0x7f18);
        put8(m, p, 0xd, 0x37);
        put16(m, p, 0x10, 0x50);
        put16(m, p, 0x12, 0x49);
        put8(m, p, 3, 4);
        put8(m, p, 0xc, 0);
        put16(m, p, 8, 6);
        put8(m, p, 7, 0x66);
        put16(m, p, 0xa, 0xa);
        let p = buf.wrapping_add(0xc20);
        put16(m, p, 0xe, 0x7f98);
        put8(m, p, 0xd, 0x2d);
        put8(m, p, 3, 4);
        put8(m, p, 0xc, 0);
        put16(m, p, 0x10, 0xc);
        put8(m, p, 7, 0x66);
        put16(m, p, 0x12, 0xa);
        put16(m, p, 8, 0xa);
        put16(m, p, 0xa, 0xa);
        put8(m, buf, 0xa97, 1);
        put8(m, buf, 0xa9f, 1);
        put32(m, buf, 0xa98, TPAGE_236);
        put32(m, buf, 0xaa0, TPAGE_236);
        for i in 0..2 {
            let p = element(buf, 0xc34, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x62);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0);
            put8(m, p, 6, 0);
            let x = m.lhu(TILE_XY);
            put16(m, p, 8, x);
            let y = m.lhu(TILE_XY + 4);
            put16(m, p, 0xc, 0x40);
            put16(m, p, 0xe, 0x40);
            put16(m, p, 0xa, y);
        }
        for (offset, value) in [
            (0xa5c, 0xe600_0000),
            (0xa60, 0),
            (0xa68, 0xe600_0002),
            (0xa6c, 0),
            (0xa74, 0xe600_0000),
            (0xa78, 0),
            (0xa80, 0xe600_0002),
            (0xa84, 0),
            (0xa8c, 0xe600_0000),
            (0xa90, 0),
        ] {
            put32(m, buf, offset, value);
        }
    }
}

/// 8003E4FC: packet set `[gp+0x434]`, two 0x1558-byte buffers (the last in
/// `[gp+0x438]`): 64 LINE_F2, 128 LINE_F3, 68 SPRT and a DR_TPAGE at +0;
/// then [`clear_records_4568`] and [`reset_gp_344`].
pub fn packets_434(m: &mut Machine) {
    let table = arena::alloc(m, 0x18);
    m.set_gw(SET_434_TABLE, table);
    for index in 0..2 {
        let buf = arena::alloc(m, 0x1558);
        let table = m.gw(SET_434_TABLE);
        m.set_gw(SET_434_BUFFER, buf);
        put32(m, table, index * 4, buf);
        // 64 LINE_F2 (code 0x40, semi, colour 0/FF/0) from +0x8.
        for i in 0..64 {
            let p = element(buf, 0x8, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x40);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0xff);
            put8(m, p, 6, 0);
            semi_trans(m, p);
        }
        // 128 LINE_F3 (code 0x48, semi, colour 0/FF/0) from +0x408.
        for i in 0..128 {
            let p = element(buf, 0x408, 0x18, i);
            put8(m, p, 3, 5);
            put8(m, p, 7, 0x48);
            put8(m, p, 4, 0);
            put32(m, p, 0x14, LINE_END);
            put8(m, p, 5, 0xff);
            put8(m, p, 6, 0);
            semi_trans(m, p);
        }
        // 64 SPRT (code 0x64, semi, clut 0x7F9B) from +0x1008.
        for i in 0..64 {
            let p = element(buf, 0x1008, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x64);
            put16(m, p, 0xe, 0x7f9b);
            semi_trans(m, p);
        }
        // 4 SPRT (code 0x64, semi, clut 0x7F98) from +0x1508.
        for i in 0..4 {
            let p = element(buf, 0x1508, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x64);
            put16(m, p, 0xe, 0x7f98);
            semi_trans(m, p);
        }
        put8(m, buf, 3, 1);
        put32(m, buf, 4, TPAGE_636);
    }
    clear_records_4568(m);
    reset_gp_344(m);
}

/// 8003EACC: for each non-null pointer of the 64 at 800C4568, clear bytes
/// +0/+1 and halfwords +6/+8/+A of its record.
pub fn clear_records_4568(m: &mut Machine) {
    for i in 0..64 {
        let record = m.lw(RECORDS_4568 + i * 4);
        if record != 0 {
            put8(m, record, 0, 0);
            put8(m, record, 1, 0);
            put16(m, record, 6, 0);
            put16(m, record, 8, 0);
            put16(m, record, 0xa, 0);
        }
    }
}

/// 800400F4: halfwords `gp+0x348` = 0xA0, `gp+0x34A` = 0x80, `gp+0x346` =
/// 0x40; bytes `gp+0x344/0x345` = 0.
pub fn reset_gp_344(m: &mut Machine) {
    m.sh(GP + 0x348, 0xa0);
    m.sh(GP + 0x34a, 0x80);
    m.sb(GP + 0x344, 0);
    m.sb(GP + 0x345, 0);
    m.sh(GP + 0x346, 0x40);
}

/// 80040748: packet set `[gp+0x440]`, two 0x278-byte buffers (the last in
/// `[gp+0x444]`); then the four resets 80040C90, 80040F50, 80041A48,
/// 80041DBC.
pub fn packets_440(m: &mut Machine) {
    let table = arena::alloc(m, 0x10);
    m.set_gw(SET_440_TABLE, table);
    for index in 0..2 {
        let buf = arena::alloc(m, 0x278);
        let table = m.gw(SET_440_TABLE);
        m.set_gw(SET_440_BUFFER, buf);
        put32(m, table, index * 4, buf);
        // 16 SPRT (code 0x64, semi) from +0x68.
        for i in 0..16 {
            let p = element(buf, 0x68, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x64);
            semi_trans(m, p);
        }
        // 3 SPRT (colour 0/83/0) at +0, +0x14, +0x28.
        for i in 0..3 {
            let p = element(buf, 0, 0x14, i);
            put8(m, p, 7, 0x66);
            put8(m, p, 3, 4);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0x83);
            put8(m, p, 6, 0);
            put16(m, p, 0xe, 0x7f98);
        }
        put8(m, buf, 0xc, 0xaa);
        put8(m, buf, 0xd, 0);
        put16(m, buf, 0x10, 0x31);
        put16(m, buf, 0x12, 0xf);
        let p = buf.wrapping_add(0x14);
        put8(m, p, 0xc, 0xcc);
        put8(m, p, 0xd, 0x10);
        put16(m, p, 0x10, 0xe);
        put16(m, p, 0x12, 0x28);
        // 3 TILE (len 3, code 0x62) at +0x28 (over the third sprite), +0x38,
        // +0x48.
        for i in 0..3 {
            let p = element(buf, 0x28, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x62);
        }
        put8(m, buf, 0x5b, 1);
        put8(m, buf, 0x63, 1);
        put32(m, buf, 0x5c, TPAGE_636);
        put32(m, buf, 0x64, TPAGE_616);
        // 2 POLY_FT4 (code 0x2E, colour 83/83/83) at +0x1E8, +0x210.
        for i in 0..2 {
            let p = element(buf, 0x1e8, 0x28, i);
            put8(m, p, 3, 9);
            put8(m, p, 4, 0x83);
            put8(m, p, 5, 0x83);
            put8(m, p, 6, 0x83);
            put8(m, p, 7, 0x2e);
        }
        put16(m, buf, 0x1fe, 0x25);
        let p = buf.wrapping_add(0x210);
        put16(m, p, 0x16, 0x36);
        put8(m, p, 0xd, 0x57);
        put8(m, p, 0x15, 0x57);
        put8(m, p, 0xc, 0x50);
        put8(m, p, 0x14, 0x7b);
        put8(m, p, 0x1c, 0x50);
        put8(m, p, 0x1d, 0x80);
        put8(m, p, 0x24, 0x7b);
        put8(m, p, 0x25, 0x80);
        put16(m, p, 0xe, 0x7f98);
        // 3 black TILE (code 0x62) from +0x238.
        for i in 0..3 {
            let p = element(buf, 0x238, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0);
            put8(m, p, 6, 0);
            put8(m, p, 7, 0x62);
        }
        put8(m, buf, 0x26b, 1);
        put8(m, buf, 0x273, 1);
        put32(m, buf, 0x26c, TPAGE_616);
        put32(m, buf, 0x274, TPAGE_616);
    }
    reset_gp_44c(m);
    reset_4f38(m);
    dispatch::call(m, 0x8004_1a48, [0; 4]); // hud.rs: message slot reset
    reset_gp_454(m);
}

/// 80040C90: byte `gp+0x450`, word `gp+0x44C` and halfword `gp+0x452` = 0.
pub fn reset_gp_44c(m: &mut Machine) {
    m.sb(GP + 0x450, 0);
    m.set_gw(0x44c, 0);
    m.sh(GP + 0x452, 0);
}

/// 80040F50: 800C4F38: bytes +3/+4 = 0xFF, byte +2 and halfword +0 = 0.
pub fn reset_4f38(m: &mut Machine) {
    let block = 0x800c_4f38;
    m.sb(block + 3, 0xff);
    m.sb(block + 4, 0xff);
    m.sb(block + 2, 0);
    m.sh(block, 0);
}

/// 80041DBC: byte `gp+0x454` = 0.
pub fn reset_gp_454(m: &mut Machine) {
    m.sb(GP + 0x454, 0);
}

/// 80041E08: packet set `[gp+0x45C]`, two 0x1450-byte buffers (the last in
/// `[gp+0x460]`): 128 LINE_F3, 128 LINE_F2 and 4 POLY_F3, all
/// semi-transparent.
pub fn packets_45c(m: &mut Machine) {
    let table = arena::alloc(m, 0x14);
    m.set_gw(SET_45C_TABLE, table);
    for index in 0..2 {
        let buf = arena::alloc(m, 0x1450);
        let table = m.gw(SET_45C_TABLE);
        m.set_gw(SET_45C_BUFFER, buf);
        put32(m, table, index * 4, buf);
        // 128 LINE_F3 (code 0x48, colour 0/FF/0) from +0.
        for i in 0..128 {
            let p = element(buf, 0, 0x18, i);
            put8(m, p, 3, 5);
            put8(m, p, 7, 0x48);
            put8(m, p, 4, 0);
            put32(m, p, 0x14, LINE_END);
            put8(m, p, 5, 0xff);
            put8(m, p, 6, 0);
            semi_trans(m, p);
        }
        // 128 LINE_F2 (code 0x40) from +0xC00.
        for i in 0..128 {
            let p = element(buf, 0xc00, 0x10, i);
            put8(m, p, 3, 3);
            put8(m, p, 7, 0x40);
            semi_trans(m, p);
        }
        // 4 POLY_F3 (len 4, code 0x20, colour 0/FF/0) from +0x1400.
        for i in 0..4 {
            let p = element(buf, 0x1400, 0x14, i);
            put8(m, p, 3, 4);
            put8(m, p, 7, 0x20);
            put8(m, p, 4, 0);
            put8(m, p, 5, 0xff);
            put8(m, p, 6, 0);
            semi_trans(m, p);
        }
    }
}

/// Every routine this module translates, by original address, with the
/// original `a0..a3` (`dispatch::call` convention); routines without a
/// meaningful `v0` return 0.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    // Routines without a meaningful v0 return 0; their callers ignore v0.
    let void = |m: &mut Machine, f: fn(&mut Machine)| {
        f(m);
        Some(0)
    };
    let with_pointer = |m: &mut Machine, f: fn(&mut Machine, u32)| {
        f(m, args[0]);
        Some(0)
    };
    match target {
        0x8003_9a68 => void(m, initialize_39a68),
        0x8003_9b30 => void(m, copy_name_upper),
        0x8006_2324 => void(m, packets_508),
        0x8005_cb98 => void(m, packets_4f4),
        0x8005_c9e4 => with_pointer(m, packet_block_4f4),
        0x8005_aeb8 => with_pointer(m, block_4f4_008),
        0x8005_b298 => with_pointer(m, block_4f4_09c),
        0x8005_b7e4 => with_pointer(m, block_4f4_1b8),
        0x8005_bc1c => with_pointer(m, block_4f4_30c),
        0x8005_c11c => with_pointer(m, block_4f4_244),
        0x8005_c580 => with_pointer(m, block_4f4_384),
        0x8005_c7d4 => with_pointer(m, block_4f4_3dc),
        0x8003_b180 => void(m, packets_424),
        0x8003_ba98 => void(m, packets_424_quads),
        0x8003_e4fc => void(m, packets_434),
        0x8003_eacc => void(m, clear_records_4568),
        0x8004_00f4 => void(m, reset_gp_344),
        0x8004_0748 => void(m, packets_440),
        0x8004_0c90 => void(m, reset_gp_44c),
        0x8004_0f50 => void(m, reset_4f38),
        0x8004_1dbc => void(m, reset_gp_454),
        0x8004_1e08 => void(m, packets_45c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_copy_uppercases_only_lowercase_letters() {
        let mut m = Machine::default();
        let context = 0x800b_d158;
        m.set_gw(CONTEXT, context);
        let name = b"Ace`{az@Z09_~\xe1x";
        m.write_bytes(context + 0x94c, name);
        m.sb(context + 0x94c + 15, b'q');
        copy_name_upper(&mut m);
        assert_eq!(
            m.read_bytes(NAME_UPPER, 16),
            b"ACE`{AZ@Z09_~\xe1XQ".to_vec()
        );
        assert_eq!(m.lbu(NAME_UPPER + 16), 0, "exactly 16 bytes");
    }

    #[test]
    fn record_clear_skips_null_pointers() {
        let mut m = Machine::default();
        let record = 0x8012_0000;
        m.write_bytes(record, &[0xaa; 16]);
        // A followed null pointer would clear RAM at address 0.
        m.write_bytes(0, &[0xaa; 16]);
        m.sw(RECORDS_4568 + 5 * 4, record);
        clear_records_4568(&mut m);
        let mut expected = [0xaa; 16];
        for i in [0, 1, 6, 7, 8, 9, 10, 11] {
            expected[i] = 0;
        }
        assert_eq!(m.read_bytes(record, 16), expected.to_vec());
        assert_eq!(m.read_bytes(0, 16), vec![0xaa; 16], "null entries skipped");
        assert!(m.take_fault().is_ok());
    }
}
