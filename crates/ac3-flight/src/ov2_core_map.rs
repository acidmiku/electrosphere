//! Overlay 2 map and background block (`[0x800F9890]`, 0x124 bytes),
//! translated from the original instructions.
//!
//! [`initialize`] (800EAF0C) allocates the block and its arrays: +0/+4 two
//! 0x400-byte buffers, +0x80 64 8-byte and +0x84 64 16-byte ring records,
//! +0xDC/+0xE0 48 star records, +0xF0/+0xF4 three 15-entry trails, +0xF8
//! five anchor points, +0x104 16 circle points. +0xC counts frames, +0x14
//! enables the map (800E9748), +0x18 its mode (800E9824 and friends) and
//! the stars (800E9A54), +0x1C the third layer (800EAE74), +0xBC the
//! palette kept after loading, +0x110..+0x120 the background voices.
//! [`draw`] (800EB014) runs the enabled layers with the map projection
//! (offset 160x240, H 250, depth cue) into the scene packet area.
use super::{MAP, call, framed, sext16};
use crate::{
    arena,
    machine::{Machine, SmallData},
    sdk,
};
use ac3_gte::ctrl;

fn block(m: &mut Machine) -> u32 {
    m.lw(MAP)
}
fn rand(m: &mut Machine) -> i32 {
    call(m, 0x800a_c178, [0; 4]) as i32
}
fn rsin(m: &mut Machine, angle: u32) -> i32 {
    call(m, 0x800a_45c8, [angle, 0, 0, 0]) as i32
}
fn rcos(m: &mut Machine, angle: u32) -> i32 {
    call(m, 0x800a_4698, [angle, 0, 0, 0]) as i32
}
/// The compiler's signed division by a constant: high word of the product
/// with `magic`, arithmetic shift, minus the dividend's sign.
fn magic_quotient(value: i32, magic: u32, shift: u32) -> i32 {
    let high = ((i64::from(value) * i64::from(magic as i32)) >> 32) as i32;
    (high >> shift).wrapping_sub(value >> 31)
}
/// Truncated signed division by `1 << bits` (bias, `sra`).
fn divide_pow2(value: i32, bits: u32) -> i32 {
    let biased = if value < 0 {
        value.wrapping_add((1 << bits) - 1)
    } else {
        value
    };
    biased >> bits
}
/// Truncated signed remainder by `1 << bits`.
fn remainder_pow2(value: i32, bits: u32) -> i32 {
    value.wrapping_sub(divide_pow2(value, bits) << bits)
}

// ---- 800EAF0C: initialization ------------------------------------------------------------------

/// 800EAF0C: allocate and reset the map block: arrays, 16 circle points
/// (rcos/rsin of i*256 halved), counters, then 800E7E58, 800E986C and
/// 800EA598 (whose v0, 0x200, it returns).
pub fn initialize(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let map = arena::alloc(m, 0x124);
        m.sw(MAP, map);
        for (bytes, offset) in [
            (0x200, 0x80),
            (0x400, 0x84),
            (0x60, 0xdc),
            (0x300, 0xe0),
            (0x80, 0x104),
        ] {
            let array = arena::alloc(m, bytes);
            m.sw(map + offset, array);
        }
        for point in 0..0x10u32 {
            let angle = point << 8;
            let cosine = rcos(m, angle);
            let at = m.lw(map + 0x104).wrapping_add(point << 3);
            m.sh(
                at,
                (cosine.wrapping_add(((cosine as u32) >> 31) as i32) >> 1) as u16,
            );
            let sine = rsin(m, angle);
            let at = m.lw(map + 0x104).wrapping_add(point << 3);
            m.sh(
                at + 2,
                (sine.wrapping_add(((sine as u32) >> 31) as i32) >> 1) as u16,
            );
            let at = m.lw(map + 0x104).wrapping_add(point << 3);
            m.sh(at + 4, 0);
        }
        m.sw(map + 0xc, 0);
        m.sw(map + 0x14, 0);
        m.sw(map + 0x18, 0);
        m.sw(map + 0x1c, 0);
        reset_rings(m, map);
        scatter_stars(m, map);
        reset_trails(m, map)
    })
}

/// 800E7E58: map state: +0x78 200, +0x70 0x100, +0x6C/+0x7C/+0x72 0,
/// +0x108 0x200, +0x10A 0, +0xC 0; the sprite page/CLUT/uv/size at +0xB0
/// from the texture rectangle 0x800F6800 (CLUT 0x800F6808); the two
/// 0x400-byte buffers; 64 ring records (depth 0x5DA8 - i*3125/8).
pub fn reset_rings(m: &mut Machine, map: u32) {
    framed(m, 0x20, |m, _| {
        m.sw(map + 0x78, 0xc8);
        m.sh(map + 0x70, 0x100);
        m.sh(map + 0x6c, 0);
        m.sw(map + 0x7c, 0);
        m.sh(map + 0x72, 0);
        m.sh(map + 0x108, 0x200);
        m.sh(map + 0x10a, 0);
        m.sw(map + 0xc, 0);
        let rect = 0x800f_6800;
        let y = u32::from(m.lhu(rect + 2));
        let x = u32::from(m.lhu(rect));
        let page =
            (((((y & 0x100) << 16) as i32) >> 20) as u32) | ((x & 0x3ff) >> 6) | ((y & 0x200) << 2);
        let sprite = map + 0xb0;
        m.sh(sprite, page as u16);
        let clut_y = u32::from(m.lhu(rect + 0xa));
        let clut_x = u32::from(m.lhu(rect + 8));
        m.sh(sprite + 2, ((clut_y << 6) | ((clut_x >> 4) & 0x3f)) as u16);
        let x = i32::from(m.lh(rect));
        m.sh(sprite + 4, ((remainder_pow2(x, 6) << 16) >> 14) as u16);
        let y = i32::from(m.lh(rect + 2));
        m.sh(sprite + 6, remainder_pow2(y, 8) as u16);
        m.sh(sprite + 8, 0x20);
        m.sh(sprite + 0xa, 0x20);
        for buffer in 0..2u32 {
            let data = arena::alloc(m, 0x400);
            m.sw(map + buffer * 4, data);
        }
        for ring in 0..0x40u32 {
            let short = m.lw(map + 0x80).wrapping_add(ring << 3);
            m.sw(short, 0);
            let short = m.lw(map + 0x80).wrapping_add(ring << 3);
            m.sw(short + 4, 0);
            let long = m.lw(map + 0x84).wrapping_add(ring << 4);
            m.sw(long, 0);
            let long = m.lw(map + 0x84).wrapping_add(ring << 4);
            m.sw(long + 4, 0);
            let long = m.lw(map + 0x84).wrapping_add(ring << 4);
            let step = (ring.wrapping_mul(0xc35) << 3) as i32;
            m.sw(
                long + 8,
                0x5da8i32.wrapping_sub(divide_pow2(step, 6)) as u32,
            );
        }
    })
}

/// 800E986C: 48 stars: brightness rand mod 64 (+0xDC halfwords), x/y =
/// cos/sin(rand) * (rand mod 1536) / 4096 pushed out by 0x180, depth rand
/// mod 1562 (+0xE0 records of 16 bytes).
pub fn scatter_stars(m: &mut Machine, map: u32) {
    framed(m, 0x28, |m, _| {
        for star in 0..0x30u32 {
            let record = star << 4;
            let value = rand(m);
            let at = m.lw(map + 0xdc).wrapping_add(star << 1);
            m.sh(at, remainder_pow2(value, 6) as u16);
            let angle = rand(m) as u32 & 0xfff;
            let cosine = rcos(m, angle);
            let value = rand(m);
            let radius =
                value.wrapping_sub(magic_quotient(value, 0x2aaa_aaab, 8).wrapping_mul(0x600));
            let x = divide_pow2(cosine.wrapping_mul(radius), 12);
            let at = m.lw(map + 0xe0).wrapping_add(record);
            m.sw(at, x as u32);
            let angle = rand(m) as u32 & 0xfff;
            let sine = rsin(m, angle);
            let value = rand(m);
            let radius =
                value.wrapping_sub(magic_quotient(value, 0x2aaa_aaab, 8).wrapping_mul(0x600));
            let y = divide_pow2(sine.wrapping_mul(radius), 12);
            let at = m.lw(map + 0xe0).wrapping_add(record);
            m.sw(at + 4, y as u32);
            let value = rand(m);
            let depth =
                value.wrapping_sub(magic_quotient(value, 0x14fa_6d7b, 7).wrapping_mul(0x61a));
            let at = m.lw(map + 0xe0).wrapping_add(record);
            m.sw(at + 8, depth as u32);
            for offset in [0u32, 4] {
                let at = m.lw(map + 0xe0).wrapping_add(record);
                let value = m.lw(at + offset) as i32;
                let value = if value >= 0 {
                    value.wrapping_add(0x180)
                } else {
                    value.wrapping_sub(0x180)
                };
                m.sw(at + offset, value as u32);
            }
        }
    })
}

