//! JP overlay 1, part 2: resource loaders, primitive builders, the cursor
//! emblem, the audio level and the disc prompt (`800D8AF0`..`800DA6F8`).
//!
//! The primitive builders take a description record, the packet to fill
//! and the ordering-table entry to link it into, and return the packet's
//! end: [`line2`] (LINE_F2), [`line3`] (LINE_F3), [`line4`] (LINE_F4),
//! [`tile`] (TILE), [`poly_g3`] (POLY_G3), [`poly_g4`] (POLY_G4),
//! [`poly_ft4`] (POLY_FT4) and [`draw_mode`] (DR_MODE).
//!
//! The disc block `[0x800DD910]` (0x800DDA70: +0 prompt state, +4, +8 the
//! required disc's index into 0x800DD8D4) drives the "insert disc" prompt
//! through the resident disc getters 8002B504 / 8002B528 and the lid check
//! 8002B428.
use super::ov1_p2::{
    STRCPY, call, context, lbu, lh, lhu, load_unaligned, loaded, off, sext16, slt, sra,
    store_unaligned,
};
use crate::{arena, bios, boot, loading, machine::Machine, sdk};

/// 0x1A7A6C55: the compressed-resource signature.
const COMPRESSED: u32 = 0x1a7a_6c55;
/// 80027084 (resident): upload a TIM's image at a position (five arguments).
const UPLOAD_AT: u32 = 0x8002_7084;
/// 8002877C vsprintf(buffer, format, arguments).
const VSPRINTF: u32 = 0x8002_877c;
/// 8002B504: the disc number (-1 for an error).
const DISC_NUMBER: u32 = 0x8002_b504;
/// 8002B528: the disc state.
const DISC_STATE: u32 = 0x8002_b528;
/// 8002B428: the lid check.
const LID_CHECK: u32 = 0x8002_b428;
/// 800AA9CC StoreImage2(rect, pixels).
const STORE_IMAGE2: u32 = 0x800a_a9cc;
/// `[0x800DD910]`: the disc block.
const DISC: u32 = 0x800d_d910;
/// 0x800DD8D4: the disc of each index.
const DISC_OF: u32 = 0x800d_d8d4;
/// `[0x800DDA60]`: the audio buffer block (0x101C bytes).
const AUDIO: u32 = 0x800d_da60;
/// `[0x800DDA68]`: the sound history block (12 bytes).
const SOUNDS: u32 = 0x800d_da68;
/// 0x800CC58C: the screen rectangle {0, 0, 320, 480}; 0x800CC594 the dump
/// file name format.
const SCREEN_RECT: u32 = 0x800c_c58c;
const DUMP_NAME: u32 = 0x800c_c594;
/// 0x80200000: the dump buffer (a TIM header, then the pixels).
const DUMP: u32 = 0x8020_0000;

/// Expand `data` into `buffer` when it is compressed: the data to use.
fn expand(m: &mut Machine, data: u32, buffer: u32) -> u32 {
    if m.lw(data) == COMPRESSED {
        loading::decompress(m, data, buffer);
        buffer
    } else {
        data
    }
}

