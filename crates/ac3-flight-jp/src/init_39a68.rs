//! Subsystem initialization tree under 8003A008 (called by gameplay
//! initialization 80032754).
//!
//! 8003A008 resets a few state blocks and builds six double-buffered GPU
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
    arena, dispatch,
    machine::{GP, Machine, SmallData},
    sdk,
};

/// 800C67A0: 64 record pointers cleared by [`clear_records_4568`].
const RECORDS_4568: u32 = 0x800c_67a0;
/// gp slots of the packet sets: pointer table, then current buffer.
const SET_508_TABLE: u32 = 0x500;
const SET_508_BUFFER: u32 = 0x504;
const SET_4F4_TABLE: u32 = 0x4ec;
const SET_434_TABLE: u32 = 0x444;
const SET_434_BUFFER: u32 = 0x448;
const SET_440_TABLE: u32 = 0x450;
const SET_440_BUFFER: u32 = 0x454;
const SET_45C_TABLE: u32 = 0x46c;
const SET_45C_BUFFER: u32 = 0x470;

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

// JP-REVIEW(R0192): near function: PAL 80039A68 -> JP 8003A008, call-:1 del:7, 7 instructions differ (whole routine)
/// 8003A008: reset the 800C6D60 state block, build the six packet sets in
/// original order, then (when `[gp+0x4B8]` is 0x1E) copy the context name in
/// upper case.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::initialize_39a68` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn initialize_39a68(m: &mut Machine) {
    crate::jp::res_p3_init_39a68::initialize_39a68(m)
}

/// 80063AF0: packet set `[gp+0x500]`, two 0x9D4-byte buffers (the last in
/// `[gp+0x504]`).
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

/// 8005E2C0: packet set `[gp+0x4EC]`, two 0x424-byte blocks built by
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

/// 8005E10C: one 0x424-byte block of set `[gp+0x4EC]`: seven primitive
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

// JP-REVIEW(R0194): near function: PAL 8005AEB8 -> JP 8005C5B0, imm:1, 1 instructions differ (whole routine)
/// 8005C5B0 (block +0x008): five SPRT from +0, then two POLY_F4 (code 0x2A)
/// at +0x64/+0x7C; +0x64 links to +0x7C and +0x7C back to +0.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::block_4f4_008` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_4f4_008(m: &mut Machine, p: u32) {
    crate::jp::res_p3_init_39a68::block_4f4_008(m, p)
}

// JP-REVIEW(R0195): near function: PAL 8005B298 -> JP 8005C990, imm:1, 1 instructions differ (whole routine)
/// 8005C990 (block +0x09C): thirteen SPRT. +0 links to +0x14; the five from
/// +0x28 (6x10 at x 0x12E) link in a chain; six bare ones from +0x8C.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::block_4f4_09c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_4f4_09c(m: &mut Machine, p: u32) {
    crate::jp::res_p3_init_39a68::block_4f4_09c(m, p)
}

// JP-REVIEW(R0196): near function: PAL 8005B7E4 -> JP 8005CEDC, imm:2, 2 instructions differ (whole routine)
/// 8005CEDC (block +0x1B8): seven SPRT, the first six linked in order.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::block_4f4_1b8` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_4f4_1b8(m: &mut Machine, p: u32) {
    crate::jp::res_p3_init_39a68::block_4f4_1b8(m, p)
}

// JP-REVIEW(R0197): near function: PAL 8005BC1C -> JP 8005D344, imm:3, 3 instructions differ (whole routine)
/// 8005D344 (block +0x30C): six SPRT; +0 links to +0x14.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::block_4f4_30c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_4f4_30c(m: &mut Machine, p: u32) {
    crate::jp::res_p3_init_39a68::block_4f4_30c(m, p)
}

// JP-REVIEW(R0199): near function: PAL 8005C11C -> JP 8005D844, imm:3, 3 instructions differ (whole routine)
/// 8005D844 (block +0x244): ten SPRT: one at +0, four 5x7 at y 0xCA (x from
/// 0xF2, step 5), two at y 0xC1 (x from 0xFC), three bare ones.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::block_4f4_244` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_4f4_244(m: &mut Machine, p: u32) {
    crate::jp::res_p3_init_39a68::block_4f4_244(m, p)
}

// JP-REVIEW(R0201): near function: PAL 8005C580 -> JP 8005DCA8, imm:3, 3 instructions differ (whole routine)
/// 8005DCA8 (block +0x384): two SPRT (clut 0x7F18) and three LINE_F2,
/// linked in order.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::block_4f4_384` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_4f4_384(m: &mut Machine, p: u32) {
    crate::jp::res_p3_init_39a68::block_4f4_384(m, p)
}

// JP-REVIEW(R0202): changed function: PAL 8005C7D4 -> JP 8005DEFC, del:1 imm:1 ins:1 op:3 reg:12, 18 instructions differ (whole routine)
/// 8005DEFC (block +0x3DC): two SPRT and two LINE_F2, linked +0x28, +0x38,
/// +0, +0x14.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::block_4f4_3dc` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_4f4_3dc(m: &mut Machine, p: u32) {
    crate::jp::res_p3_init_39a68::block_4f4_3dc(m, p)
}