/// 800EA598: three trails (800EA038; +0xF0 15 angle records, +0xF4 15
/// position records each), five anchors (+0xF8: origin, then cos/sin of
/// -0x200 + i*0x400 / 32), +0xFC 0, +0x100 0x200. Returns 0x200.
pub fn reset_trails(m: &mut Machine, map: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        let angles = arena::alloc(m, 0xc);
        m.sw(map + 0xf0, angles);
        let positions = arena::alloc(m, 0xc);
        m.sw(map + 0xf4, positions);
        let anchors = arena::alloc(m, 0x28);
        m.sw(map + 0xf8, anchors);
        for trail in 0..3u32 {
            let angles = arena::alloc(m, 0x78);
            let at = m.lw(map + 0xf0).wrapping_add(trail << 2);
            m.sw(at, angles);
            let positions = arena::alloc(m, 0xf0);
            let at = m.lw(map + 0xf4).wrapping_add(trail << 2);
            m.sw(at, positions);
            reset_trail(m, trail, map);
        }
        let anchors = m.lw(map + 0xf8);
        m.sw(anchors, 0);
        let anchors = m.lw(map + 0xf8);
        m.sw(anchors + 4, 0);
        let mut angle = (-0x200i32) as u32;
        for anchor in 1..5u32 {
            let cosine = rcos(m, angle);
            let sine = rsin(m, angle);
            let at = m.lw(map + 0xf8).wrapping_add(anchor << 3);
            let x = divide_pow2(cosine, 5) as u32 & 0xffff;
            let y = (divide_pow2(sine, 5) as u32) << 16;
            m.sw(at, x | y);
            let at = m.lw(map + 0xf8).wrapping_add(anchor << 3);
            m.sw(at + 4, 0);
            angle = angle.wrapping_add(0x400);
        }
        m.sw(map + 0xfc, 0);
        m.sw(map + 0x100, 0x200);
        0x200
    })
}

/// 800EA038: trail `index`: radius r = rand & 0x3FFF; one of four starts
/// (rand & 3): ±0.64 r across with a random offset along, or ±0.96 r along
/// with a random offset across; random angles (masks 0x1FF/0xDFF/0x5FF and
/// 0xFFF); then the 14 following records copy the first.
pub fn reset_trail(m: &mut Machine, index: u32, map: u32) {
    framed(m, 0x30, |m, _| {
        const MAGIC: u32 = 0x1062_4dd3;
        let radius = rand(m) & 0x3fff;
        let across = magic_quotient(radius.wrapping_mul(160), MAGIC, 4);
        let along = magic_quotient(radius.wrapping_mul(240), MAGIC, 4);
        let kind = rand(m) & 3;
        let slot = index << 2;
        let position = |m: &mut Machine| {
            let table = m.lw(map + 0xf4);
            m.lw(table.wrapping_add(slot))
        };
        let angle = |m: &mut Machine| {
            let table = m.lw(map + 0xf0);
            m.lw(table.wrapping_add(slot))
        };
        let (first, mask) = match kind {
            0 | 1 => {
                let x = if kind == 0 {
                    sext16(across as i16)
                } else {
                    (-i32::from(across as i16)) as u32
                };
                let value = rand(m);
                let offset = remainder_pow2(value, 9)
                    .wrapping_sub(0x100)
                    .wrapping_mul(radius);
                let y = magic_quotient(offset, MAGIC, 4);
                let at = position(m);
                m.sw(at, x);
                let at = position(m);
                m.sw(at + 4, y as u32);
                let at = position(m);
                m.sw(at + 8, radius as u32);
                (0x1ff, if kind == 0 { 0xdff } else { 0x5ff })
            }
            _ => {
                let value = rand(m);
                let offset = value
                    .wrapping_sub(magic_quotient(value, 0x6666_6667, 7).wrapping_mul(0x140))
                    .wrapping_sub(0xa0)
                    .wrapping_mul(radius);
                let x = magic_quotient(offset, MAGIC, 4);
                let at = position(m);
                m.sw(at, x as u32);
                let y = i32::from(along as i16);
                let y = if kind == 2 { y } else { y.wrapping_neg() };
                let at = position(m);
                m.sw(at + 4, y as u32);
                let at = position(m);
                m.sw(at + 8, radius as u32);
                (if kind == 2 { 0xdff } else { 0x5ff }, 0xfff)
            }
        };
        let value = rand(m) as u32;
        let at = angle(m);
        m.sh(at, (value & first) as u16);
        let value = rand(m) as u32;
        let at = angle(m);
        m.sh(at + 2, (value & mask) as u16);
        let at = angle(m);
        m.sh(at + 4, 0);
        for entry in 1..0xfu32 {
            for offset in [0u32, 2, 4] {
                let at = angle(m).wrapping_add(entry << 3);
                let value = m.lhu(at + offset - 8);
                m.sh(at + offset, value);
            }
            for offset in [0u32, 4, 8] {
                let at = position(m).wrapping_add(entry << 4);
                let value = m.lw(at + offset - 0x10);
                m.sw(at + offset, value);
            }
        }
    })
}

// ---- 800E7AB4 / 800E7E20 / 800E8008 ----------------------------------------------------------

/// 800E7AB4: select the map ordering table of the display buffer
/// (`[map + buffer*4]`, 256 entries, at +8), clear it and chain it under
/// the entry at `entry` (entry 255 first). Returns the link to entry 255
/// (`(ot + 0x3FC) & 0xFFFFFF`).
pub fn prepare_ot(m: &mut Machine, entry: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let ctx = super::context(m);
        let buffer = m.lw(ctx + 4);
        let map = block(m);
        let ot = m.lw(map.wrapping_add(buffer << 2));
        m.sw(map + 8, ot);
        sdk::clear_otag_r(m, ot, 0x100);
        let map = block(m);
        let ot = m.lw(map + 8);
        let link = m.lw(entry);
        super::link_word(m, ot, link);
        let ot = m.lw(map + 8);
        super::link_word(m, entry, ot.wrapping_add(0x3fc));
        ot.wrapping_add(0x3fc) & 0x00ff_ffff
    })
}
/// 800E7E20: keep the map palette: StoreImage2 of the CLUT rectangle
/// 0x800F6808 into +0xBC, then DrawSync(0) (whose v0 it returns).
pub fn keep_palette(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let map = block(m);
        call(m, 0x800a_8e9c, [0x800f_6808, map + 0xbc, 0, 0]);
        call(m, 0x800a_6664, [0, 0, 0, 0])
    })
}
/// 800E8008: start sounds 0x5D..0x61 (voices at +0x110..+0x120) and set
/// each to level 0 over 0 ticks (8002DB18). Returns the last call's v0.
pub fn start_background_voices(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        for (sound, offset) in [
            (0x5d, 0x110),
            (0x5e, 0x114),
            (0x5f, 0x118),
            (0x60, 0x11c),
            (0x61, 0x120),
        ] {
            let voice = call(m, 0x8002_d724, [sound, 0, 0, 0]);
            let map = block(m);
            m.sw(map + offset, voice);
        }
        let mut v0 = 0;
        for offset in [0x110, 0x114, 0x118, 0x11c, 0x120] {
            let map = block(m);
            let voice = m.lw(map + offset);
            v0 = call(m, 0x8002_db18, [voice, 0, 0, 0]);
        }
        v0
    })
}

// ---- 800EB014: drawing --------------------------------------------------------------------------

/// 800EB014: map projection (OFX 160, OFY 240 in 16.16, H 250, DQA
/// -0x2BB0, DQB 0x1555000); the map layer (800E9748) when +0x14, the
/// stars (800E9A54) when +0x18, otherwise the third layer (800EAE74) when
/// +0x1C; counts the frame (+0xC). Returns the packet pointer.
pub fn draw(m: &mut Machine, packet: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let map = block(m);
        m.gte.ctc2(ctrl::OFX, 0x00a0_0000);
        m.gte.ctc2(ctrl::OFY, 0x00f0_0000);
        m.gte.ctc2(ctrl::H, 0xfa);
        m.gte.ctc2(ctrl::DQA, (-0x2bb0i32) as u32);
        m.gte.ctc2(ctrl::DQB, 0x0155_5000);
        let layer = m.lw(map + 0x14);
        let stars = m.lw(map + 0x18);
        let mut packet = packet;
        if layer != 0 {
            packet = draw_layer(m, map, 0x1f80_0000, packet);
        }
        if stars != 0 {
            packet = draw_stars(m, map, 0x1f80_0000, packet);
        }
        if layer == 0 && stars == 0 && m.lw(map + 0x1c) != 0 {
            packet = draw_ribbon_layer(m, map, 0x1f80_0000, packet);
        }
        let frame = m.lw(map + 0xc);
        m.sw(map + 0xc, frame.wrapping_add(1));
        packet
    })
}