/// The packet `packet` takes the link of the entry `ot`, which then points
/// at it (`tag` is the packet's first word as read before).
fn link_with(m: &mut Machine, ot: u32, packet: u32, tag: u32) {
    let next = m.lw(ot);
    m.sw(packet, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let entry = m.lw(ot);
    m.sw(ot, (entry & 0xff00_0000) | (packet & 0x00ff_ffff));
}

/// The blend bit of a packet's code byte (+7) from flag `flag`.
fn blend_code(m: &mut Machine, packet: u32, flag: u32) {
    let code = lbu(m, off(packet, 7));
    let code = if flag != 0 { code | 2 } else { code & 0xfd };
    m.sb(off(packet, 7), code as u8);
}

// ---- resources -------------------------------------------------------------------------

/// 800D8AF0 (unreferenced): read the file `directory` + `name` (`bytes`
/// bytes) into a new arena buffer: the buffer (the read's result at
/// `[result]`), or 0 (-1 there) when it does not open.
pub fn load_file(m: &mut Machine, directory: u32, name: u32, bytes: u32, result: u32) -> u32 {
    let frame = m.push_frame(0x78);
    let path = off(frame, 0x10);
    call(m, STRCPY, [path, directory, 0, 0]);
    let length = sdk::strlen(m, directory);
    call(m, STRCPY, [off(path, length), name, 0, 0]);
    let mut buffer = arena::alloc(m, bytes);
    let fd = bios::open(m, path, 1);
    if fd == u32::MAX {
        m.sw(result, fd);
        buffer = 0;
    } else {
        let read = bios::read(m, fd, buffer, bytes);
        m.sw(result, read);
    }
    bios::close(m, fd);
    m.pop_frame(0x78);
    buffer
}

/// 800D8BA8: upload the TIMs of a texture group (expanded into a 0x30000
/// buffer when compressed), then free the buffer. Returns 1.
pub fn upload_group(m: &mut Machine, data: u32) -> u32 {
    m.push_frame(0x20);
    let buffer = arena::alloc(m, 0x30000);
    let group = expand(m, data, buffer);
    let list = loading::relocate_directory(m, group);
    // 80027338 leaves its loop counter, the TIM count, in a1
    let count = lhu(m, group);
    loading::upload_tims(m, list, count);
    sdk::draw_sync(m, 0);
    arena::reset(m, buffer);
    m.pop_frame(0x20);
    1
}

/// 800D8C30 (unreferenced): upload each TIM of a directory (each expanded
/// into a 0x10000 buffer when compressed).
pub fn upload_directory(m: &mut Machine, directory: u32) {
    m.push_frame(0x28);
    let count = m.lw(directory);
    let buffer = arena::alloc(m, 0x10000);
    let mut slot = loading::relocate_directory(m, directory);
    let mut i = 0u32;
    while slt(i, count) {
        let tim = m.lw(slot);
        let tim = expand(m, tim, buffer);
        slot = off(slot, 4);
        boot::upload_tim(m, tim);
        sdk::draw_sync(m, 0);
        i = i.wrapping_add(1);
    }
    arena::reset(m, buffer);
    m.pop_frame(0x28);
}

/// 800D8CEC: upload one TIM (expanded into a 0x10000 buffer when
/// compressed). Returns the arena reset's result.
pub fn upload_one(m: &mut Machine, data: u32) -> u32 {
    m.push_frame(0x20);
    let buffer = arena::alloc(m, 0x10000);
    let tim = expand(m, data, buffer);
    boot::upload_tim(m, tim);
    sdk::draw_sync(m, 0);
    let v0 = arena::reset(m, buffer);
    m.pop_frame(0x20);
    v0
}

/// 800D8D64 (unreferenced): upload a TIM (expanded when compressed) at a
/// position through 80027084 (`x`, `y`, `w` and the fifth argument as
/// halfwords).
pub fn upload_placed(m: &mut Machine, data: u32, x: u32, y: u32, w: u32) {
    let frame = m.push_frame(0x38);
    let h = m.lw(off(frame, 0x48));
    let buffer = arena::alloc(m, 0x10000);
    let tim = expand(m, data, buffer);
    m.sw(off(frame, 0x10), sext16(h));
    call(m, UPLOAD_AT, [tim, sext16(x), sext16(y), sext16(w)]);
    sdk::draw_sync(m, 0);
    arena::reset(m, buffer);
    m.pop_frame(0x38);
}

/// 800D8E38 (unreferenced): upload a TIM (expanded when compressed), copy
/// its CLUT words (+0x14, 8 words) to `clut` and fill `pixels` with
/// 0xCE73CE73, which LoadImage sends to {0x1B0, 0x38, 16, 8}.
pub fn upload_with_clut(m: &mut Machine, data: u32, clut: u32, pixels: u32) {
    let frame = m.push_frame(0x30);
    m.sh(off(frame, 0x10), 0x1b0);
    m.sh(off(frame, 0x12), 0x38);
    m.sh(off(frame, 0x14), 0x10);
    m.sh(off(frame, 0x16), 8);
    let buffer = arena::alloc(m, 0x10000);
    let tim = expand(m, data, buffer);
    boot::upload_tim(m, tim);
    for i in 0..8 {
        let word = m.lw(off(tim, 0x14 + 4 * i));
        m.sw(off(clut, 4 * i), word);
        m.sw(off(pixels, 4 * i), 0xce73_ce73);
    }
    sdk::load_image(m, off(frame, 0x10), pixels);
    sdk::draw_sync(m, 0);
    arena::reset(m, buffer);
    m.pop_frame(0x30);
}

// ---- primitives ------------------------------------------------------------------------

/// 800D8F3C (unreferenced): a DR_MODE packet from the record's bytes (dfe
/// +0, dtd +1, semi-transparency +2, colour mode +3) and texture position
/// (+4, +6), linked into `ot`. Returns the packet's end.
pub fn draw_mode(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let dfe = lbu(m, record);
    let dtd = lbu(m, off(record, 1));
    let semi = lbu(m, off(record, 2));
    let depth = lbu(m, off(record, 3));
    let ty = lhu(m, off(record, 6));
    let tx = lhu(m, off(record, 4));
    m.sw(off(frame, 0x10), 0);
    let tpage = ((semi & 3) << 7)
        | ((depth & 3) << 5)
        | sext16(sra((ty & 0x100) << 16, 20))
        | ((tx & 0x3ff) >> 6)
        | (sext16(ty & 0x200) << 2);
    sdk::set_draw_mode(m, packet, dfe, dtd, tpage, 0);
    let tag = m.lw(packet);
    link_with(m, ot, packet, tag);
    m.pop_frame(0x28);
    off(packet, 0xc)
}

/// 800D9024: a LINE_F2 packet (code 0x40, 0x42 blended by +0xB) from the
/// colour +8..+0xA and the points +0..+6, linked into `ot`.
pub fn line2(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    flat_packet(m, record, packet, ot, 0x40)
}

/// 800D9400: a TILE packet (code 0x60, 0x62 blended by +0xB): colour
/// +8..+0xA, position +0/+2 and size +4/+6, linked into `ot`.
pub fn tile(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    flat_packet(m, record, packet, ot, 0x60)
}

/// The 0x10-byte packets of [`line2`] and [`tile`].
fn flat_packet(m: &mut Machine, record: u32, packet: u32, ot: u32, code: u8) -> u32 {
    m.sb(off(packet, 3), 3);
    m.sb(off(packet, 7), code);
    let blend = lbu(m, off(record, 0xb));
    m.sb(off(packet, 7), if blend != 0 { code | 2 } else { code });
    for k in 0..3 {
        let c = lbu(m, off(record, 8 + k));
        m.sb(off(packet, 4 + k), c as u8);
    }
    for k in 0..3 {
        let v = lhu(m, off(record, 2 * k));
        m.sh(off(packet, 8 + 2 * k), v as u16);
    }
    let v = lhu(m, off(record, 6));
    let tag = m.lw(packet);
    m.sh(off(packet, 0xe), v as u16);
    link_with(m, ot, packet, tag);
    off(packet, 0x10)
}

/// 800D90E8: [`line2`] twice, the second one a line lower (the record's y
/// coordinates +2 and +6 incremented).
pub fn line2_double(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    m.push_frame(0x20);
    let next = line2(m, record, packet, ot);
    let y0 = lhu(m, off(record, 2)).wrapping_add(1);
    let y1 = lhu(m, off(record, 6)).wrapping_add(1);
    m.sh(off(record, 2), y0 as u16);
    m.sh(off(record, 6), y1 as u16);
    let end = line2(m, record, next, ot);
    m.pop_frame(0x20);
    end
}

/// 800D9140 (unreferenced): a LINE_F3 packet (code 0x48, terminator
/// 0x55555555; blended by +0xF) from the colour +0xC..+0xE and the points
/// +0..+0xA, linked into `ot`.
pub fn line3(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    polyline(m, record, packet, ot, 3)
}

/// 800D9294 (unreferenced): a LINE_F4 packet (code 0x4C; blended by +0x13)
/// from the colour +0x10..+0x12 and the points +0..+0xE, linked into `ot`.
pub fn line4(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    polyline(m, record, packet, ot, 4)
}

/// The packets of [`line3`] (`points` 3) and [`line4`] (4).
fn polyline(m: &mut Machine, record: u32, packet: u32, ot: u32, points: u32) -> u32 {
    let colour = 4 * points;
    m.sb(off(packet, 3), (points + 2) as u8);
    m.sb(off(packet, 7), (0x40 + 4 * (points - 1)) as u8);
    m.sw(off(packet, 8 + colour), 0x5555_5555);
    let blend = lbu(m, off(record, colour + 3));
    blend_code(m, packet, blend);
    let c = lbu(m, off(record, colour));
    let tag = m.lw(packet);
    m.sb(off(packet, 4), c as u8);
    for k in 1..3 {
        let c = lbu(m, off(record, colour + k));
        m.sb(off(packet, 4 + k), c as u8);
    }
    for k in 0..2 * points {
        let v = lhu(m, off(record, 2 * k));
        m.sh(off(packet, 8 + 2 * k), v as u16);
    }
    link_with(m, ot, packet, tag);
    off(packet, 0xc + colour)
}

/// 800D9230 / 800D939C (unreferenced): [`line3`] / [`line4`] twice, the
/// second one with the y coordinates +2, +0xA and +6 incremented.
pub fn polyline_double(m: &mut Machine, record: u32, packet: u32, ot: u32, points: u32) -> u32 {
    m.push_frame(0x20);
    let next = polyline(m, record, packet, ot, points);
    let y0 = lhu(m, off(record, 2));
    let y2 = lhu(m, off(record, 0xa));
    m.sh(off(record, 2), y0.wrapping_add(1) as u16);
    let y1 = lhu(m, off(record, 6));
    m.sh(off(record, 0xa), y2.wrapping_add(1) as u16);
    m.sh(off(record, 6), y1.wrapping_add(1) as u16);
    let end = polyline(m, record, next, ot, points);
    m.pop_frame(0x20);
    end
}

/// 800D94C4: a POLY_G4 packet (code 0x38, 0x3A blended by +0x13): the
/// colours +0x10, +0x14, +0x18, +0x1C and the points +0..+0xE.
pub fn poly_g4(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    gouraud(m, record, packet, ot, 4)
}

/// 800D9624: a POLY_G3 packet (code 0x30, 0x32 blended by +0x13): the
/// colours +0x10, +0x14, +0x18 and the points +0..+0xA.
pub fn poly_g3(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    gouraud(m, record, packet, ot, 3)
}

/// The packets of [`poly_g3`] (`points` 3) and [`poly_g4`] (4).
fn gouraud(m: &mut Machine, record: u32, packet: u32, ot: u32, points: u32) -> u32 {
    let code = if points == 4 { 0x38 } else { 0x30 };
    m.sb(off(packet, 3), (2 * points) as u8);
    m.sb(off(packet, 7), code);
    let blend = lbu(m, off(record, 0x13));
    m.sb(off(packet, 7), if blend != 0 { code | 2 } else { code });
    for point in 0..points {
        for k in 0..3 {
            let c = lbu(m, off(record, 0x10 + 4 * point + k));
            m.sb(off(packet, 4 + 8 * point + k), c as u8);
        }
    }
    for point in 0..points {
        let x = lhu(m, off(record, 4 * point));
        m.sh(off(packet, 8 + 8 * point), x as u16);
        let y = lhu(m, off(record, 4 * point + 2));
        if point == points - 1 {
            let tag = m.lw(packet);
            m.sh(off(packet, 0xa + 8 * point), y as u16);
            link_with(m, ot, packet, tag);
        } else {
            m.sh(off(packet, 0xa + 8 * point), y as u16);
        }
    }
    off(packet, 4 + 8 * points)
}

/// `x % 64 * 4` (C remainder) as the original's halfword shift computes it.
fn texture_u(x: u32) -> u32 {
    let rounded = if (x as i32) < 0 {
        x.wrapping_add(0x3f)
    } else {
        x
    };
    sra(x.wrapping_sub(sra(rounded, 6) << 6) << 16, 14)
}
/// `y % 256` (C remainder).
fn texture_v(y: u32) -> u32 {
    let rounded = if (y as i32) < 0 {
        y.wrapping_add(0xff)
    } else {
        y
    };
    y.wrapping_sub(sra(rounded, 8) << 8)
}

/// 800D9748: a POLY_FT4 packet (code 0x2C, 0x2E blended by +0x13): colour
/// +0x10..+0x12, the points +0..+0xE, the texture at +0x20/+0x22 (size
/// +0x24/+0x26, mode +0x17) and the CLUT at +0x28/+0x2A, linked into `ot`.
pub fn poly_ft4(m: &mut Machine, record: u32, packet: u32, ot: u32) -> u32 {
    m.sb(off(packet, 3), 9);
    m.sb(off(packet, 7), 0x2c);
    let blend = lbu(m, off(record, 0x13));
    m.sb(off(packet, 7), if blend != 0 { 0x2e } else { 0x2c });
    for k in 0..3 {
        let c = lbu(m, off(record, 0x10 + k));
        m.sb(off(packet, 4 + k), c as u8);
    }
    for (point, at) in [8, 0x10, 0x18, 0x20].into_iter().enumerate() {
        let x = lhu(m, off(record, 4 * point as u32));
        m.sh(off(packet, at), x as u16);
        let y = lhu(m, off(record, 4 * point as u32 + 2));
        m.sh(off(packet, at + 2), y as u16);
    }
    let u = |m: &mut Machine| {
        let x = lh(m, off(record, 0x20));
        texture_u(x)
    };
    let v = |m: &mut Machine| {
        let y = lh(m, off(record, 0x22));
        texture_v(y)
    };
    let u0 = u(m);
    m.sb(off(packet, 0xc), u0 as u8);
    let v0 = v(m);
    m.sb(off(packet, 0xd), v0 as u8);
    let u1 = u(m);
    let w = lbu(m, off(record, 0x24));
    m.sb(off(packet, 0x14), w.wrapping_add(u1) as u8);
    let v1 = v(m);
    m.sb(off(packet, 0x15), v1 as u8);
    let u2 = u(m);
    m.sb(off(packet, 0x1c), u2 as u8);
    let v2 = v(m);
    let h = lbu(m, off(record, 0x26));
    m.sb(off(packet, 0x1d), v2.wrapping_add(h) as u8);
    let u3 = u(m);
    let w = lbu(m, off(record, 0x24));
    m.sb(off(packet, 0x24), w.wrapping_add(u3) as u8);
    let v3 = v(m);
    let h = lbu(m, off(record, 0x26));
    m.sb(off(packet, 0x25), v3.wrapping_add(h) as u8);
    let mode = lbu(m, off(record, 0x17));
    let ty = lhu(m, off(record, 0x22));
    let tx = lhu(m, off(record, 0x20));
    let tpage =
        ((mode & 3) << 5) | sra((ty & 0x100) << 16, 20) | ((tx & 0x3ff) >> 6) | ((ty & 0x200) << 2);
    m.sh(off(packet, 0x16), tpage as u16);
    let cy = lhu(m, off(record, 0x2a));
    let cx = lhu(m, off(record, 0x28));
    m.sh(off(packet, 0xe), ((cy << 6) | ((cx >> 4) & 0x3f)) as u16);
    let tag = m.lw(packet);
    link_with(m, ot, packet, tag);
    off(packet, 0x28)
}

// ---- the cursor emblem -----------------------------------------------------------------

/// `v / 4096` with C (truncating) division.
fn q12(v: u32) -> u32 {
    let v = if (v as i32) < 0 {
        v.wrapping_add(0xfff)
    } else {
        v
    };
    sra(v, 12)
}
/// `v / 2` with C (truncating) division.
fn half(v: u32) -> u32 {
    sra(v.wrapping_add(v >> 31), 1)
}

/// 800D9A08: the two rotating cursor emblem quads at `x`, `y` (radius 7
/// turned by `angle`, radius 13 turned by `-angle`; textures {0x2E3, 0x94,
/// 7, 15} and {0x2E2, 0xA4, 13, 27}, CLUT 0x180, 0xA) as POLY_FT4 packets
/// from `packet`, linked into the visual block's ordering-table entry.
/// Returns the packets' end.
pub fn cursor_emblem(m: &mut Machine, x: u32, y: u32, angle: u32, packet: u32) -> u32 {
    let frame = m.push_frame(0x68);
    let ot = super::ov1_p2_visual::current_bin(m);
    let r = off(frame, 0x10);
    for (offset, value) in [(0x30, 0x2e3), (0x32, 0x94), (0x34, 7), (0x36, 0xf)] {
        m.sh(off(frame, offset), value);
    }
    m.sh(off(frame, 0x38), 0x180);
    m.sh(off(frame, 0x3a), 0xa);
    for k in 0..3 {
        m.sb(off(frame, 0x20 + k), 0x80);
    }
    m.sb(off(frame, 0x23), 0);
    m.sb(off(frame, 0x27), 0);
    let times7 = |v: u32| v.wrapping_mul(8).wrapping_sub(v);
    let neg = |v: u32| 0u32.wrapping_sub(v);
    // each corner calls rcos/rsin again, in the original's order
    let c = sdk::rcos(m, angle);
    let s = sdk::rsin(m, angle);
    let x0 = x.wrapping_add(half(q12(neg(times7(c))).wrapping_add(q12(neg(times7(s))))));
    m.sh(r, x0 as u16);
    let s = sdk::rsin(m, angle);
    let c = sdk::rcos(m, angle);
    let y0 = y.wrapping_add(q12(times7(s)).wrapping_add(q12(neg(times7(c)))));
    m.sh(off(r, 2), y0 as u16);
    let c = sdk::rcos(m, angle);
    let s = sdk::rsin(m, angle);
    let x1 = x.wrapping_add(half(q12(times7(c)).wrapping_add(q12(neg(times7(s))))));
    m.sh(off(r, 4), x1 as u16);
    let s = sdk::rsin(m, angle);
    let c = sdk::rcos(m, angle);
    let y1 = y.wrapping_add(q12(neg(times7(s))).wrapping_add(q12(neg(times7(c)))));
    m.sh(off(r, 6), y1 as u16);
    let c = sdk::rcos(m, angle);
    let s = sdk::rsin(m, angle);
    let x2 = x.wrapping_add(half(q12(neg(times7(c))).wrapping_add(q12(times7(s)))));
    m.sh(off(r, 8), x2 as u16);
    let s = sdk::rsin(m, angle);
    let c = sdk::rcos(m, angle);
    let y2 = y.wrapping_add(q12(times7(s)).wrapping_add(q12(times7(c))));
    m.sh(off(r, 0xa), y2 as u16);
    let c = sdk::rcos(m, angle);
    let s = sdk::rsin(m, angle);
    let x3 = x.wrapping_add(half(q12(times7(c)).wrapping_add(q12(times7(s)))));
    m.sh(off(r, 0xc), x3 as u16);
    let s = sdk::rsin(m, angle);
    let c = sdk::rcos(m, angle);
    let y3 = y.wrapping_add(q12(neg(times7(s))).wrapping_add(q12(times7(c))));
    m.sh(off(r, 0xe), y3 as u16);
    let next = poly_ft4(m, r, packet, ot);
    let back = neg(angle);
    for (offset, value) in [(0x30, 0x2e2), (0x32, 0xa4), (0x34, 0xd), (0x36, 0x1b)] {
        m.sh(off(frame, offset), value);
    }
    let times13 = |v: u32| {
        let t = (v << 1).wrapping_add(v);
        (t << 2).wrapping_add(v)
    };
    let c = sdk::rcos(m, back);
    let s = sdk::rsin(m, back);
    let x0 = x.wrapping_add(half(
        q12(neg(times13(c))).wrapping_add(q12(neg(times13(s)))),
    ));
    m.sh(r, x0 as u16);
    let s = sdk::rsin(m, back);
    let c = sdk::rcos(m, back);
    let y0 = y.wrapping_add(q12(neg(times13(neg(s)))).wrapping_add(q12(neg(times13(c)))));
    m.sh(off(r, 2), y0 as u16);
    let c = sdk::rcos(m, back);
    let s = sdk::rsin(m, back);
    let x1 = x.wrapping_add(half(q12(times13(c)).wrapping_add(q12(neg(times13(s))))));
    m.sh(off(r, 4), x1 as u16);
    let s = sdk::rsin(m, back);
    let c = sdk::rcos(m, back);
    let y1 = y.wrapping_add(q12(times13(neg(s))).wrapping_add(q12(neg(times13(c)))));
    m.sh(off(r, 6), y1 as u16);
    let c = sdk::rcos(m, back);
    let s = sdk::rsin(m, back);
    let x2 = x.wrapping_add(half(q12(neg(times13(c))).wrapping_add(q12(times13(s)))));
    m.sh(off(r, 8), x2 as u16);
    let s = sdk::rsin(m, back);
    let c = sdk::rcos(m, back);
    let y2 = y.wrapping_add(q12(neg(times13(neg(s)))).wrapping_add(q12(times13(c))));
    m.sh(off(r, 0xa), y2 as u16);
    let c = sdk::rcos(m, back);
    let s = sdk::rsin(m, back);
    let x3 = x.wrapping_add(half(q12(times13(c)).wrapping_add(q12(times13(s)))));
    m.sh(off(r, 0xc), x3 as u16);
    let s = sdk::rsin(m, back);
    let c = sdk::rcos(m, back);
    let y3 = y.wrapping_add(q12(times13(neg(s))).wrapping_add(q12(times13(c))));
    m.sh(off(r, 0xe), y3 as u16);
    let end = poly_ft4(m, r, next, ot);
    m.pop_frame(0x68);
    end
}

/// 800DA0E0 (unreferenced, debug): store the 320x480 screen into a 16-bit
/// TIM at 0x80200000 and write it to the PC file "sim:\usr\src\t_dp<n>.tim".
pub fn dump_screen(m: &mut Machine, number: u32) {
    let frame = m.push_frame(0x78);
    let rect = load_unaligned(m, SCREEN_RECT);
    let size = load_unaligned(m, off(SCREEN_RECT, 4));
    store_unaligned(m, off(frame, 0x10), rect);
    store_unaligned(m, off(frame, 0x14), size);
    m.sw(DUMP, 0x10);
    m.sw(off(DUMP, 4), 2);
    m.sw(off(DUMP, 8), 0x0002_580c);
    m.sw(off(DUMP, 0xc), 0);
    m.sw(off(DUMP, 0x10), 0x01e0_0140);
    m.sh(off(frame, 0x12), 0);
    call(m, STORE_IMAGE2, [off(frame, 0x10), off(DUMP, 0x14), 0, 0]);
    let name = off(frame, 0x18);
    call(m, VSPRINTF, [name, DUMP_NAME, number, 0]);
    let fd = bios::open(m, name, 0x202);
    bios::write(m, fd, DUMP, 0x0004_b014);
    bios::close(m, fd);
    m.pop_frame(0x78);
}

/// 800DA1B8: reset the context's mission records (+0x95C..: 180 bytes from
/// +0xD98, 80 from +0xD48, the 60-entry tables, scores 0xD2F0).
pub fn reset_records(m: &mut Machine) {
    let ctx = context(m);
    let base = off(ctx, 0x95c);
    for i in (0..=0xb3u32).rev() {
        m.sb(off(ctx, 0xa0f + 0x43c - (0xb3 - i)), 0);
    }
    for i in (0..=0x4fu32).rev() {
        m.sb(off(base, 0x3ec + i), 0);
    }
    for i in 0..0x3c {
        let at = off(base, i);
        for offset in [0x374, 0x338, 0x2fc, 0x2c0, 0x3b0] {
            m.sb(off(at, offset), 0);
        }
        let at = off(base, 2 * i);
        for offset in [0x1d0, 0x158, 0xe0, 0x68, 0x248] {
            m.sh(off(at, offset), 0xd2f0);
        }
        let at = off(base, 4 * i);
        m.sw(off(at, 0x4f0), 0);
        m.sw(off(at, 0x5e0), 0);
    }
    m.sw(off(base, 0x6d0), 0);
    m.sw(off(base, 0x6d4), 0);
    m.sw(off(base, 0x6d8), 0);
}

// ---- audio ------------------------------------------------------------------------------

/// 800DA25C: the audio buffer block (0x101C bytes; +0xC 0x3FFF, +0x10 0xFF).
pub fn init_audio(m: &mut Machine) {
    m.push_frame(0x18);
    let block = arena::alloc(m, 0x101c);
    m.sw(off(block, 0xc), 0x3fff);
    m.sw(off(block, 4), 0);
    m.sw(off(block, 8), 0);
    m.sw(block, 0);
    m.sw(off(block, 0x10), 0xff);
    m.sw(off(block, 0x14), 0);
    m.sw(AUDIO, block);
    m.pop_frame(0x18);
}

/// 800DA2A8 (JP; PAL 800D0358, changed): read the decoded CD audio (part
/// 5) into the block (+0x18) and return the half just completed: `side` 0
/// the left (+0xE0 or +0x18), 1 the right (+0x4E0 or +0x418), else 1.
pub fn decoded_audio(m: &mut Machine, side: u32) -> u32 {
    m.push_frame(0x20);
    let block = m.lw(AUDIO);
    let first = sdk::spu_read_decoded_data(m, off(block, 0x18), 5);
    sdk::spu_is_transfer_completed(m, 1);
    let block = m.lw(AUDIO);
    let (left, right) = if first == 0 {
        (off(block, 0xe0), off(block, 0x4e0))
    } else {
        (off(block, 0x18), off(block, 0x418))
    };
    m.pop_frame(0x20);
    match side {
        0 => left,
        1 => right,
        _ => 1,
    }
}

/// 800DA340 (JP; PAL 800D03E0, exact): the audio level: the mean of the
/// average absolute values of the first 100 samples of both halves.
pub fn audio_level(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let mut left = decoded_audio(m, 0);
    let mut right = decoded_audio(m, 1);
    let (mut sum_left, mut sum_right) = (0u32, 0u32);
    for _ in 0..100 {
        let a = lh(m, left);
        left = off(left, 2);
        let b = lh(m, right);
        right = off(right, 2);
        sum_left = sum_left.wrapping_add(if (a as i32) < 0 {
            0u32.wrapping_sub(a)
        } else {
            a
        });
        sum_right = sum_right.wrapping_add(if (b as i32) < 0 {
            0u32.wrapping_sub(b)
        } else {
            b
        });
    }
    let high = |v: u32| ((i64::from(v as i32) * 0x51eb_851f) >> 32) as u32;
    let mean_left = sra(high(sum_left), 5).wrapping_sub(sra(sum_left, 31));
    let mean_right = sra(high(sum_right), 5).wrapping_sub(sra(sum_right, 31));
    let total = mean_left.wrapping_add(mean_right);
    m.pop_frame(0x20);
    sra(total.wrapping_add(total >> 31) << 15, 16)
}

// ---- sounds -----------------------------------------------------------------------------

/// 800DA418: the sound block (12 bytes): +8 the relocated sound directory
/// `directory`, +0..+7 an empty history; then [`wait`].
pub fn init_sounds(m: &mut Machine, directory: u32) -> u32 {
    m.push_frame(0x18);
    let block = arena::alloc(m, 0xc);
    m.sw(SOUNDS, block);
    let list = loading::relocate_directory(m, directory);
    let block = m.lw(SOUNDS);
    m.sw(off(block, 8), list);
    for i in 0..8 {
        let block = m.lw(SOUNDS);
        m.sb(off(block, i), 0);
    }
    let v0 = wait();
    m.pop_frame(0x18);
    v0
}

/// 800DA484: a short busy loop; returns its counter's last value (-2).
pub fn wait() -> u32 {
    let mut v0: u32 = 5;
    v0 = v0.wrapping_sub(1);
    loop {
        let taken = (v0 as i32) >= 0;
        v0 = v0.wrapping_sub(1);
        if !taken {
            break;
        }
    }
    v0
}

/// 800DA49C: push `sound` into the history (the eight entries move up one),
/// then [`wait`].
pub fn push_sound(m: &mut Machine, sound: u32) -> u32 {
    m.push_frame(0x18);
    let block = m.lw(SOUNDS);
    for i in (0..=7u32).rev() {
        let at = off(i, block);
        let v = lbu(m, at);
        m.sb(off(at, 1), v as u8);
    }
    m.sb(block, sound as u8);
    let v0 = wait();
    m.pop_frame(0x18);
    v0
}

// ---- the disc prompt ----------------------------------------------------------------------

fn disc(m: &mut Machine) -> u32 {
    m.lw(DISC)
}

/// 800DA4DC (JP; PAL 800D04F4, changed; unreferenced): the prompt state.
pub fn prompt_state(m: &mut Machine) -> u32 {
    let block = disc(m);
    m.lw(block)
}

/// 800DA4F4 (JP; PAL 800CEB80 of overlay 2, exact; unreferenced): the
/// required disc's index.
pub fn set_required(m: &mut Machine, index: u32) {
    let block = disc(m);
    m.sw(off(block, 8), index);
}

/// 800DA504 (JP; PAL 800D050C, changed; table 0x800DD914): reset the
/// prompt. Returns 1.
pub fn reset_prompt(m: &mut Machine) -> u32 {
    let block = disc(m);
    m.sw(off(block, 4), 0);
    m.sw(block, 0);
    1
}

/// 800DA520 (table 0x800DD918): one step of the disc check against the
/// required disc: 0 waiting (no disc: state 1), 2 a wrong disc or a final
/// state (state 0 or 6), 1 the state set from the disc read and the lid
/// checked (8002B428).
pub fn check_disc(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = check_disc_body(m);
    m.pop_frame(0x18);
    result
}

fn check_disc_body(m: &mut Machine) -> u32 {
    let number = call(m, DISC_NUMBER, [0; 4]);
    if number == 0 {
        let block = disc(m);
        m.sw(block, 1);
        return 0;
    }
    let block = disc(m);
    let index = m.lw(off(block, 8));
    let required = lbu(m, off(DISC_OF, index));
    if required == 3 || number == 3 {
        m.sw(block, 0);
        return 2;
    }
    if number == 4 || number == u32::MAX {
        if required == 1 {
            let state = if m.lw(off(block, 4)) == 0 { 2 } else { 4 };
            m.sw(block, state);
        }
        let block = disc(m);
        let index = m.lw(off(block, 8));
        if lbu(m, off(DISC_OF, index)) == 2 {
            let state = if m.lw(off(block, 4)) == 0 { 3 } else { 5 };
            m.sw(block, state);
        }
    } else if number == 1 {
        if required == number {
            m.sw(block, 6);
            return 2;
        }
        let state = if m.lw(off(block, 4)) == 0 { 3 } else { 5 };
        m.sw(block, state);
    } else if number == 2 {
        if required == number {
            m.sw(block, 6);
            return 2;
        }
        let state = if m.lw(off(block, 4)) == 0 { 2 } else { 4 };
        m.sw(block, state);
    } else {
        return 0;
    }
    call(m, LID_CHECK, [0; 4]);
    1
}

/// 800DA694 (JP; PAL 800D0D04, changed; table 0x800DD91C): whether the
/// disc state (8002B528) is 3.
pub fn disc_state_is_3(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = call(m, DISC_STATE, [0; 4]);
    m.pop_frame(0x18);
    u32::from(state ^ 3 == 0)
}

/// 800DA6B8 (JP; PAL 800D25A0, changed; table 0x800DD920): when the disc
/// state is 1, prompt state 1 with +4 set: 4, else 0.
pub fn disc_state_ready(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = call(m, DISC_STATE, [0; 4]);
    let result = if state == 1 {
        let block = disc(m);
        m.sw(block, state);
        m.sw(off(block, 4), state);
        4
    } else {
        0
    };
    m.pop_frame(0x18);
    result
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !loaded(m) {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_8af0 => load_file(m, a0, a1, a2, a3),
        0x800d_8ba8 => upload_group(m, a0),
        0x800d_8c30 => {
            upload_directory(m, a0);
            0
        }
        0x800d_8cec => upload_one(m, a0),
        0x800d_8d64 => {
            upload_placed(m, a0, a1, a2, a3);
            0
        }
        0x800d_8e38 => {
            upload_with_clut(m, a0, a1, a2);
            0
        }
        0x800d_8f3c => draw_mode(m, a0, a1, a2),
        0x800d_9024 => line2(m, a0, a1, a2),
        0x800d_90e8 => line2_double(m, a0, a1, a2),
        0x800d_9140 => line3(m, a0, a1, a2),
        0x800d_9230 => polyline_double(m, a0, a1, a2, 3),
        0x800d_9294 => line4(m, a0, a1, a2),
        0x800d_939c => polyline_double(m, a0, a1, a2, 4),
        0x800d_9400 => tile(m, a0, a1, a2),
        0x800d_94c4 => poly_g4(m, a0, a1, a2),
        0x800d_9624 => poly_g3(m, a0, a1, a2),
        0x800d_9748 => poly_ft4(m, a0, a1, a2),
        0x800d_9a08 => cursor_emblem(m, a0, a1, a2, a3),
        0x800d_a0e0 => {
            dump_screen(m, a0);
            0
        }
        0x800d_a1b8 => {
            reset_records(m);
            0
        }
        0x800d_a25c => {
            init_audio(m);
            0
        }
        0x800d_a2a8 => decoded_audio(m, a0),
        0x800d_a340 => audio_level(m),
        0x800d_a418 => init_sounds(m, a0),
        0x800d_a484 => wait(),
        0x800d_a49c => push_sound(m, a0),
        0x800d_a4dc => prompt_state(m),
        0x800d_a4f4 => {
            set_required(m, a0);
            0
        }
        0x800d_a504 => reset_prompt(m),
        0x800d_a520 => check_disc(m),
        0x800d_a694 => disc_state_is_3(m),
        0x800d_a6b8 => disc_state_ready(m),
        _ => return None,
    })
}
