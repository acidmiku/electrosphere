//! Patches of `init_39a68` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3), part 3 of the resident wave. Each is listed in
//! [`REPLACES`](super::REPLACES). (`reset_gp_344`, 800409AC, is the
//! readdress agent's example in `init_39a68.rs`.)
//!
//! Most differences move the panel graphics up for the 240-line NTSC
//! display: y coordinates near the bottom by -20 (0xE6 -> 0xD2), the 64x64
//! window by 2 lines, the frame-buffer height 0x100 -> 0xF0.
use crate::{
    arena,
    machine::{GP, Machine, SmallData},
    sdk,
};

/// 800C6D60: state block reset first by 8003A008.
const STATE_4B28: u32 = 0x800c_6d60;
/// 800C6EC8: word set to 0xC80 by 8003A008.
const WORD_4C90: u32 = 0x800c_6ec8;
/// gp slots of the packet set `[gp+0x434]`: pointer table, current buffer.
const SET_424_TABLE: u32 = 0x434;
const SET_424_BUFFER: u32 = 0x438;

/// Byte tables of 8003B924 in the small-data area.
const QUAD_X: u32 = GP + 0xb8;
const QUAD_Y: u32 = GP + 0xc0;
const QUAD_W: u32 = GP + 0xc8;
const QUAD_H: u32 = GP + 0xd0;
const ICON_X: u32 = GP + 0xd8;
const ICON_Y: u32 = GP + 0xdc;
/// Halfword x/y and byte u tables of the four 8003B924 sprites.
const SPRITE_X: u32 = GP + 0xe0;
const SPRITE_Y: u32 = GP + 0xe8;
const SPRITE_U: u32 = GP + 0xf0;
/// 8003C250 tables: four (x, _, y, _) halfword quads, four u and four v
/// byte quads.
const TILE_XY: u32 = 0x800b_108c;
const TILE_U: u32 = 0x800b_10ac;
const TILE_V: u32 = 0x800b_10bc;

/// Polyline terminator word.
const LINE_END: u32 = 0x5555_5555;
/// DR_TPAGE command words (libgpu `getTPage` results with draw-mode bits).
const TPAGE_205: u32 = 0xe100_0205;
const TPAGE_236: u32 = 0xe100_0236;

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

// ---- 8003A008 -------------------------------------------------------------------

/// 8003A008 (JP; PAL 80039A68, near, `call-:1 del:7`): reset the 800C6D60
/// state block and build the six packet sets in original order. JP has no
/// upper-case copy of the context name (PAL 80039B30 when `[gp+0x4B8]` is
/// 0x1E).
pub fn initialize_39a68(m: &mut Machine) {
    use crate::init_39a68::{packets_4f4, packets_45c, packets_434, packets_440, packets_508};
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
}

// ---- set [gp+0x4EC] blocks -------------------------------------------------------

/// 8005C5B0 (JP; PAL 8005AEB8, near, `imm:1`) (block +0x008): five SPRT
/// from +0, then two POLY_F4 (code 0x2A) at +0x64/+0x7C; +0x64 links to
/// +0x7C and +0x7C back to +0. The first sprite's y is 0x80 (PAL 0x88).
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
    put16(m, p, 0xa, 0x80);
    let first = get32(m, p, 0x64);
    let second = get32(m, p, 0x7c);
    put8(m, p, 0xc, 0);
    let next = p.wrapping_add(0x7c) & 0x00ff_ffff;
    put32(m, p, 0x64, (first & 0xff00_0000) | next);
    put32(m, p, 0x7c, (second & 0xff00_0000) | (p & 0x00ff_ffff));
}