// ---- GTE helpers ----------------------------------------------------------------------------

/// `lwc2 reg, address`.
fn lwc2(m: &mut Machine, reg: usize, address: u32) {
    let value = m.lw(address);
    m.gte.mtc2(reg, value);
}
/// `swc2 reg, address` (the `mfc2` read with its storage side effect).
fn swc2(m: &mut Machine, reg: usize, address: u32) {
    let value = m.gte.mfc2(reg);
    m.sw(address, value);
}
/// The inlined libgte SetRotMatrix/SetTransMatrix pair: control registers
/// 0..7 from the MATRIX at `matrix` (rotation words, then translation).
fn load_rot_trans(m: &mut Machine, matrix: u32) {
    for (index, offset) in [
        (0, 0),
        (1, 4),
        (2, 8),
        (3, 0xc),
        (4, 0x10),
        (5, 0x14),
        (6, 0x18),
        (7, 0x1c),
    ] {
        let value = m.lw(matrix + offset);
        m.gte.ctc2(index, value);
    }
}
/// Signed `div`-free quotient by 390 (`0xA80A80A9` multiply-high).
fn div390(value: i32) -> i32 {
    let high = ((i64::from(value) * i64::from(0xa80a_80a9u32 as i32)) >> 32) as i32;
    (high.wrapping_add(value) >> 8).wrapping_sub(value >> 31)
}
/// `25000 * value / 64` as compiled (shift-add multiply, truncating divide).
fn ring_rise(value: i32) -> i32 {
    divide_pow2(value.wrapping_mul(25000), 6)
}

// ---- 800E9748 / 800E80D8: map layer ----------------------------------------------------------

/// 800E9748: the map layer: path (800E80D8), rings (800E8574), tunnel
/// (800E9060); in mode 1 (reveal) / 3 (hide) the reveal counter +0x20
/// ends at 96 and switches the mode to 2 / 0. Returns the packet pointer.
pub fn draw_layer(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        update_path(m, map, work);
        let packet = draw_rings(m, map, work, packet);
        let packet = draw_tunnel(m, map, work, packet);
        let mode = m.lw(map + 0x14);
        if mode == 1 || mode == 3 {
            let reveal = m.lw(map + 0x20) as i32;
            if divide_pow2(reveal, 3) >= 0xc {
                m.sw(map + 0x20, 0);
                m.sw(map + 0x14, if mode == 1 { 2 } else { 0 });
            }
        }
        packet
    })
}

/// 800E80D8: advance the camera along the ring path. Speed +0x70 (0x200
/// while revealing or hiding, else 0x80 + |frame mod 512 - 256|) accumulates
/// in +0x72; every 390 units shift the 64-entry ring history by one
/// (angles +0x80, positions +0x84 relative to entry 62) and generate the
/// new rings with RotMatrix_gte and MVMVA; the reveal (+0x20) and star
/// (+0x24) counters advance by the same steps. The camera interpolates
/// between rings 62 and 63 (GPF, INTPL toward ring 42/43), looks along
/// the path (two ratan2) and stores its matrices (+0x4C, transposed +0x2C)
/// and translations (+0x40, +0x60, +0xA0, work +0x1AC). Returns the view
/// translation's z (work +0x1B4).
pub fn update_path(m: &mut Machine, map: u32, work: u32) -> u32 {
    framed(m, 0x38, |m, _| {
        let mode = m.lw(map + 0x14);
        let speed = if mode == 1 || mode == 3 {
            0x200
        } else {
            let offset = ((m.lw(map + 0xc) & 0x1ff) as i32).wrapping_sub(0x100);
            offset.wrapping_abs().wrapping_add(0x80) as u32
        };
        m.sh(map + 0x70, speed as u16);
        let sum = u32::from(m.lhu(map + 0x72)).wrapping_add(u32::from(m.lhu(map + 0x70)));
        let value = i32::from(sum as u16 as i16);
        m.sh(map + 0x72, sum as u16);
        let steps = div390(value);
        m.sh(map + 0x74, steps as u16);
        let steps = i32::from(m.lh(map + 0x74));
        m.sh(
            map + 0x72,
            value.wrapping_sub(div390(value).wrapping_mul(390)) as u16,
        );
        if steps != 0 {
            let phase = m.lw(map + 0x7c).wrapping_add(steps as u32);
            let mode = m.lw(map + 0x14);
            m.sw(map + 0x7c, phase & 7);
            if mode == 1 || mode == 3 {
                let steps = sext16(m.lh(map + 0x74));
                let reveal = m.lw(map + 0x20);
                m.sw(map + 0x20, reveal.wrapping_add(steps));
            }
            let stars = m.lw(map + 0x18);
            if stars == 1 || stars == 3 {
                let steps = sext16(m.lh(map + 0x74));
                let counter = m.lw(map + 0x24);
                m.sw(map + 0x24, counter.wrapping_add(steps));
            }
            shift_rings(m, map);
            generate_rings(m, map, work);
        }
        camera(m, map, work);
        m.lw(work + 0x1b4)
    })
}

/// 800E8244..800E82C4: shift the ring history by `[+0x74]` entries.
fn shift_rings(m: &mut Machine, map: u32) {
    let steps = i32::from(m.lh(map + 0x74));
    let angles = m.lw(map + 0x80);
    let positions = m.lw(map + 0x84);
    let base = positions + 0x3e0;
    let origin = [m.lw(base), m.lw(base + 4), m.lw(base + 8)];
    let mut index = 0x40i32;
    if steps >= index {
        return;
    }
    loop {
        index -= 1;
        let to = index as u32;
        let from = to - 1;
        let first = m.lw(angles + from * 8);
        let second = m.lw(angles + from * 8 + 4);
        m.sw(angles + to * 8, first);
        m.sw(angles + to * 8 + 4, second);
        let x = m.lw(positions + from * 16);
        let y = m.lw(positions + from * 16 + 4);
        let z = m.lw(positions + from * 16 + 8);
        m.sw(positions + to * 16, x.wrapping_sub(origin[0]));
        m.sw(positions + to * 16 + 4, y.wrapping_sub(origin[1]));
        m.sw(positions + to * 16 + 8, z.wrapping_sub(origin[2]));
        if steps >= index {
            break;
        }
    }
}

/// 800E82C8..800E83B4: generate the `[+0x74]` newest rings, newest last:
/// angles (rsin of the running counter +0x6C, x >> 6, y >> 5) and
/// positions (previous ring + rotation of (0, 0, 0x186)).
fn generate_rings(m: &mut Machine, map: u32, work: u32) {
    let mut count = i32::from(m.lh(map + 0x74));
    let mut angle = sext16(m.lh(map + 0x6c));
    let angles = m.lw(map + 0x80);
    let mut ring = angles.wrapping_add((count as u32) << 3).wrapping_sub(8);
    let positions = m.lw(map + 0x84);
    let mut position = positions.wrapping_add((count as u32) << 4);
    m.gte.mtc2(0, 0);
    m.gte.mtc2(1, 0x186);
    let matrix = work + 0x1bc;
    angle = angle.wrapping_add(1);
    loop {
        let sine = rsin(m, angle << 4);
        m.sh(ring, (sine >> 6) as u16);
        m.sh(ring + 2, (sine >> 5) as u16);
        m.sh(ring + 4, 0);
        call(m, 0x800a_5e78, [ring + 8, matrix, 0, 0]);
        for (index, offset) in [(0, 0), (1, 4), (2, 8), (3, 0xc), (4, 0x10)] {
            let value = m.lw(matrix + offset);
            m.gte.ctc2(index, value);
        }
        let y = m.lw(position + 4);
        let z = m.lw(position + 8);
        let x = m.lw(position);
        m.gte.ctc2(5, x);
        m.gte.ctc2(6, y);
        m.gte.ctc2(7, z);
        count -= 1;
        ring = ring.wrapping_sub(8);
        m.gte.mvmva(true, false, 0, 0, 0);
        position = position.wrapping_sub(0x10);
        swc2(m, 25, position);
        swc2(m, 26, position + 4);
        swc2(m, 27, position + 8);
        if count <= 0 {
            break;
        }
        angle = angle.wrapping_add(1);
    }
    m.sh(map + 0x6c, angle as u16);
}