const TPAGE_616: u32 = 0xe100_0616;
const TPAGE_636: u32 = 0xe100_0636;

// JP-REVIEW(R0203): near function: PAL 8003B180 -> JP 8003B924, del:8 imm:15 ins:13 op:2 reg:23, 61 instructions differ (whole routine)
/// 8003B924: packet set `[gp+0x434]`, two 0x1080-byte buffers (the last in
/// `[gp+0x438]`): two DR_AREA (a 64x64 window at (12, 12) or (12, 268) and
/// the full 320x256 frame buffer), sprites, tiles, lines and textured quads
/// placed from the gp+0xB8.. tables. Ends with [`packets_424_quads`]. The
/// RECT passed to [`sdk::set_draw_area`] lives in the original 0x30-byte frame on
/// the guest stack (`sp+0x10`).
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::packets_424` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn packets_424(m: &mut Machine) {
    crate::jp::res_p3_init_39a68::packets_424(m)
}

// JP-REVIEW(R0204): near function: PAL 8003BA98 -> JP 8003C250, del:1 imm:4 ins:1, 6 instructions differ (whole routine)
/// 8003C250: second pass over both `[gp+0x434]` buffers (leaving the last in
/// `[gp+0x438]`): textured quads from the 800B108C tables (+0xAA4, eight
/// chained by four from +0xACC), two sprites, two DR_TPAGE, two tiles and
/// five E6 words.
// JP-PATCH: replaced by `crate::jp::res_p3_init_39a68::packets_424_quads` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn packets_424_quads(m: &mut Machine) {
    crate::jp::res_p3_init_39a68::packets_424_quads(m)
}

/// 8003ECBC: packet set `[gp+0x444]`, two 0x1558-byte buffers (the last in
/// `[gp+0x448]`): 64 LINE_F2, 128 LINE_F3, 68 SPRT and a DR_TPAGE at +0;
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

/// 8003F284: for each non-null pointer of the 64 at 800C67A0, clear bytes
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

// JP-REVIEW(R0205): near function: PAL 800400F4 -> JP 800409AC, imm:1, 1 instructions differ (whole routine)
/// 800409AC: halfwords `gp+0x358` = 0xA0, `gp+0x35A` = 0x80, `gp+0x356` =
/// 0x40; bytes `gp+0x354/0x355` = 0.
// JP-PATCH: replaced by `crate::jp::init_39a68::reset_gp_344` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn reset_gp_344(m: &mut Machine) {
    crate::jp::init_39a68::reset_gp_344(m)
}

/// 80041120: packet set `[gp+0x450]`, two 0x278-byte buffers (the last in
/// `[gp+0x454]`); then the four resets 80041640, 80041900, 80042524,
/// 800428EC.
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
    dispatch::call(m, 0x8004_2524, [0; 4]); // hud.rs: message slot reset
    reset_gp_454(m);
}

/// 80041640: byte `gp+0x460`, word `gp+0x45C` and halfword `gp+0x462` = 0.
pub fn reset_gp_44c(m: &mut Machine) {
    m.sb(GP + 0x460, 0);
    m.set_gw(0x45c, 0);
    m.sh(GP + 0x462, 0);
}

/// 80041900: 800C7170: bytes +3/+4 = 0xFF, byte +2 and halfword +0 = 0.
pub fn reset_4f38(m: &mut Machine) {
    let block = 0x800c_7170;
    m.sb(block + 3, 0xff);
    m.sb(block + 4, 0xff);
    m.sb(block + 2, 0);
    m.sh(block, 0);
}

/// 800428EC: byte `gp+0x464` = 0.
pub fn reset_gp_454(m: &mut Machine) {
    m.sb(GP + 0x464, 0);
}

/// 80042938: packet set `[gp+0x46C]`, two 0x1450-byte buffers (the last in
/// `[gp+0x470]`): 128 LINE_F3, 128 LINE_F2 and 4 POLY_F3, all
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
#[allow(unused_variables)]
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
        0x8003_a008 => void(m, initialize_39a68),
        0x8006_3af0 => void(m, packets_508),
        0x8005_e2c0 => void(m, packets_4f4),
        0x8005_e10c => with_pointer(m, packet_block_4f4),
        0x8005_c5b0 => with_pointer(m, block_4f4_008),
        0x8005_c990 => with_pointer(m, block_4f4_09c),
        0x8005_cedc => with_pointer(m, block_4f4_1b8),
        0x8005_d344 => with_pointer(m, block_4f4_30c),
        0x8005_d844 => with_pointer(m, block_4f4_244),
        0x8005_dca8 => with_pointer(m, block_4f4_384),
        0x8005_defc => with_pointer(m, block_4f4_3dc),
        0x8003_b924 => void(m, packets_424),
        0x8003_c250 => void(m, packets_424_quads),
        0x8003_ecbc => void(m, packets_434),
        0x8003_f284 => void(m, clear_records_4568),
        0x8004_09ac => void(m, reset_gp_344),
        0x8004_1120 => void(m, packets_440),
        0x8004_1640 => void(m, reset_gp_44c),
        0x8004_1900 => void(m, reset_4f38),
        0x8004_28ec => void(m, reset_gp_454),
        0x8004_2938 => void(m, packets_45c),
        _ => None,
    }
}