/// 8005C990 (JP; PAL 8005B298, near, `imm:1`) (block +0x09C): thirteen
/// SPRT. +0 links to +0x14; the five from +0x28 (6x10 at x 0x12E) link in a
/// chain; six bare ones from +0x8C. The first two sprites' y is 0x60 (PAL
/// 0x68).
pub fn block_4f4_09c(m: &mut Machine, p: u32) {
    put8(m, p, 0xc, 0x82);
    put16(m, p, 0x10, 0x36);
    put16(m, p, 0x12, 0x10);
    put16(m, p, 8, 0xef);
    put8(m, p, 3, 4);
    put16(m, p, 0xe, 0x7f98);
    put8(m, p, 0xd, 0xf);
    put16(m, p, 0xa, 0x60);
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
    put16(m, q, 0xa, 0x60);
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

/// 8005CEDC (JP; PAL 8005B7E4, near, `imm:2`) (block +0x1B8): seven SPRT,
/// the first six linked in order; the first two at y 0xCE/0xD2 (PAL
/// 0xE2/0xE6).
pub fn block_4f4_1b8(m: &mut Machine, p: u32) {
    put8(m, p, 0xc, 0x44);
    put8(m, p, 0xd, 0x19);
    put16(m, p, 0x10, 0x3c);
    put16(m, p, 8, 0xc);
    put8(m, p, 3, 4);
    put16(m, p, 0xe, 0x7f98);
    put16(m, p, 0x12, 0xe);
    put16(m, p, 0xa, 0xce);
    put8(m, p, 7, 0x66);
    let q = p.wrapping_add(0x14);
    put8(m, q, 0xc, 0x32);
    put8(m, q, 0xd, 0x31);
    put16(m, q, 0x10, 5);
    put16(m, q, 0x12, 6);
    put8(m, q, 3, 4);
    put16(m, q, 0xe, 0x7f98);
    put16(m, q, 8, 0xe);
    put16(m, q, 0xa, 0xd2);
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

/// 8005D344 (JP; PAL 8005BC1C, near, `imm:3`) (block +0x30C): six SPRT; +0
/// links to +0x14. The first three at y 0xD2, 0xB6, 0xAC (PAL 0xE6, 0xCA,
/// 0xC0).
pub fn block_4f4_30c(m: &mut Machine, p: u32) {
    put16(m, p, 0xe, 0x7f98);
    put8(m, p, 0xc, 0xdc);
    put16(m, p, 0x10, 0x24);
    put16(m, p, 0x12, 0x11);
    put16(m, p, 8, 0xee);
    put8(m, p, 3, 4);
    put16(m, p, 0xa, 0xd2);
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
    put16(m, q, 0xa, 0xb6);
    let q = p.wrapping_add(0x28);
    put16(m, q, 0xe, 0x7f5b);
    put8(m, q, 0xd, 0xd0);
    put16(m, q, 0x10, 0x28);
    put16(m, q, 0x12, 0x28);
    put16(m, q, 8, 0x107);
    put8(m, q, 3, 4);
    put8(m, q, 7, 0x66);
    put8(m, q, 0xc, 0xd8);
    put16(m, q, 0xa, 0xac);
    for i in 0..3 {
        let q = element(p, 0x3c, 0x14, i);
        put8(m, q, 3, 4);
        put8(m, q, 7, 0x66);
        put16(m, q, 0xe, 0x7f98);
    }
}

/// One 5x7 SPRT of 8005D844 at `q`: uv (0x38, 0x30), position (`x`, `y`).
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

/// 8005D844 (JP; PAL 8005C11C, near, `imm:3`) (block +0x244): ten SPRT: one
/// at +0 (y 0xBE, PAL 0xD2), four 5x7 at y 0xB6 (PAL 0xCA; x from 0xF2,
/// step 5), two at y 0xAD (PAL 0xC1; x from 0xFC), three bare ones.
pub fn block_4f4_244(m: &mut Machine, p: u32) {
    put8(m, p, 3, 4);
    put8(m, p, 7, 0x66);
    put16(m, p, 0xe, 0x7f98);
    put8(m, p, 0xc, 0x9e);
    put8(m, p, 0xd, 0x21);
    put16(m, p, 0x10, 0x19);
    put16(m, p, 0x12, 0x14);
    put16(m, p, 8, 0xee);
    put16(m, p, 0xa, 0xbe);
    let mut x = 0xf2;
    for i in 0..4 {
        small_sprite(m, element(p, 0x14, 0x14, i), x, 0xb6);
        x += 5;
    }
    let mut x = 0xfc;
    for i in 0..2 {
        small_sprite(m, element(p, 0x64, 0x14, i), x, 0xad);
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

/// 8005DCA8 (JP; PAL 8005C580, near, `imm:3`) (block +0x384): two SPRT
/// (clut 0x7F18) and three LINE_F2, linked in order; the second sprite at
/// y 0xDB (PAL 0xEF), the first line at y 0xE2 (PAL 0xF6), the second from
/// y 0xDA (PAL 0xEE).
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
    put16(m, q, 0xa, 0xdb);
    put8(m, q, 7, 0x66);
    line(m, p.wrapping_add(0x28), 0, 0xe2, 0xde, 0xe2);
    line(m, p.wrapping_add(0x38), 0xe6, 0xda, 0xe6, 0xcb);
    line(m, p.wrapping_add(0x48), 0x10c, 0xa4, 0x140, 0xa4);
    sdk::cat_prim(m, p, p.wrapping_add(0x14));
    sdk::cat_prim(m, p.wrapping_add(0x14), p.wrapping_add(0x28));
    sdk::cat_prim(m, p.wrapping_add(0x28), p.wrapping_add(0x38));
    sdk::cat_prim(m, p.wrapping_add(0x38), p.wrapping_add(0x48));
}

/// 8005DEFC (JP; PAL 8005C7D4, changed, `del:1 imm:1 ins:1 op:3 reg:12`)
/// (block +0x3DC): two SPRT and two LINE_F2, linked +0x28, +0x38, +0,
/// +0x14, in the JP store order. JP moves the first line to y 0x2D (PAL
/// 0x2B), the second to y 0xE (PAL 0xC) and both sprites to y 0xC (PAL 0xA).
pub fn block_4f4_3dc(m: &mut Machine, p: u32) {
    let q = p.wrapping_add(0x28);
    put8(m, q, 3, 3);
    put8(m, q, 7, 0x42);
    put16(m, q, 8, 0);
    put16(m, q, 0xa, 0x2d);
    put16(m, q, 0xc, 4);
    put16(m, q, 0xe, 0x2d);
    let q = p.wrapping_add(0x38);
    put8(m, q, 7, 0x42);
    put8(m, q, 3, 3);
    put16(m, q, 8, 0x55);
    put16(m, q, 0xc, 0x140);
    put16(m, q, 0xa, 0xe);
    put16(m, q, 0xe, 0xe);
    put16(m, p, 8, 5);
    put8(m, p, 0xd, 0x37);
    put16(m, p, 0x10, 0x50);
    put16(m, p, 0x12, 0x49);
    put8(m, p, 3, 4);
    put8(m, p, 7, 0x66);
    put16(m, p, 0xe, 0x7f98);
    put16(m, p, 0xa, 0xc);
    put8(m, p, 0xc, 0);
    let q = p.wrapping_add(0x14);
    put16(m, q, 8, 9);
    put8(m, q, 0xc, 0x18);
    put8(m, q, 3, 4);
    put8(m, q, 7, 0x66);
    put16(m, q, 0xe, 0x7f98);
    put16(m, q, 0xa, 0xc);
    put8(m, q, 0xd, 0x2d);
    put16(m, q, 0x10, 0xc);
    put16(m, q, 0x12, 0xa);
    sdk::cat_prim(m, p.wrapping_add(0x28), p.wrapping_add(0x38));
    sdk::cat_prim(m, p.wrapping_add(0x38), p);
    sdk::cat_prim(m, p, p.wrapping_add(0x14));
}

// ---- set [gp+0x434] ------------------------------------------------------------------

/// Texture coordinates of the four 16x16 quads at 8003B924 +0xDEC: the same
/// cell mirrored horizontally and vertically.
const ICON_UV: [[(u8, u8); 4]; 4] = [
    [(0x70, 0x37), (0x80, 0x37), (0x70, 0x47), (0x80, 0x47)],
    [(0x7f, 0x37), (0x6f, 0x37), (0x7f, 0x47), (0x6f, 0x47)],
    [(0x70, 0x46), (0x80, 0x46), (0x70, 0x36), (0x80, 0x36)],
    [(0x7f, 0x46), (0x6f, 0x46), (0x7f, 0x36), (0x6f, 0x36)],
];
/// JP 8003B924 halfword coordinates of the four LINE_F3 at +0xECC, in the
/// original store order (PAL y 0x60/0x68/0xA0/0x98 are 0x58/0x60/0x98/0x90).
const FRAME_LINES: [(u32, u16); 24] = [
    (0xed6, 0x58),
    (0xeda, 0x58),
    (0xeee, 0x58),
    (0xef2, 0x58),
    (0xed8, 0x80),
    (0xedc, 0x80),
    (0xf08, 0x80),
    (0xf0c, 0x80),
    (0xed4, 0x88),
    (0xede, 0x60),
    (0xeec, 0xb8),
    (0xef0, 0xc0),
    (0xef4, 0xc0),
    (0xef6, 0x60),
    (0xf04, 0x88),
    (0xf06, 0x98),
    (0xf0a, 0x98),
    (0xf0e, 0x90),
    (0xf1c, 0xb8),
    (0xf1e, 0x98),
    (0xf20, 0xc0),
    (0xf22, 0x98),
    (0xf24, 0xc0),
    (0xf26, 0x90),
];
/// JP 8003B924 halfword coordinates of the ten LINE_F2 at +0xF2C, in the
/// JP store order (the scale moves up 8 lines; PAL's order differs).
const SCALE_LINES: [(u32, u16); 40] = [
    (0xf38, 0xa4),
    (0xf48, 0xa4),
    (0xf58, 0xa4),
    (0xf68, 0x84),
    (0xf34, 0x9c),
    (0xf44, 0x9c),
    (0xf54, 0x9c),
    (0xf74, 0xbc),
    (0xf46, 0x78),
    (0xf4a, 0x78),
    (0xf66, 0x78),
    (0xf6a, 0x78),
    (0xf76, 0x78),
    (0xf7a, 0x78),
    (0xf8a, 0x7c),
    (0xf9a, 0x7c),
    (0xfaa, 0x7c),
    (0xf36, 0x58),
    (0xf3a, 0x58),
    (0xf56, 0x98),
    (0xf5a, 0x98),
    (0xf64, 0x80),
    (0xf78, 0xc0),
    (0xf84, 0x80),
    (0xf86, 0x74),
    (0xf88, 0x80),
    (0xf94, 0xa0),
    (0xf96, 0x74),
    (0xf98, 0xa0),
    (0xfa4, 0xc0),
    (0xfa6, 0x74),
    (0xfa8, 0xc0),
    (0xfb4, 0xa0),
    (0xfba, 0x5c),
    (0xfb6, 0x58),
    (0xfb8, 0xa0),
    (0xfc4, 0xa0),
    (0xfc6, 0x98),
    (0xfc8, 0xa0),
    (0xfca, 0x94),
];

/// 8003B924 (JP; PAL 8003B180, near, `del:8 imm:15 ins:13 op:2 reg:23`):
/// packet set `[gp+0x434]`, two 0x1080-byte buffers (the last in
/// `[gp+0x438]`), as PAL with the JP layout: the 64x64 drawing area at (12,
/// 14) or (12, 270) (PAL y 12/268), the full-screen area 320x240 (PAL
/// 256), the frame and scale lines and the 8x9 sprite moved up (y 0x74,
/// PAL 0x7C). Ends with [`packets_424_quads`]. The RECT passed to
/// `SetDrawArea` lives in the original 0x30-byte frame (`sp+0x10`).
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
        m.sh(rect + 2, 0xe);
        m.sh(rect + 4, 0x40);
        m.sh(rect + 6, 0x40);
        if index == 1 {
            m.sh(rect + 2, 0x10e);
        }
        sdk::set_draw_area(m, buf, rect);
        m.sh(rect + 4, 0x140);
        m.sh(rect, 0);
        m.sh(rect + 2, 0);
        m.sh(rect + 6, 0xf0);
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
        // SPRT 8x9 at (0x9C, 0x74), colour 83/83/83.
        let p = buf.wrapping_add(0xffc);
        put8(m, p, 7, 0x66);
        put8(m, p, 3, 4);
        put8(m, p, 4, 0x83);
        put8(m, p, 5, 0x83);
        put8(m, p, 6, 0x83);
        put16(m, p, 0xe, 0x7f98);
        put16(m, p, 8, 0x9c);
        put16(m, p, 0xa, 0x74);
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

/// 8003C250 (JP; PAL 8003BA98, near, `del:1 imm:4 ins:1`): second pass over
/// both `[gp+0x434]` buffers (leaving the last in `[gp+0x438]`), as PAL
/// except that both sprites at +0xC0C/+0xC20 are at y 0xC (PAL 0xA), the
/// second in the JP store order.
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
        put16(m, p, 0xa, 0xc);
        let p = buf.wrapping_add(0xc20);
        put16(m, p, 0xe, 0x7f98);
        put8(m, p, 0xd, 0x2d);
        put8(m, p, 3, 4);
        put8(m, p, 0xc, 0);
        put16(m, p, 0x12, 0xa);
        put16(m, p, 8, 0xa);
        put8(m, p, 7, 0x66);
        put16(m, p, 0x10, 0xc);
        put16(m, p, 0xa, 0xc);
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