/// 800E83B8..800E8540: the camera between rings 62 and 63.
fn camera(m: &mut Machine, map: u32, work: u32) {
    let fraction = i32::from(m.lh(map + 0x72)) << 12;
    let positions = m.lw(map + 0x84);
    let base = positions + 0x3e0;
    let x = m.lw(base);
    let z = m.lw(base + 8);
    let next_z = m.lw(base + 0x18);
    let next_x = m.lw(base + 0x10);
    let dz = z.wrapping_sub(next_z);
    let y = m.lw(base + 4);
    let next_y = m.lw(base + 0x14);
    let dx = x.wrapping_sub(next_x);
    let dy = y.wrapping_sub(next_y);
    m.gte.mtc2(8, div390(fraction) as u32);
    m.gte.mtc2(9, dx);
    m.gte.mtc2(10, dy);
    m.gte.mtc2(11, dz);
    m.sh(work + 0x198, 0);
    m.sh(map + 0x8c, 0);
    m.gte.gpf(true, false);
    let positions = m.lw(map + 0x84);
    let ahead = positions + 0x2a0;
    let px = m.gte.mfc2(25);
    let py = m.gte.mfc2(26);
    let pz = m.gte.mfc2(27);
    lwc2(m, 9, ahead);
    lwc2(m, 10, ahead + 4);
    lwc2(m, 11, ahead + 8);
    let far = positions + 0x2b0;
    for (index, offset) in [(21, 0), (22, 4), (23, 8)] {
        let value = m.lw(far + offset);
        m.gte.ctc2(index, value);
    }
    m.sw(map + 0x90, px);
    m.sw(map + 0x94, py);
    m.gte.intpl(true, false);
    m.sw(map + 0x98, pz);
    for (offset, scratch, value) in [(0x60, 0x1ac, px), (0x64, 0x1b0, py), (0x68, 0x1b4, pz)] {
        let negated = value.wrapping_neg();
        m.sw(map + offset, negated);
        m.sw(work + scratch, negated);
    }
    let tx = m.gte.mfc2(25);
    let ty = m.gte.mfc2(26);
    let tz = m.gte.mfc2(27);
    let along = tz.wrapping_sub(pz);
    let pitch = call(m, 0x800a_6088, [ty.wrapping_sub(py), along, 0, 0]);
    m.sh(work + 0x194, pitch as u16);
    m.sh(map + 0x88, pitch as u16);
    let yaw = call(m, 0x800a_6088, [tx.wrapping_sub(px), along, 0, 0]);
    m.sh(work + 0x196, yaw as u16);
    m.sh(map + 0x8a, yaw as u16);
    call(m, 0x800a_5958, [work + 0x194, map + 0x4c, 0, 0]);
    call(m, 0x800a_5918, [map + 0x4c, map + 0x2c, 0, 0]);
    call(m, 0x800a_4d78, [map + 0x2c, work + 0x1ac, work + 0x1ac, 0]);
    let x = m.lw(work + 0x1ac);
    let y = m.lw(work + 0x1b0);
    let z = m.lw(work + 0x1b4);
    m.sw(map + 0x40, x);
    m.sw(map + 0xa0, x);
    m.sw(map + 0x44, y);
    m.sw(map + 0xa4, y);
    m.sw(map + 0x48, z);
    m.sw(map + 0xa8, z);
}

// ---- 800E8574: rings ------------------------------------------------------------------------------

/// The map texture: rectangle 0x800F6800 (x, y), CLUT 0x800F6808.
const MAP_TEXTURE: u32 = 0x800f_6800;

/// Texture page and CLUT words of the map texture as 800E8574/800E9060
/// compute them.
fn map_page_clut(m: &mut Machine) -> (u32, u32) {
    let y = u32::from(m.lhu(MAP_TEXTURE + 2));
    let x = u32::from(m.lhu(MAP_TEXTURE));
    let page = (((((y & 0x100) << 16) as i32) >> 20) as u32)
        | ((x & 0x3ff) >> 6)
        | (((((y & 0x200) << 16) as i32) >> 14) as u32);
    let clut_y = sext16(m.lh(MAP_TEXTURE + 0xa));
    let clut_x = u32::from(m.lhu(MAP_TEXTURE + 8));
    (page, (clut_y << 6) | ((clut_x >> 4) & 0x3f))
}
/// `u` of the map texture: (x mod 64) * 4.
fn map_u(m: &mut Machine) -> u32 {
    let x = i32::from(m.lh(MAP_TEXTURE));
    ((remainder_pow2(x, 6) << 16) >> 14) as u32
}

/// Ring height of circle point `point` (800E870C..800E880C and its two
/// copies): rings rise from the reveal row while revealing (mode 1, row of
/// the previous ring `previous`) or sink while hiding (mode 3, row of
/// this ring `current`); other rings keep their height. `circle` is
/// `[map+0x104]`.
fn ring_height(m: &mut Machine, map: u32, point: u32, previous: i32, current: i32) {
    let reveal = m.lw(map + 0x20) as i32;
    let row = divide_pow2(reveal, 3);
    if row == previous {
        if m.lw(map + 0x14) != 1 {
            let circle = m.lw(map + 0x104);
            m.sh(point.wrapping_add(circle) + 4, 0);
            return;
        }
        let circle = m.lw(map + 0x104);
        let step = 7i32.wrapping_sub(reveal.wrapping_sub(previous.wrapping_mul(8)));
        m.sh(point.wrapping_add(circle) + 4, ring_rise(step) as u16);
    } else if row == current {
        if m.lw(map + 0x14) != 3 {
            let circle = m.lw(map + 0x104);
            m.sh(point.wrapping_add(circle) + 4, 0);
            return;
        }
        let circle = m.lw(map + 0x104);
        let step = remainder_pow2(reveal.wrapping_neg(), 3);
        m.sh(point.wrapping_add(circle) + 4, ring_rise(step) as u16);
    }
}

/// The ring-quad projection pipeline shared by 800E8574 and 800E9060:
/// 16 circle points (`[map+0x104]`, 8 bytes each) in batches of three
/// (RTPT) and one single (RTPS), stored in the work area from +4: screen
/// XY at +4, SZ at +0x84, depth cue (IR0 of a batch, MAC0 of the single)
/// at +0x104 (one word per point; a batch value also covers the two slots
/// before it). `height` runs before each point is loaded (800E8574 raises
/// the points; 800E9060 leaves them). The two-point RTPT tail is the
/// original's (unreachable with 16 points).
fn project_circle(m: &mut Machine, map: u32, work: u32, mut height: impl FnMut(&mut Machine, u32)) {
    let mut index = 0u32;
    let mut point = 0u32;
    let mut latest = (-4i32) as u32;
    let mut oldest = (-8i32) as u32;
    loop {
        height(m, point);
        let circle = m.lw(map + 0x104);
        lwc2(m, 0, circle.wrapping_add(point));
        lwc2(m, 1, circle.wrapping_add(point) + 4);
        latest = latest.wrapping_add(4);
        oldest = oldest.wrapping_add(4);
        index += 1;
        point = point.wrapping_add(8);
        if index >= 0x10 {
            m.gte.rtps(true, false);
            let at = work + (index << 2);
            swc2(m, 14, at);
            swc2(m, 19, at + 0x80);
            swc2(m, 24, at + 0x100);
            return;
        }
        height(m, point);
        let circle = m.lw(map + 0x104);
        lwc2(m, 2, circle.wrapping_add(point));
        lwc2(m, 3, circle.wrapping_add(point) + 4);
        latest = latest.wrapping_add(4);
        oldest = oldest.wrapping_add(4);
        index += 1;
        point = point.wrapping_add(8);
        if index >= 0x10 {
            m.gte.rtpt(true, false);
            let at = work.wrapping_add(latest);
            swc2(m, 12, at);
            swc2(m, 13, at + 4);
            let depth = work + (index << 2) + 0x7c;
            let cue = m.gte.mfc2(24);
            swc2(m, 17, depth);
            swc2(m, 18, depth + 4);
            m.sw(work.wrapping_add(latest) + 0x104, cue);
            m.sw(work.wrapping_add(oldest) + 0x104, cue);
            return;
        }
        height(m, point);
        let circle = m.lw(map + 0x104);
        lwc2(m, 4, circle.wrapping_add(point));
        lwc2(m, 5, circle.wrapping_add(point) + 4);
        latest = latest.wrapping_add(4);
        oldest = oldest.wrapping_add(4);
        point = point.wrapping_add(8);
        index += 1;
        m.gte.rtpt(true, false);
        let at = work.wrapping_add(oldest);
        swc2(m, 12, at);
        swc2(m, 13, at + 4);
        swc2(m, 14, at + 8);
        let depth = work + (index << 2) + 0x78;
        let cue = m.gte.mfc2(8);
        swc2(m, 17, depth);
        swc2(m, 18, depth + 4);
        swc2(m, 19, depth + 8);
        m.sw(work.wrapping_add(latest) + 0x104, cue);
        m.sw(work.wrapping_add(oldest) + 0x104, cue);
        m.sw(work + ((index - 3) << 2) + 0x104, cue);
        if index >= 0x10 {
            return;
        }
    }
}

/// Keep this ring's projection (+4, +0x84, +0x104) as the previous one
/// (+0x44, +0xC4, +0x144).
fn keep_previous(m: &mut Machine, work: u32) {
    for point in 0..0x10u32 {
        let at = work + (point << 2);
        let xy = m.lw(at + 4);
        let depth = m.lw(at + 0x84);
        let cue = m.lw(at + 0x104);
        m.sw(at + 0x44, xy);
        m.sw(at + 0xc4, depth);
        m.sw(at + 0x144, cue);
    }
}

/// Whether all four screen points of a quad lie beyond one screen edge
/// (x < 0, y < 0, x > 320, y > 512, tested in that order).
fn quad_offscreen(m: &mut Machine, points: [u32; 4]) -> bool {
    let mut all =
        |offset: u32, outside: fn(i16) -> bool| points.iter().all(|&p| outside(m.lh(p + offset)));
    all(0, |v| v < 0) || all(2, |v| v < 0) || all(0, |v| v >= 0x141) || all(2, |v| v >= 0x201)
}

/// Link a 9-word primitive into map ordering-table bucket `bucket`
/// (clamped at 255; negative buckets index below the table as compiled).
fn link_bucket(m: &mut Machine, ot: u32, bucket: i32, prim: u32) {
    let bucket = if bucket >= 0x100 { 0xff } else { bucket };
    let entry = ot.wrapping_add((bucket as u32) << 2);
    let link = m.lw(entry);
    m.sw(prim, link | 0x0900_0000);
    m.sw(entry, (prim << 8) >> 8);
}

/// 800E8574: the seven rings every 8 history entries, projected with each
/// ring's matrix (RotMatrix_gte of its angles, TransMatrix of its position,
/// CompMatrix with the camera +0x2C) and raised or lowered by the reveal;
/// from the second ring on POLY_FT4 quads (code 0x2C, color 0x838383, map
/// texture, v scrolling with +0x7C) between it and the previous ring,
/// culled off-screen and by NCLIP, at the previous ring's depth; then the
/// ring's palette upload. Returns the packet pointer.
pub fn draw_rings(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x50, |m, sp| {
        let mut packet = packet;
        let (page, clut) = map_page_clut(m);
        m.sw(sp + 0x10, page);
        m.sw(sp + 0x14, clut);
        let y = i32::from(m.lh(MAP_TEXTURE + 2));
        let v = i32::from(remainder_pow2(y, 8) as i16);
        let ot = m.lw(map + 8);
        m.sw(sp + 0x1c, ot);
        let scroll = ((m.lw(map + 0x7c) << 2) & 0x1f) as i32 - 0x20;
        m.sw(sp + 0x18, v.wrapping_sub(scroll) as u32);
        let (mut current, mut previous) = (0i32, -1i32);
        let mut record = 0u32;
        loop {
            let matrix = work + 0x1bc;
            let global = m.lw(super::MAP);
            let angles = m.lw(global + 0x80);
            call(
                m,
                0x800a_5e78,
                [angles.wrapping_add(record << 3), matrix, 0, 0],
            );
            let positions = m.lw(map + 0x84);
            call(
                m,
                0x800a_53a8,
                [matrix, positions.wrapping_add(record << 4), 0, 0],
            );
            call(m, 0x800a_4c18, [map + 0x2c, matrix, work + 0x1dc, 0]);
            load_rot_trans(m, work + 0x1dc);
            project_circle(m, map, work, |m, point| {
                ring_height(m, map, point, previous, current)
            });
            if current != 0 {
                for index in [21, 22, 23] {
                    m.gte.ctc2(index, 0x800);
                }
                let mut prim = packet;
                let top = m.lw(sp + 0x18);
                let near = top << 8;
                let far = top.wrapping_add(0x1f) << 8;
                for point in 0..0x10u32 {
                    let mode = m.lw(map + 0x14);
                    if mode == 1 && divide_pow2(m.lw(map + 0x20) as i32, 3) < previous {
                        break;
                    }
                    if mode == 3 && previous < divide_pow2(m.lw(map + 0x20) as i32, 3) {
                        break;
                    }
                    let next = remainder_pow2(point as i32 + 1, 4) as u32;
                    let old = work + (point << 2) + 0x44;
                    let old_next = work + (next << 2) + 0x44;
                    let new = work + (point << 2) + 4;
                    let new_next = work + (next << 2) + 4;
                    if quad_offscreen(m, [old, old_next, new, new_next]) {
                        continue;
                    }
                    let a = m.lw(old);
                    let c = m.lw(new);
                    let d = m.lw(new_next);
                    m.gte.mtc2(12, a);
                    m.gte.mtc2(13, c);
                    m.gte.mtc2(14, d);
                    m.sw(prim + 8, a);
                    m.sw(prim + 0x18, c);
                    m.gte.nclip();
                    m.sw(prim + 4, 0x2c83_8383);
                    let b = m.lw(old_next);
                    m.sw(prim + 0x20, d);
                    m.sw(prim + 0x10, b);
                    if (m.gte.mfc2(24) as i32) < 0 {
                        continue;
                    }
                    let u = map_u(m);
                    let clut = m.lw(sp + 0x14);
                    m.sw(prim + 0xc, u | near | (clut << 16));
                    let u = map_u(m);
                    let page = m.lw(sp + 0x10);
                    m.sw(prim + 0x14, u.wrapping_add(0x1f) | near | (page << 16));
                    let u = map_u(m);
                    m.sw(prim + 0x1c, u | far);
                    let u = map_u(m);
                    m.sw(prim + 0x24, u.wrapping_add(0x1f) | far);
                    let depth = m.lw(work + 0xc4 + (point << 2)) as i32;
                    let ot = m.lw(sp + 0x1c);
                    link_bucket(m, ot, depth >> 8, prim);
                    prim = prim.wrapping_add(0x28);
                }
                packet = upload_palette(m, map, work, prim, sp);
            }
            keep_previous(m, work);
            previous += 1;
            current += 1;
            record += 8;
            if current >= 7 {
                break;
            }
        }
        packet
    })
}

/// 800E8EDC..800E8FD8: the ring palette upload at `prim` (13 words: A0
/// copy of 16x1 to the CLUT rectangle 0x800F6808, the 16 colors of +0xBC
/// faded toward the background color by the first depth cue with INTPL,
/// cache clear) into map bucket (previous depth >> 8) + 5. Returns the
/// packet pointer after it.
fn upload_palette(m: &mut Machine, map: u32, work: u32, prim: u32, sp: u32) -> u32 {
    m.sw(prim, 0x0c00_0000);
    m.sw(prim + 4, 0xa000_0000);
    let x = sext16(m.lh(0x800f_6808));
    let y = sext16(m.lh(0x800f_680a));
    m.sw(prim + 0x30, 0x0100_0000);
    let display = m.gw(super::DISPLAY);
    m.sw(prim + 0xc, 0x0001_0010);
    m.sw(prim + 8, x | (y << 16));
    for (index, offset) in [(21, 0x10), (22, 0x11), (23, 0x12)] {
        let channel = u32::from(m.lbu(display + offset));
        m.gte.ctc2(index, channel << 4);
    }
    for color in 0..0x10u32 {
        let value = sext16(m.lh(map + 0xbc + color * 2));
        let cue = m.lw(work + 0x104);
        m.gte.mtc2(28, value);
        m.gte.mtc2(8, cue);
        m.gte.intpl(true, false);
        let packed = m.gte.mfc2(29);
        m.sh(prim + 0x10 + color * 2, packed as u16);
    }
    let mut bucket = ((m.lw(work + 0xc4) as i32) >> 8).wrapping_add(5);
    if bucket >= 0x100 {
        bucket = 0xff;
    }
    let ot = m.lw(sp + 0x1c);
    let entry = ot.wrapping_add((bucket as u32) << 2);
    super::add_prim(m, entry, prim);
    prim.wrapping_add(0x34)
}

// ---- 800E9060: tunnel ----------------------------------------------------------------------

/// 800E9060: the tunnel: 16 rings from history entry 0x30 on, projected
/// with their matrices; from the second on POLY_FT4 quads to the previous
/// ring (map texture, v from the ring row and +0x7C), culled off-screen,
/// at the previous ring's depth. Revealing (mode 1) draws rows up to the
/// reveal counter - 0x30, hiding (mode 3) rows from it. Returns the packet
/// pointer.
pub fn draw_tunnel(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x40, |m, sp| {
        let mut packet = packet;
        let ot = m.lw(map + 8);
        m.sw(sp + 0x14, ot);
        let scroll = (m.lw(map + 0x7c) & 7) << 2;
        m.sw(sp + 0x10, scroll);
        let (page, clut) = map_page_clut(m);
        for row in 0..0x10i32 {
            let record = 0x30 + row as u32;
            let matrix = work + 0x1bc;
            let angles = m.lw(map + 0x80);
            call(
                m,
                0x800a_5e78,
                [angles.wrapping_add(record << 3), matrix, 0, 0],
            );
            let positions = m.lw(map + 0x84);
            call(
                m,
                0x800a_53a8,
                [matrix, positions.wrapping_add(record << 4), 0, 0],
            );
            call(m, 0x800a_4c18, [map + 0x2c, matrix, work + 0x1dc, 0]);
            load_rot_trans(m, work + 0x1dc);
            project_circle(m, map, work, |_, _| {});
            if row != 0 {
                packet = tunnel_quads(m, map, work, packet, row, sp, page, clut);
            }
            keep_previous(m, work);
        }
        packet
    })
}

/// 800E92CC..800E96C4: one tunnel row's quads.
#[allow(clippy::too_many_arguments)]
fn tunnel_quads(
    m: &mut Machine,
    map: u32,
    work: u32,
    packet: u32,
    row: i32,
    sp: u32,
    page: u32,
    clut: u32,
) -> u32 {
    for index in [21, 22, 23] {
        m.gte.ctc2(index, 0x800);
    }
    let mut prim = packet;
    for point in 0..0x10u32 {
        let mode = m.lw(map + 0x14);
        if mode == 1 && (m.lw(map + 0x20) as i32).wrapping_sub(0x30) < row {
            break;
        }
        if mode == 3 && row < (m.lw(map + 0x20) as i32).wrapping_sub(0x30) {
            break;
        }
        let next = remainder_pow2(point as i32 + 1, 4) as u32;
        let new = work + (point << 2) + 4;
        let new_next = work + (next << 2) + 4;
        let old = new + 0x40;
        let old_next = new_next + 0x40;
        if quad_offscreen(m, [old, old_next, new, new_next]) {
            continue;
        }
        let a = m.lw(old);
        let b = m.lw(old_next);
        m.sw(prim + 8, a);
        m.sw(prim + 0x10, b);
        let c = m.lw(new);
        let d = m.lw(new_next);
        m.sw(prim + 4, 0x2c83_8383);
        m.sw(prim + 0x18, c);
        m.sw(prim + 0x20, d);
        let scroll = m.lw(sp + 0x10);
        let band = ((((row - 1) as u32) & 7) << 2).wrapping_sub(scroll) & 0x1f;
        let v = |m: &mut Machine| {
            let y = i32::from(m.lh(MAP_TEXTURE + 2));
            band.wrapping_add(i32::from(remainder_pow2(y, 8) as i16) as u32)
        };
        let u = map_u(m);
        let near = v(m) << 8;
        m.sw(prim + 0xc, u | near | (clut << 16));
        let u = map_u(m);
        let near = v(m) << 8;
        m.sw(prim + 0x14, u.wrapping_add(0x1f) | near | (page << 16));
        let u = map_u(m);
        let far = v(m).wrapping_add(7) << 8;
        m.sw(prim + 0x1c, u | far);
        let u = map_u(m);
        let far = v(m).wrapping_add(7) << 8;
        m.sw(prim + 0x24, u.wrapping_add(0x1f) | far);
        let depth = m.lw(work + 0xc4 + (point << 2)) as i32;
        let ot = m.lw(sp + 0x14);
        link_bucket(m, ot, depth >> 8, prim);
        prim = prim.wrapping_add(0x28);
    }
    prim
}

// ---- 800E9A54: stars -----------------------------------------------------------------------

/// 800E9A54: the 48 stars: each star's ring index (+0xDC) advances with the
/// path steps (mod 62); stars shown by the star mode (+0x18: 1 appearing
/// up to the counter +0x24, 3 disappearing from it) are projected with the
/// camera, their ring and their offset (RotMatrix, TransMatrix,
/// CompMatrix twice, RTPS of the origin) and drawn as POLY_FT4 squares of
/// 0xFA00 / depth pixels (at least 2) colored by DPCS of 0xB4B4B4 toward
/// the background (code 0x2C), texture rectangle 0x800F6810 (CLUT
/// 0x800F6818); the counter ending at 64 switches mode 1/3 to 2/0.
/// Returns the packet pointer.
pub fn draw_stars(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x40, |m, sp| {
        const TEXTURE: u32 = 0x800f_6810;
        let mut packet = packet;
        let steps = sext16(m.lh(map + 0x74));
        let ot = m.lw(map + 8);
        m.sw(sp + 0x10, ot);
        if steps != 0 {
            for star in 0..0x30u32 {
                let at = m.lw(map + 0xdc).wrapping_add(star << 1);
                let mut ring = sext16(m.lh(at)).wrapping_add(steps) as i32;
                if ring >= 0x3e {
                    let high = ((i64::from(ring) * i64::from(0x8421_0843u32 as i32)) >> 32) as i32;
                    let quotient = (high.wrapping_add(ring) >> 5).wrapping_sub(ring >> 31);
                    ring = ring.wrapping_sub(quotient.wrapping_mul(62));
                }
                m.sh(at, ring as u16);
            }
        }
        let display = m.gw(super::DISPLAY);
        for (index, offset) in [(21, 0x10), (22, 0x11), (23, 0x12)] {
            let channel = u32::from(m.lbu(display + offset));
            m.gte.ctc2(index, channel << 4);
        }
        m.gte.mtc2(6, 0x2cb4_b4b4);
        let camera = work + 0x44;
        m.sw(sp + 0x14, camera);
        let local = work + 0x64;
        for star in 0..0x30u32 {
            let prim = packet;
            let mode = m.lw(map + 0x18);
            let rings = m.lw(map + 0xdc);
            if mode == 1
                && (m.lw(map + 0x24) as i32) < i32::from(m.lh(rings.wrapping_add(star << 1)))
            {
                continue;
            }
            if mode == 3 && i32::from(m.lh(rings.wrapping_add(star << 1))) < m.lw(map + 0x24) as i32
            {
                continue;
            }
            let global = m.lw(super::MAP);
            call(m, 0x800a_5958, [global + 0x88, camera, 0, 0]);
            let global = m.lw(super::MAP);
            let offsets = m.lw(global + 0xe0);
            call(
                m,
                0x800a_53a8,
                [camera, offsets.wrapping_add(star << 4), 0, 0],
            );
            let rings = m.lw(map + 0xdc);
            let ring = sext16(m.lh(rings.wrapping_add(star << 1)));
            let angles = m.lw(map + 0x80);
            call(
                m,
                0x800a_5958,
                [angles.wrapping_add(ring << 3), local, 0, 0],
            );
            let positions = m.lw(map + 0x84);
            call(
                m,
                0x800a_53a8,
                [local, positions.wrapping_add(ring << 4), 0, 0],
            );
            call(m, 0x800a_4c18, [local, camera, work + 4, 0]);
            call(m, 0x800a_4c18, [map + 0x2c, work + 4, work + 0x24, 0]);
            load_rot_trans(m, work + 0x24);
            m.gte.mtc2(0, 0);
            m.gte.mtc2(1, 0);
            m.gte.rtps(true, false);
            star_texture(m, TEXTURE, prim);
            let depth = m.gte.mfc2(19) as i32;
            swc2(m, 14, work + 0xa0);
            if depth <= 0 {
                continue;
            }
            m.gte.dpcs(true, false);
            let mut size = super::visual::quotient(m, 0xfa00, depth, 0x800e_9e80);
            if size < 2 {
                size = 2;
            }
            let sx = i32::from(m.lh(work + 0xa0));
            m.sw(work, (depth >> 8) as u32);
            let left = sx.wrapping_sub(size >> 2);
            let half = size >> 1;
            let sy = i32::from(m.lh(work + 0xa2));
            let right = left.wrapping_add(half);
            let top = sy.wrapping_sub(half);
            if right < 0 || left >= 0x141 {
                continue;
            }
            let bottom = top.wrapping_add(size);
            if bottom < 0 || top >= 0x201 {
                continue;
            }
            let upper = (top as u32) << 16;
            m.sw(prim + 0x10, upper | right as u32);
            let lower = (bottom as u32) << 16;
            m.sw(prim + 8, upper | left as u32);
            m.sw(prim + 0x18, lower | left as u32);
            m.sw(prim + 0x20, lower | right as u32);
            swc2(m, 22, prim + 4);
            let bucket = m.lw(work) as i32;
            let ot = m.lw(sp + 0x10);
            link_bucket(m, ot, bucket, prim);
            packet = prim.wrapping_add(0x28);
        }
        let mode = m.lw(map + 0x18);
        if (mode == 1 || mode == 3) && m.lw(map + 0x24) as i32 >= 0x40 {
            m.sw(map + 0x24, 0);
            m.sw(map + 0x18, if mode == 1 { 2 } else { 0 });
        }
        packet
    })
}

/// 800E9CA0..800E9E58: the star square's texture words: uv0 + CLUT
/// (0x800F6818), uv1 + page, uv2/uv3 halfwords (16x16 from the texture
/// rectangle's x mod 64 * 4, y mod 256).
fn star_texture(m: &mut Machine, texture: u32, prim: u32) {
    let y = i32::from(m.lh(texture + 2));
    let clut_y = sext16(m.lh(0x800f_681a));
    let clut_x = u32::from(m.lhu(0x800f_6818));
    let clut = ((clut_y << 6) | ((clut_x >> 4) & 0x3f)) << 16;
    let v = ((remainder_pow2(y, 8) << 16) >> 8) as u32;
    let x = i32::from(m.lh(texture));
    let u = ((remainder_pow2(x, 6) << 16) >> 14) as u32;
    m.sw(prim + 0xc, clut | v | u);
    let ty = u32::from(m.lhu(texture + 2));
    let tx = u32::from(m.lhu(texture));
    let page = (((((ty & 0x100) << 16) as i32) >> 20) as u32)
        | ((tx & 0x3ff) >> 6)
        | (sext16((ty & 0x200) as u16 as i16) << 2);
    let v = ((remainder_pow2(i32::from(ty as u16 as i16), 8) << 16) >> 8) as u32;
    let u = ((remainder_pow2(i32::from(tx as u16 as i16), 6) << 16) >> 14) as u32;
    m.sw(prim + 0x14, (page << 16) | v | u.wrapping_add(0xf));
    let y = i32::from(m.lh(texture + 2));
    let v = (remainder_pow2(y, 8).wrapping_add(0xf) as u32) << 8;
    let x = i32::from(m.lh(texture));
    let u = ((remainder_pow2(x, 6) << 16) >> 14) as u32;
    m.sh(prim + 0x1c, (v | u) as u16);
    let y = i32::from(m.lh(texture + 2));
    let v = (remainder_pow2(y, 8).wrapping_add(0xf) as u32) << 8;
    let x = i32::from(m.lh(texture));
    let u = ((remainder_pow2(x, 6) << 16) >> 14) as u32;
    m.sh(prim + 0x24, (v | u.wrapping_add(0xf)) as u16);
}

// ---- 800EAE74: ribbon layer ---------------------------------------------------------------

/// 800EAE74: the ribbon layer (not reached on the hub-briefing-launch
/// path): mode +0x1C 1 becomes 2; mode 3 counts +0x28 to 60, then ends
/// (0); then the camera (800EA6CC) and the ribbons (800EAAFC). Returns the
/// packet pointer.
pub fn draw_ribbon_layer(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let mut mode = m.lw(map + 0x1c);
        if mode == 1 {
            m.sw(map + 0x1c, 2);
            mode = m.lw(map + 0x1c);
        }
        if mode == 3 {
            let count = m.lw(map + 0x28);
            if (count as i32) < 0x3c {
                m.sw(map + 0x28, count.wrapping_add(1));
            } else {
                m.sw(map + 0x1c, 0);
            }
        }
        ribbon_camera(m, map, work);
        draw_ribbons(m, map, work, packet)
    })
}

/// 800EA6CC: a fixed camera for the ribbons: angles and position +0x88..
/// +0x98 cleared, rotation +0x4C (RotMatrix), translation = -position
/// (work +0xD8, TransMatrix), copied to +0x2C and transposed there, the
/// view translation +0xA0 = transpose * -position (ApplyMatrixLV) set on
/// +0x2C (TransMatrix). Returns that last TransMatrix's v0 (map +0x2C).
pub fn ribbon_camera(m: &mut Machine, map: u32, work: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        for offset in [0x88, 0x8c, 0x90, 0x94, 0x98] {
            m.sw(map + offset, 0);
        }
        call(m, 0x800a_5958, [map + 0x88, map + 0x4c, 0, 0]);
        for (from, to) in [(0x90, 0xd8), (0x94, 0xdc), (0x98, 0xe0)] {
            let value = m.lw(map + from);
            m.sw(work + to, value.wrapping_neg());
        }
        call(m, 0x800a_53a8, [map + 0x4c, work + 0xd8, 0, 0]);
        let first: Vec<u32> = (0..4).map(|i| m.lw(map + 0x4c + i * 4)).collect();
        for (i, word) in first.into_iter().enumerate() {
            m.sw(map + 0x2c + i as u32 * 4, word);
        }
        let second: Vec<u32> = (4..8).map(|i| m.lw(map + 0x4c + i * 4)).collect();
        for (i, word) in second.into_iter().enumerate() {
            m.sw(map + 0x3c + i as u32 * 4, word);
        }
        call(m, 0x800a_5918, [map + 0x4c, map + 0x2c, 0, 0]);
        call(m, 0x800a_4d78, [map + 0x2c, work + 0xd8, map + 0xa0, 0]);
        call(m, 0x800a_53a8, [map + 0x2c, map + 0xa0, 0, 0])
    })
}

/// 800EAAFC: the three ribbons: each trail's history shifts by one
/// (positions +0xF4, angles +0xF0), the head turns by three random steps
/// (rand & 0x1F) and advances by (0, 0, 0x200) rotated by it (MVMVA); a
/// trail whose positions all left the view box (800EAA34) restarts
/// (800EA038), otherwise its 15 segments are projected (800EA7D0 of the
/// five anchors +0xF8) and drawn as gouraud quads (800EA900) between
/// consecutive segments. A DR_MODE (blend 0) opens the layer in bucket 0
/// and one (blend 1) closes it in bucket 255. Returns the packet pointer.
pub fn draw_ribbons(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x30, |m, _| {
        let ot = m.lw(map + 8);
        let mut packet = ribbon_mode(m, 0, ot, packet);
        let mut slot = 0u32;
        for trail in 0..3u32 {
            let positions = |m: &mut Machine| {
                let table = m.lw(map + 0xf4);
                m.lw(table.wrapping_add(slot))
            };
            let angles = |m: &mut Machine| {
                let table = m.lw(map + 0xf0);
                m.lw(table.wrapping_add(slot))
            };
            let mut entry = 0xeu32;
            loop {
                for offset in [0u32, 4, 8] {
                    let at = positions(m).wrapping_add(entry << 4);
                    let value = m.lw(at.wrapping_add(offset).wrapping_sub(0x10));
                    m.sw(at + offset, value);
                }
                for offset in [0u32, 2, 4] {
                    let at = angles(m).wrapping_add(entry << 3);
                    let value = m.lhu(at.wrapping_add(offset).wrapping_sub(8));
                    m.sh(at + offset, value);
                }
                entry -= 1;
                if entry == 0 {
                    break;
                }
            }
            for offset in [0u32, 2, 4] {
                let step = rand(m) as u32;
                let head = angles(m);
                let value = u32::from(m.lhu(head + offset)).wrapping_add(step & 0x1f);
                m.sh(head + offset, value as u16);
            }
            let head = angles(m);
            call(m, 0x800a_5958, [head, work, 0, 0]);
            let next = positions(m);
            call(m, 0x800a_53a8, [work, next + 0x10, 0, 0]);
            load_rot_trans(m, work);
            lwc2(m, 0, map + 0xfc);
            lwc2(m, 1, map + 0x100);
            m.gte.mvmva(true, false, 0, 0, 0);
            let head = positions(m);
            swc2(m, 25, head);
            swc2(m, 26, head + 4);
            swc2(m, 27, head + 8);
            let head = positions(m);
            if ribbon_visible(m, head) == 0 {
                reset_trail(m, trail, map);
                slot += 4;
                continue;
            }
            for segment in 0..0xfu32 {
                let angle = angles(m).wrapping_add(segment << 3);
                call(m, 0x800a_5958, [angle, work, 0, 0]);
                let position = positions(m).wrapping_add(segment << 4);
                call(m, 0x800a_53a8, [work, position, 0, 0]);
                call(m, 0x800a_4c18, [map + 0x2c, work, work + 0x40, 0]);
                let anchors = m.lw(map + 0xf8);
                project_anchors(m, anchors, work, 5);
                if segment != 0 {
                    packet = ribbon_quads(m, map, work + 0x60, segment - 1, packet);
                }
                for word in 0..5u32 {
                    let value = m.lw(work + 0x60 + word * 4);
                    m.sw(work + 0x74 + word * 4, value);
                }
            }
            slot += 4;
        }
        let ot = m.lw(map + 8);
        ribbon_mode(m, 1, ot.wrapping_add(0x3fc), packet)
    })
}

/// 800EA53C: a DR_MODE with blend mode `mode & 3` (SetDrawMode(p, 0, 0,
/// mode << 5, null)) added at `entry`; returns the packet after it.
pub fn ribbon_mode(m: &mut Machine, mode: u32, entry: u32, prim: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        m.sw(sp + 0x10, 0);
        call(m, 0x800a_ac38, [prim, 0, 0, (mode & 3) << 5]);
        super::add_prim(m, entry, prim);
        prim.wrapping_add(0xc)
    })
}

/// 800EA7D0: project `count` SVECTOR points with the matrix at `work +
/// 0x40` (RTPT in threes; a remainder of one or two still runs RTPT) into
/// the screen XY words at `work + 0x60`. Returns the last output address
/// for a remainder of one or two, else 0 (the loop test).
pub fn project_anchors(m: &mut Machine, points: u32, work: u32, count: u32) -> u32 {
    let out = work + 0x60;
    load_rot_trans(m, work + 0x40);
    let mut at = points;
    let (mut done, mut latest, mut older) = (0i32, (-4i32) as u32, (-8i32) as u32);
    loop {
        lwc2(m, 0, at);
        lwc2(m, 1, at + 4);
        at = at.wrapping_add(8);
        latest = latest.wrapping_add(4);
        done += 1;
        older = older.wrapping_add(4);
        if done >= count as i32 {
            m.gte.rtpt(true, false);
            swc2(m, 12, out.wrapping_add(latest));
            return out.wrapping_add(latest);
        }
        lwc2(m, 2, at);
        lwc2(m, 3, at + 4);
        at = at.wrapping_add(8);
        latest = latest.wrapping_add(4);
        done += 1;
        older = older.wrapping_add(4);
        if done >= count as i32 {
            m.gte.rtpt(true, false);
            swc2(m, 12, out.wrapping_add(older));
            swc2(m, 13, out.wrapping_add(latest));
            return out.wrapping_add(latest);
        }
        lwc2(m, 4, at);
        lwc2(m, 5, at + 4);
        at = at.wrapping_add(8);
        m.gte.rtpt(true, false);
        latest = latest.wrapping_add(4);
        older = older.wrapping_add(4);
        done += 1;
        let first = out.wrapping_add(((done as u32) << 2).wrapping_sub(0xc));
        swc2(m, 12, first);
        swc2(m, 13, out.wrapping_add(older));
        swc2(m, 14, out.wrapping_add(latest));
        if done >= count as i32 {
            return 0;
        }
    }
}

/// 800EA900: four semi-transparent gouraud quads (POLY_G4, code 0x3A) of
/// segment `segment` fanning from the first anchor of this (`points`) and
/// the previous segment (`points + 0x14`), gray levels 64 - segment * 64 /
/// 15 and 64 - (segment + 1) * 64 / 15, into map bucket 0. Returns the
/// packet pointer.
pub fn ribbon_quads(m: &mut Machine, map: u32, points: u32, segment: u32, packet: u32) -> u32 {
    let level = |value: u32| {
        let value = (value << 6) as i32;
        let high = ((i64::from(value) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
        let fifteenth = (high.wrapping_add(value) >> 3).wrapping_sub(value >> 31);
        i32::from(0x40i32.wrapping_sub(fifteenth) as i16) as u32
    };
    let ot = m.lw(map + 8);
    let near = level(segment);
    let far = level(segment.wrapping_add(1));
    let near_color = near | (near << 8) | (near << 16);
    let far_color = far | (far << 8) | (far << 16);
    let mut prim = packet;
    let mut next = points + 4;
    for _ in 1..5 {
        m.sw(prim + 4, near_color);
        m.sw(prim + 0xc, near_color);
        m.sw(prim + 0x14, far_color);
        m.sw(prim + 0x1c, far_color);
        let xy = |m: &mut Machine, at: u32| (sext16(m.lh(at + 2)) << 16) | u32::from(m.lhu(at));
        let value = xy(m, points);
        m.sw(prim + 8, value);
        let value = xy(m, next);
        m.sw(prim + 0x10, value);
        let value = xy(m, points + 0x14);
        m.sw(prim + 0x18, value);
        let value = xy(m, next + 0x14);
        next += 4;
        m.sb(prim + 7, 0x3a);
        m.sw(prim + 0x20, value);
        let link = m.lw(ot);
        m.sw(prim, link | 0x0800_0000);
        m.sw(ot, (prim << 8) >> 8);
        prim = prim.wrapping_add(0x24);
    }
    prim
}

/// 800EAA34: 1 when any of the trail's 15 positions lies inside the view
/// box (depth 0x1000..0x8000, |x| <= 0.64 depth, |y| <= 0.96 depth), else 0.
pub fn ribbon_visible(m: &mut Machine, positions: u32) -> u32 {
    const MAGIC: u32 = 0x1062_4dd3;
    let mut visible = 0;
    for entry in 0..0xfu32 {
        let at = positions + entry * 0x10;
        let depth = m.lw(at + 8) as i32;
        let y = m.lw(at + 4) as i32;
        let x = m.lw(at) as i32;
        if !(0x1000..=0x8000).contains(&depth) {
            continue;
        }
        let across = magic_quotient(depth.wrapping_mul(160), MAGIC, 4);
        let along = magic_quotient(depth.wrapping_mul(240), MAGIC, 4);
        if x < across.wrapping_neg() || across < x || y < along.wrapping_neg() || along < y {
            continue;
        }
        visible = 1;
    }
    visible
}

// ---- dispatch -----------------------------------------------------------------------------------

/// Map routines by original address (asked by `ov2_core::dispatch` while
/// module 2 is loaded).
pub(crate) fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x800e_af0c => initialize(m),
        0x800e_7e58 => {
            reset_rings(m, a0);
            0
        }
        0x800e_986c => {
            scatter_stars(m, a0);
            0
        }
        0x800e_a598 => reset_trails(m, a0),
        0x800e_a038 => {
            reset_trail(m, a0, a1);
            0
        }
        0x800e_7ab4 => prepare_ot(m, a0),
        0x800e_7e20 => keep_palette(m),
        0x800e_8008 => start_background_voices(m),
        0x800e_b014 => draw(m, a0),
        0x800e_9748 => draw_layer(m, a0, a1, a2),
        0x800e_80d8 => update_path(m, a0, a1),
        0x800e_8574 => draw_rings(m, a0, a1, a2),
        0x800e_9060 => draw_tunnel(m, a0, a1, a2),
        0x800e_9a54 => draw_stars(m, a0, a1, a2),
        0x800e_ae74 => draw_ribbon_layer(m, a0, a1, a2),
        0x800e_a6cc => ribbon_camera(m, a0, a1),
        0x800e_aafc => draw_ribbons(m, a0, a1, a2),
        0x800e_a53c => ribbon_mode(m, a0, a1, a2),
        0x800e_a7d0 => project_anchors(m, a0, a1, a2),
        0x800e_a900 => ribbon_quads(m, a0, a1, a2, args[3]),
        0x800e_aa34 => ribbon_visible(m, a0),
        _ => return None,
    })
}
