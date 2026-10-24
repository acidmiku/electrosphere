//! JP overlay 2: the map and background block (`[0x80112DA8]`, 0x110
//! bytes; PAL `ov2_core_map`, block `[0x800F9890]`, 0x124 bytes),
//! translated from the JP instructions.
//!
//! [`initialize_map`] (801020BC) allocates the block and its arrays: +0/+4
//! two 0x400-byte buffers (800FF390, another part), +0x80 64 8-byte and
//! +0x84 64 16-byte ring records, +0xDC/+0xE0 48 star records, +0xF0/+0xF4
//! three 15-entry trails, +0xF8 five anchor points, +0x104 16 circle
//! points. +0xC counts frames, +0x14 enables the map layer (8010084C),
//! +0x18 the stars (80100B94), +0x1C the ribbon layer (80101E4C), +0xB0/
//! +0xB2 the map texture's page and CLUT, +0xBC the palette kept after
//! loading. [`draw_map`] (80101FEC) runs the enabled layers with the map
//! projection into the scene packet area.
//!
//! Against PAL, JP draws the rings and the tunnel with the page and CLUT
//! kept in the block and fixed texture rows (v 0xE0 / 0xC0), culls against
//! a 480-line screen, keeps the ring palette at a fixed VRAM position
//! (0x3F0, 0xD1), draws the stars with fixed texture words as half-width
//! squares, scatters the trails over 480 instead of 512 units, and starts
//! the background voices when the map is revealed (8010084C).
use super::ov2_p6::{
    DISPLAY, HUB, MAP, add_prim, call, divide_pow2, framed, link_bucket, load_rot_trans, lwc2,
    magic_quotient, remainder_pow2, sext16, swc2,
};
use crate::{
    arena,
    machine::{Machine, SmallData},
    sdk,
};

/// 800FF390 (another part): reset the rings and the buffers.
const RESET_RINGS: u32 = 0x800f_f390;
/// 800FF4EC (another part): advance the camera along the ring path.
const UPDATE_PATH: u32 = 0x800f_f4ec;
/// 8002DB4C: start a sound, returning its voice.
const START_SOUND: u32 = 0x8002_db4c;

fn rand(m: &mut Machine) -> i32 {
    sdk::rand(m) as i32
}
fn rsin(m: &mut Machine, angle: u32) -> i32 {
    sdk::rsin(m, angle) as i32
}
fn rcos(m: &mut Machine, angle: u32) -> i32 {
    sdk::rcos(m, angle) as i32
}

// ---- 801020BC / 801009AC / 80101570 / 80100FDC: initialization -------------------------

/// 80101EE4 (PAL 800EAF0C, near: the block is 0x110 bytes): allocate and
/// reset the map block (`[0x80112DA8]`): arrays, 16 circle points
/// (rcos/rsin of i*256 halved), counters, then 800FF390, 801009AC and
/// 80101570 (whose v0, 0x200, it returns).
pub fn initialize_map(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let map = arena::alloc(m, 0x110);
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
                at.wrapping_add(2),
                (sine.wrapping_add(((sine as u32) >> 31) as i32) >> 1) as u16,
            );
            let at = m.lw(map + 0x104).wrapping_add(point << 3);
            m.sh(at.wrapping_add(4), 0);
        }
        m.sw(map + 0xc, 0);
        m.sw(map + 0x14, 0);
        m.sw(map + 0x18, 0);
        m.sw(map + 0x1c, 0);
        call(m, RESET_RINGS, [map, 0, 0, 0]);
        scatter_stars(m, map);
        reset_trails(m, map)
    })
}

/// 801009AC (PAL 800E986C, exact): 48 stars: brightness rand mod 64 (+0xDC
/// halfwords), x/y = cos/sin(rand) * (rand mod 1536) / 4096 pushed out by
/// 0x180, depth rand mod 1562 (+0xE0 records of 16 bytes).
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
            m.sw(at.wrapping_add(4), y as u32);
            let value = rand(m);
            let depth =
                value.wrapping_sub(magic_quotient(value, 0x14fa_6d7b, 7).wrapping_mul(0x61a));
            let at = m.lw(map + 0xe0).wrapping_add(record);
            m.sw(at.wrapping_add(8), depth as u32);
            for offset in [0u32, 4] {
                let at = m.lw(map + 0xe0).wrapping_add(record);
                let value = m.lw(at.wrapping_add(offset)) as i32;
                let value = if value >= 0 {
                    value.wrapping_add(0x180)
                } else {
                    value.wrapping_sub(0x180)
                };
                m.sw(at.wrapping_add(offset), value as u32);
            }
        }
    })
}

/// 80101570 (PAL 800EA598, exact): three trails (80100FDC; +0xF0 15 angle
/// records, +0xF4 15 position records each), five anchors (+0xF8: origin,
/// then cos/sin of -0x200 + i*0x400 / 32), +0xFC 0, +0x100 0x200. Returns
/// 0x200.
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
        m.sw(anchors.wrapping_add(4), 0);
        let mut angle = (-0x200i32) as u32;
        for anchor in 1..5u32 {
            let cosine = rcos(m, angle);
            let sine = rsin(m, angle);
            let at = m.lw(map + 0xf8).wrapping_add(anchor << 3);
            let x = divide_pow2(cosine, 5) as u32 & 0xffff;
            let y = (divide_pow2(sine, 5) as u32) << 16;
            m.sw(at, x | y);
            let at = m.lw(map + 0xf8).wrapping_add(anchor << 3);
            m.sw(at.wrapping_add(4), 0);
            angle = angle.wrapping_add(0x400);
        }
        m.sw(map + 0xfc, 0);
        m.sw(map + 0x100, 0x200);
        0x200
    })
}

/// 80100FDC (PAL 800EA038, near: kinds 0/1 scatter over 480 units): trail
/// `index`: radius r = rand & 0x3FFF; one of four starts (rand & 3): x
/// ±0.16 r with y (rand mod 480 - 240) r / 1000 (kinds 0/1), or x (rand
/// mod 320 - 160) r / 1000 with y ±0.24 r (kinds 2/3); random angles
/// (masks 0x1FF/0xDFF/0x5FF and 0xFFF); then the 14 following records copy
/// the first.
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
            m.lw(slot.wrapping_add(table))
        };
        let angle = |m: &mut Machine| {
            let table = m.lw(map + 0xf0);
            m.lw(slot.wrapping_add(table))
        };
        let (first, mask) = if kind < 2 {
            let x = if kind == 0 {
                sext16(across as i16)
            } else {
                (-i32::from(across as i16)) as u32
            };
            let value = rand(m);
            // value / 480: multiply-high by 0x88888889, plus the dividend
            let quotient = {
                let high = ((i64::from(value) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
                (high.wrapping_add(value) >> 8).wrapping_sub(value >> 31)
            };
            let offset = value
                .wrapping_sub(quotient.wrapping_mul(480))
                .wrapping_sub(0xf0)
                .wrapping_mul(radius);
            let at = position(m);
            m.sw(at, x);
            let y = magic_quotient(offset, MAGIC, 4);
            let at = position(m);
            m.sw(at.wrapping_add(4), y as u32);
            let at = position(m);
            m.sw(at.wrapping_add(8), radius as u32);
            (0x1ff, if kind == 0 { 0xdff } else { 0x5ff })
        } else {
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
            m.sw(at.wrapping_add(4), y as u32);
            let at = position(m);
            m.sw(at.wrapping_add(8), radius as u32);
            (if kind == 2 { 0xdff } else { 0x5ff }, 0xfff)
        };
        let value = rand(m) as u32;
        let at = angle(m);
        m.sh(at, (value & first) as u16);
        let value = rand(m) as u32;
        let at = angle(m);
        m.sh(at.wrapping_add(2), (value & mask) as u16);
        let at = angle(m);
        m.sh(at.wrapping_add(4), 0);
        for entry in 1..0xfu32 {
            for offset in [0u32, 2, 4] {
                let at = angle(m).wrapping_add(entry << 3);
                let value = m.lhu(at.wrapping_add(offset).wrapping_sub(8));
                m.sh(at.wrapping_add(offset), value);
            }
            for offset in [0u32, 4, 8] {
                let at = position(m).wrapping_add(entry << 4);
                let value = m.lw(at.wrapping_add(offset).wrapping_sub(0x10));
                m.sw(at.wrapping_add(offset), value);
            }
        }
    })
}

// ---- 8010097C / 80100994 / 80100FAC / 80100FC4: layer modes --------------------------------

/// 8010097C (PAL 800E9824, exact): the stars appear: +0x18 = 1, counter
/// +0x24 = 0. Returns 1 (its v0).
pub fn stars_appear(m: &mut Machine) -> u32 {
    let map = m.lw(MAP);
    m.sw(map.wrapping_add(0x18), 1);
    m.sw(map.wrapping_add(0x24), 0);
    1
}

/// 80100994 (PAL 800E983C, exact): the stars disappear: +0x18 = 3, counter
/// +0x24 = 0. Returns 3.
pub fn stars_disappear(m: &mut Machine) -> u32 {
    let map = m.lw(MAP);
    m.sw(map.wrapping_add(0x18), 3);
    m.sw(map.wrapping_add(0x24), 0);
    3
}

/// 80100FAC (PAL 800EA008, exact): the ribbons appear: +0x1C = 1, counter
/// +0x28 = 0. Returns 1.
pub fn ribbons_appear(m: &mut Machine) -> u32 {
    let map = m.lw(MAP);
    m.sw(map.wrapping_add(0x1c), 1);
    m.sw(map.wrapping_add(0x28), 0);
    1
}

/// 80100FC4 (PAL 800EA020, exact): the ribbons disappear: +0x1C = 3,
/// counter +0x28 = 0. Returns 3.
pub fn ribbons_disappear(m: &mut Machine) -> u32 {
    let map = m.lw(MAP);
    m.sw(map.wrapping_add(0x1c), 3);
    m.sw(map.wrapping_add(0x28), 0);
    3
}

// ---- 80101FEC: drawing ------------------------------------------------------------------------

/// 80101FEC (PAL 800EB014, exact): map projection (OFX 160, OFY 240 in
/// 16.16, H 250, DQA -0x2BB0, DQB 0x1555000); the map layer (8010084C)
/// when +0x14, the stars (80100B94) when +0x18, otherwise the ribbon layer
/// (80101E4C) when +0x1C; counts the frame (+0xC). Returns the packet
/// pointer.
pub fn draw_map(m: &mut Machine, packet: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let map = m.lw(MAP);
        m.gte.ctc2(24, 0x00a0_0000);
        m.gte.ctc2(25, 0x00f0_0000);
        m.gte.ctc2(26, 0xfa);
        m.gte.ctc2(27, (-0x2bb0i32) as u32);
        m.gte.ctc2(28, 0x0155_5000);
        let layer = m.lw(map.wrapping_add(0x14));
        let stars = m.lw(map.wrapping_add(0x18));
        let mut packet = packet;
        if layer != 0 {
            // a3 still holds the DQB value (passed through to 800FF4EC)
            packet = draw_layer(m, map, 0x1f80_0000, packet, 0x0155_5000);
        }
        if stars != 0 {
            packet = draw_stars(m, map, 0x1f80_0000, packet);
        }
        if layer == 0 && stars == 0 && m.lw(map.wrapping_add(0x1c)) != 0 {
            packet = draw_ribbon_layer(m, map, 0x1f80_0000, packet);
        }
        let frame = m.lw(map.wrapping_add(0xc));
        m.sw(map.wrapping_add(0xc), frame.wrapping_add(1));
        packet
    })
}

// ---- 8010084C: map layer ----------------------------------------------------------------------

/// 8010084C (PAL 800E9748, changed: the reveal starts the background
/// voices): the map layer: path (800FF4EC), rings (800FF95C), tunnel
/// (80100304). In mode 1 (reveal) the reveal counter +0x20 ending at 96
/// switches to mode 2 and starts sounds 0x5D..0x61, keeping their voices
/// at `[0x80112DC4]` +0x130..+0x140; in mode 3 (hide) it switches to mode
/// 0. `a3` is passed through to 800FF4EC as the original leaves it.
/// Returns the packet pointer.
pub fn draw_layer(m: &mut Machine, map: u32, work: u32, packet: u32, a3: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        call(m, UPDATE_PATH, [map, work, packet, a3]);
        let packet = draw_rings(m, map, work, packet);
        let packet = draw_tunnel(m, map, work, packet);
        let mode = m.lw(map.wrapping_add(0x14));
        if mode == 1 {
            if divide_pow2(m.lw(map.wrapping_add(0x20)) as i32, 3) >= 0xc {
                m.sw(map.wrapping_add(0x20), 0);
                m.sw(map.wrapping_add(0x14), 2);
                for (sound, offset) in [
                    (0x5d, 0x130),
                    (0x5e, 0x134),
                    (0x5f, 0x138),
                    (0x60, 0x13c),
                    (0x61, 0x140),
                ] {
                    let voice = call(m, START_SOUND, [sound, 0, 0, 0]);
                    let hub = m.lw(HUB);
                    m.sw(hub.wrapping_add(offset), voice);
                }
            }
        } else if mode == 3 && divide_pow2(m.lw(map.wrapping_add(0x20)) as i32, 3) >= 0xc {
            m.sw(map.wrapping_add(0x20), 0);
            m.sw(map.wrapping_add(0x14), 0);
        }
        packet
    })
}

// ---- 800FF95C / 80100304: rings and tunnel ------------------------------------------------------

/// `25000 * value / 64` as compiled (shift-add multiply, truncating divide).
fn ring_rise(value: i32) -> i32 {
    divide_pow2(value.wrapping_mul(25000), 6)
}

/// Ring height of circle point `point` (800FFA70..800FFB6C and its two
/// copies): rings rise from the reveal row while revealing (mode 1, row of
/// the previous ring `previous`) or sink while hiding (mode 3, row of this
/// ring `current`); other rings keep their height.
fn ring_height(m: &mut Machine, map: u32, point: u32, previous: i32, current: i32) {
    let reveal = m.lw(map.wrapping_add(0x20)) as i32;
    let row = divide_pow2(reveal, 3);
    if row == previous {
        if m.lw(map.wrapping_add(0x14)) != 1 {
            let circle = m.lw(map.wrapping_add(0x104));
            m.sh(point.wrapping_add(circle).wrapping_add(4), 0);
            return;
        }
        let circle = m.lw(map.wrapping_add(0x104));
        let step = 7i32.wrapping_sub(reveal.wrapping_sub(previous.wrapping_mul(8)));
        m.sh(
            point.wrapping_add(circle).wrapping_add(4),
            ring_rise(step) as u16,
        );
    } else if row == current {
        if m.lw(map.wrapping_add(0x14)) != 3 {
            let circle = m.lw(map.wrapping_add(0x104));
            m.sh(point.wrapping_add(circle).wrapping_add(4), 0);
            return;
        }
        let circle = m.lw(map.wrapping_add(0x104));
        let step = remainder_pow2(reveal.wrapping_neg(), 3);
        m.sh(
            point.wrapping_add(circle).wrapping_add(4),
            ring_rise(step) as u16,
        );
    }
}

/// The ring projection shared by 800FF95C and 80100304: 16 circle points
/// (`[map+0x104]`, 8 bytes each) in batches of three (RTPT) and one single
/// (RTPS), stored in the work area from +4: screen XY at +4, SZ at +0x84,
/// depth cue (IR0 of a batch, MAC0 of the single) at +0x104 (one word per
/// point; a batch value also covers the two slots before it). `height`
/// runs before each point is loaded (800FF95C raises the points; 80100304
/// leaves them). The two-point RTPT tail is the original's (unreachable
/// with 16 points).
fn project_circle(m: &mut Machine, map: u32, work: u32, mut height: impl FnMut(&mut Machine, u32)) {
    let mut index = 0u32;
    let mut point = 0u32;
    let mut latest = (-4i32) as u32;
    let mut oldest = (-8i32) as u32;
    loop {
        height(m, point);
        let circle = m.lw(map.wrapping_add(0x104));
        lwc2(m, 0, circle.wrapping_add(point));
        lwc2(m, 1, circle.wrapping_add(point).wrapping_add(4));
        latest = latest.wrapping_add(4);
        oldest = oldest.wrapping_add(4);
        index += 1;
        point = point.wrapping_add(8);
        if index >= 0x10 {
            m.gte.rtps(true, false);
            let at = work.wrapping_add(index << 2);
            swc2(m, 14, at);
            swc2(m, 19, at.wrapping_add(0x80));
            swc2(m, 24, at.wrapping_add(0x100));
            return;
        }
        height(m, point);
        let circle = m.lw(map.wrapping_add(0x104));
        lwc2(m, 2, circle.wrapping_add(point));
        lwc2(m, 3, circle.wrapping_add(point).wrapping_add(4));
        latest = latest.wrapping_add(4);
        oldest = oldest.wrapping_add(4);
        index += 1;
        point = point.wrapping_add(8);
        if index >= 0x10 {
            m.gte.rtpt(true, false);
            let at = work.wrapping_add(latest);
            swc2(m, 12, at);
            swc2(m, 13, at.wrapping_add(4));
            let depth = work.wrapping_add(index << 2).wrapping_add(0x7c);
            let cue = m.gte.mfc2(24);
            swc2(m, 17, depth);
            swc2(m, 18, depth.wrapping_add(4));
            m.sw(work.wrapping_add(latest).wrapping_add(0x104), cue);
            m.sw(work.wrapping_add(oldest).wrapping_add(0x104), cue);
            return;
        }
        height(m, point);
        let circle = m.lw(map.wrapping_add(0x104));
        lwc2(m, 4, circle.wrapping_add(point));
        lwc2(m, 5, circle.wrapping_add(point).wrapping_add(4));
        latest = latest.wrapping_add(4);
        oldest = oldest.wrapping_add(4);
        point = point.wrapping_add(8);
        index += 1;
        m.gte.rtpt(true, false);
        let at = work.wrapping_add(oldest);
        swc2(m, 12, at);
        swc2(m, 13, at.wrapping_add(4));
        swc2(m, 14, at.wrapping_add(8));
        let depth = work.wrapping_add(index << 2).wrapping_add(0x78);
        let cue = m.gte.mfc2(8);
        swc2(m, 17, depth);
        swc2(m, 18, depth.wrapping_add(4));
        swc2(m, 19, depth.wrapping_add(8));
        m.sw(work.wrapping_add(latest).wrapping_add(0x104), cue);
        m.sw(work.wrapping_add(oldest).wrapping_add(0x104), cue);
        m.sw(work.wrapping_add((index - 3) << 2).wrapping_add(0x104), cue);
        if index >= 0x10 {
            return;
        }
    }
}

/// Keep this ring's projection (+4, +0x84, +0x104) as the previous one
/// (+0x44, +0xC4, +0x144).
fn keep_previous(m: &mut Machine, work: u32) {
    for point in 0..0x10u32 {
        let at = work.wrapping_add(point << 2);
        let xy = m.lw(at.wrapping_add(4));
        let depth = m.lw(at.wrapping_add(0x84));
        let cue = m.lw(at.wrapping_add(0x104));
        m.sw(at.wrapping_add(0x44), xy);
        m.sw(at.wrapping_add(0xc4), depth);
        m.sw(at.wrapping_add(0x144), cue);
    }
}

/// Whether all four screen points of a quad lie beyond one screen edge
/// (x < 0, y < 0, x > 320, y > 480, tested in that order).
fn quad_offscreen(m: &mut Machine, points: [u32; 4]) -> bool {
    let mut all = |offset: u32, outside: fn(i16) -> bool| {
        points
            .iter()
            .all(|&p| outside(m.lh(p.wrapping_add(offset))))
    };
    all(0, |v| v < 0) || all(2, |v| v < 0) || all(0, |v| v >= 0x141) || all(2, |v| v >= 0x1e1)
}

/// 800FF95C (PAL 800E8574, changed): the seven rings every 8 history
/// entries, projected with each ring's matrix (RotMatrix_gte of its
/// angles, TransMatrix of its position, CompMatrix with the camera +0x2C)
/// and raised or lowered by the reveal; from the second ring on POLY_FT4
/// quads (code 0x2C, color 0x838383; page +0xB0, CLUT +0xB2, u 0..0x1F, v
/// from 0xE0 scrolling with +0x7C) between it and the previous ring,
/// culled off-screen and by NCLIP, at the previous ring's depth; then the
/// ring's palette upload. Returns the packet pointer.
pub fn draw_rings(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x48, |m, sp| {
        let mut packet = packet;
        let page = u32::from(m.lhu(map.wrapping_add(0xb0)));
        m.sw(sp + 0x10, page);
        let clut = u32::from(m.lhu(map.wrapping_add(0xb2)));
        m.sw(sp + 0x14, clut);
        let scroll = (m.lw(map.wrapping_add(0x7c)) << 2) & 0x1f;
        let ot = m.lw(map.wrapping_add(8));
        m.sw(sp + 0x1c, ot);
        m.sw(sp + 0x18, 0xe0u32.wrapping_sub(scroll));
        let (mut current, mut previous) = (0i32, -1i32);
        let mut record = 0u32;
        loop {
            let matrix = work.wrapping_add(0x1bc);
            let global = m.lw(MAP);
            let angles = m.lw(global.wrapping_add(0x80));
            sdk::rot_matrix_gte(m, angles.wrapping_add(record << 3), matrix);
            let positions = m.lw(map.wrapping_add(0x84));
            sdk::trans_matrix(m, matrix, positions.wrapping_add(record << 4));
            let local = work.wrapping_add(0x1dc);
            sdk::comp_matrix(m, map.wrapping_add(0x2c), matrix, local);
            load_rot_trans(m, local);
            project_circle(m, map, work, |m, point| {
                ring_height(m, map, point, previous, current)
            });
            if current != 0 {
                let prim = ring_quads(m, map, work, packet, previous, sp);
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

/// 800FFEB8..8010018C: one ring's quads to the previous ring. Returns the
/// packet pointer after them.
fn ring_quads(m: &mut Machine, map: u32, work: u32, packet: u32, previous: i32, sp: u32) -> u32 {
    for index in [21, 22, 23] {
        m.gte.ctc2(index, 0x800);
    }
    let mut prim = packet;
    let v = m.lw(sp + 0x18);
    let near = v << 8;
    let clut = m.lw(sp + 0x14);
    let first = near | (clut << 16);
    let page = m.lw(sp + 0x10);
    let second = near | (page << 16) | 0x1f;
    let far = v.wrapping_add(0x1f) << 8;
    let fourth = far | 0x1f;
    for point in 0..0x10u32 {
        let mode = m.lw(map.wrapping_add(0x14));
        if mode == 1 && divide_pow2(m.lw(map.wrapping_add(0x20)) as i32, 3) < previous {
            break;
        }
        let mode = m.lw(map.wrapping_add(0x14));
        if mode == 3 && previous < divide_pow2(m.lw(map.wrapping_add(0x20)) as i32, 3) {
            break;
        }
        let next = remainder_pow2(point as i32 + 1, 4) as u32;
        let old = work.wrapping_add(point << 2).wrapping_add(0x44);
        let old_next = work.wrapping_add(next << 2).wrapping_add(0x44);
        let new = work.wrapping_add(point << 2).wrapping_add(4);
        let new_next = work.wrapping_add(next << 2).wrapping_add(4);
        if quad_offscreen(m, [old, old_next, new, new_next]) {
            continue;
        }
        let a = m.lw(old);
        let c = m.lw(new);
        let d = m.lw(new_next);
        m.gte.mtc2(12, a);
        m.gte.mtc2(13, c);
        m.gte.mtc2(14, d);
        m.sw(prim.wrapping_add(8), a);
        m.sw(prim.wrapping_add(0x18), c);
        m.gte.nclip();
        m.sw(prim.wrapping_add(4), 0x2c83_8383);
        let b = m.lw(old_next);
        m.sw(prim.wrapping_add(0x20), d);
        m.sw(prim.wrapping_add(0x10), b);
        if (m.gte.mfc2(24) as i32) < 0 {
            continue;
        }
        m.sw(prim.wrapping_add(0xc), first);
        m.sw(prim.wrapping_add(0x14), second);
        m.sw(prim.wrapping_add(0x1c), far);
        m.sw(prim.wrapping_add(0x24), fourth);
        let depth = m.lw(work.wrapping_add(0xc4).wrapping_add(point << 2)) as i32;
        let ot = m.lw(sp + 0x1c);
        link_bucket(m, ot, depth >> 8, prim, 9);
        prim = prim.wrapping_add(0x28);
    }
    prim
}

/// 80100190..8010027C: the ring palette upload at `prim` (13 words: A0
/// copy of 16x1 to VRAM (0x3F0, 0xD1), the 16 colors of +0xBC faded toward
/// the background color by the first depth cue with INTPL, cache clear)
/// into map bucket (previous depth >> 8) + 5. Returns the packet pointer
/// after it.
fn upload_palette(m: &mut Machine, map: u32, work: u32, prim: u32, sp: u32) -> u32 {
    m.sw(prim, 0x0c00_0000);
    m.sw(prim.wrapping_add(4), 0xa000_0000);
    m.sw(prim.wrapping_add(0x30), 0x0100_0000);
    m.sw(prim.wrapping_add(0xc), 0x0001_0010);
    let display = m.gw(DISPLAY);
    m.sw(prim.wrapping_add(8), 0x00d1_03f0);
    for (index, offset) in [(21, 0x10), (22, 0x11), (23, 0x12)] {
        let channel = u32::from(m.lbu(display.wrapping_add(offset)));
        m.gte.ctc2(index, channel << 4);
    }
    for color in 0..0x10u32 {
        let value = sext16(m.lh(map.wrapping_add(0xbc + color * 2)));
        let cue = m.lw(work.wrapping_add(0x104));
        m.gte.mtc2(28, value);
        m.gte.mtc2(8, cue);
        m.gte.intpl(true, false);
        let packed = m.gte.mfc2(29);
        m.sh(prim.wrapping_add(0x10 + color * 2), packed as u16);
    }
    let mut bucket = ((m.lw(work.wrapping_add(0xc4)) as i32) >> 8).wrapping_add(5);
    if bucket >= 0x100 {
        bucket = 0xff;
    }
    let ot = m.lw(sp + 0x1c);
    let entry = ot.wrapping_add((bucket as u32) << 2);
    add_prim(m, entry, prim);
    prim.wrapping_add(0x34)
}

/// 80100304 (PAL 800E9060, changed): the tunnel: 16 rings from history
/// entry 0x30 on, projected with their matrices; from the second on
/// POLY_FT4 quads to the previous ring (page +0xB0, CLUT +0xB2, v from
/// 0xC0 by the ring row and +0x7C), culled off-screen, at the previous
/// ring's depth. Revealing (mode 1) draws rows up to the reveal counter -
/// 0x30, hiding (mode 3) rows from it. Returns the packet pointer.
pub fn draw_tunnel(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x40, |m, sp| {
        let mut packet = packet;
        let page = u32::from(m.lhu(map.wrapping_add(0xb0)));
        m.sw(sp + 0x10, page);
        let clut = u32::from(m.lhu(map.wrapping_add(0xb2)));
        let scroll = (m.lw(map.wrapping_add(0x7c)) & 7) << 2;
        let ot = m.lw(map.wrapping_add(8));
        m.sw(sp + 0x14, scroll);
        for row in 0..0x10i32 {
            let record = 0x30 + row as u32;
            let matrix = work.wrapping_add(0x1bc);
            let angles = m.lw(map.wrapping_add(0x80));
            sdk::rot_matrix_gte(m, angles.wrapping_add(record << 3), matrix);
            let positions = m.lw(map.wrapping_add(0x84));
            sdk::trans_matrix(m, matrix, positions.wrapping_add(record << 4));
            let local = work.wrapping_add(0x1dc);
            sdk::comp_matrix(m, map.wrapping_add(0x2c), matrix, local);
            load_rot_trans(m, local);
            project_circle(m, map, work, |_, _| {});
            if row != 0 {
                packet = tunnel_quads(m, map, work, packet, row, sp, ot, clut);
            }
            keep_previous(m, work);
        }
        packet
    })
}

/// 80100528..801007C8: one tunnel row's quads.
#[allow(clippy::too_many_arguments)]
fn tunnel_quads(
    m: &mut Machine,
    map: u32,
    work: u32,
    packet: u32,
    row: i32,
    sp: u32,
    ot: u32,
    clut: u32,
) -> u32 {
    for index in [21, 22, 23] {
        m.gte.ctc2(index, 0x800);
    }
    let mut prim = packet;
    let scroll = m.lw(sp + 0x14);
    let band = (((row - 1) as u32 & 7) << 2).wrapping_sub(scroll) & 0x1f;
    let near = band.wrapping_add(0xc0) << 8;
    let far = band.wrapping_add(0xc7) << 8;
    let fourth = far | 0x1f;
    for point in 0..0x10u32 {
        let mode = m.lw(map.wrapping_add(0x14));
        if mode == 1 && (m.lw(map.wrapping_add(0x20)) as i32).wrapping_sub(0x30) < row {
            break;
        }
        if mode == 3 && row < (m.lw(map.wrapping_add(0x20)) as i32).wrapping_sub(0x30) {
            break;
        }
        let next = remainder_pow2(point as i32 + 1, 4) as u32;
        let new = work.wrapping_add(point << 2).wrapping_add(4);
        let new_next = work.wrapping_add(next << 2).wrapping_add(4);
        let old = new.wrapping_add(0x40);
        let old_next = new_next.wrapping_add(0x40);
        if quad_offscreen(m, [old, old_next, new, new_next]) {
            continue;
        }
        let a = m.lw(old);
        let b = m.lw(old_next);
        m.sw(prim.wrapping_add(8), a);
        m.sw(prim.wrapping_add(0x10), b);
        let c = m.lw(new);
        let d = m.lw(new_next);
        m.sw(prim.wrapping_add(4), 0x2c83_8383);
        m.sw(prim.wrapping_add(0xc), near | (clut << 16));
        let page = m.lw(sp + 0x10);
        m.sw(prim.wrapping_add(0x1c), far);
        m.sw(prim.wrapping_add(0x24), fourth);
        m.sw(prim.wrapping_add(0x14), near | (page << 16) | 0x1f);
        m.sw(prim.wrapping_add(0x18), c);
        m.sw(prim.wrapping_add(0x20), d);
        let depth = m.lw(work.wrapping_add(0xc4).wrapping_add(point << 2)) as i32;
        link_bucket(m, ot, depth >> 8, prim, 9);
        prim = prim.wrapping_add(0x28);
    }
    prim
}

// ---- 80100B94: stars ------------------------------------------------------------------------------

/// 80100B94 (PAL 800E9A54, changed: fixed texture words, half-width
/// squares, a 480-line screen): the 48 stars: each star's ring index
/// (+0xDC) advances with the path steps (mod 62); stars shown by the star
/// mode (+0x18: 1 appearing up to the counter +0x24, 3 disappearing from
/// it) are projected with the camera, their ring and their offset
/// (RotMatrix, TransMatrix, CompMatrix twice, RTPS of the origin) and
/// drawn as POLY_FT4 rectangles of width s/2 and height s = 0xFA00 / depth
/// (at least 2) colored by DPCS of 0xB4B4B4 toward the background (code
/// 0x2C), texture u 0x20..0x2F, v 0xE0..0xEF (CLUT 0x343F, page 0xF); the
/// counter ending at 64 switches mode 1/3 to 2/0. Returns the packet
/// pointer.
pub fn draw_stars(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x40, |m, sp| {
        let mut packet = packet;
        let steps = sext16(m.lh(map.wrapping_add(0x74)));
        let ot = m.lw(map.wrapping_add(8));
        m.sw(sp + 0x10, ot);
        if steps != 0 {
            for star in 0..0x30u32 {
                let at = m.lw(map.wrapping_add(0xdc)).wrapping_add(star << 1);
                let mut ring = sext16(m.lh(at)).wrapping_add(steps) as i32;
                if ring >= 0x3e {
                    let high = ((i64::from(ring) * i64::from(0x8421_0843u32 as i32)) >> 32) as i32;
                    let quotient = (high.wrapping_add(ring) >> 5).wrapping_sub(ring >> 31);
                    ring = ring.wrapping_sub(quotient.wrapping_mul(62));
                }
                m.sh(at, ring as u16);
            }
        }
        let display = m.gw(DISPLAY);
        for (index, offset) in [(21, 0x10), (22, 0x11), (23, 0x12)] {
            let channel = u32::from(m.lbu(display.wrapping_add(offset)));
            m.gte.ctc2(index, channel << 4);
        }
        m.gte.mtc2(6, 0x2cb4_b4b4);
        let camera = work.wrapping_add(0x44);
        let local = work.wrapping_add(0x64);
        let star_matrix = work.wrapping_add(4);
        m.sw(sp + 0x14, star_matrix);
        for star in 0..0x30u32 {
            let prim = packet;
            let mode = m.lw(map.wrapping_add(0x18));
            if mode == 1 {
                let rings = m.lw(map.wrapping_add(0xdc));
                let ring = i32::from(m.lh(rings.wrapping_add(star << 1)));
                if (m.lw(map.wrapping_add(0x24)) as i32) < ring {
                    continue;
                }
            }
            if mode == 3 {
                let rings = m.lw(map.wrapping_add(0xdc));
                let counter = m.lw(map.wrapping_add(0x24)) as i32;
                if i32::from(m.lh(rings.wrapping_add(star << 1))) < counter {
                    continue;
                }
            }
            let global = m.lw(MAP);
            sdk::rot_matrix(m, global.wrapping_add(0x88), camera);
            let global = m.lw(MAP);
            let offsets = m.lw(global.wrapping_add(0xe0));
            sdk::trans_matrix(m, camera, offsets.wrapping_add(star << 4));
            let rings = m.lw(map.wrapping_add(0xdc));
            let ring = sext16(m.lh(rings.wrapping_add(star << 1)));
            let angles = m.lw(map.wrapping_add(0x80));
            sdk::rot_matrix(m, angles.wrapping_add(ring << 3), local);
            let positions = m.lw(map.wrapping_add(0x84));
            sdk::trans_matrix(m, local, positions.wrapping_add(ring << 4));
            let star_matrix = m.lw(sp + 0x14);
            sdk::comp_matrix(m, local, camera, star_matrix);
            let view = work.wrapping_add(0x24);
            let star_matrix = m.lw(sp + 0x14);
            sdk::comp_matrix(m, map.wrapping_add(0x2c), star_matrix, view);
            load_rot_trans(m, view);
            m.gte.mtc2(0, 0);
            m.gte.mtc2(1, 0);
            m.gte.rtps(true, false);
            m.sw(prim.wrapping_add(0x14), 0x000f_e02f);
            m.sh(prim.wrapping_add(0x1c), 0xef20);
            m.sw(prim.wrapping_add(0xc), 0x343f_e020);
            m.sh(prim.wrapping_add(0x24), 0xef2f);
            let depth = m.gte.mfc2(19) as i32;
            swc2(m, 14, work.wrapping_add(0xa0));
            if depth <= 0 {
                continue;
            }
            m.gte.dpcs(true, false);
            let mut size = 0xfa00 / depth;
            if size < 2 {
                size = 2;
            }
            let sx = i32::from(m.lh(work.wrapping_add(0xa0)));
            m.sw(work, (depth >> 8) as u32);
            let left = sx.wrapping_sub(size >> 2);
            let sy = i32::from(m.lh(work.wrapping_add(0xa2)));
            let right = left.wrapping_add(size >> 1);
            let top = sy.wrapping_sub(size >> 1);
            if right < 0 || left >= 0x141 {
                continue;
            }
            let bottom = top.wrapping_add(size);
            if bottom < 0 || top >= 0x1e1 {
                continue;
            }
            let upper = (top as u32) << 16;
            m.sw(prim.wrapping_add(0x10), upper | right as u32);
            let lower = (bottom as u32) << 16;
            m.sw(prim.wrapping_add(8), upper | left as u32);
            m.sw(prim.wrapping_add(0x18), lower | left as u32);
            m.sw(prim.wrapping_add(0x20), lower | right as u32);
            swc2(m, 22, prim.wrapping_add(4));
            let bucket = m.lw(work) as i32;
            let ot = m.lw(sp + 0x10);
            link_bucket(m, ot, bucket, prim, 9);
            packet = prim.wrapping_add(0x28);
        }
        let mode = m.lw(map.wrapping_add(0x18));
        let mode = if mode == 1 && (m.lw(map.wrapping_add(0x24)) as i32) >= 0x40 {
            m.sw(map.wrapping_add(0x24), 0);
            m.sw(map.wrapping_add(0x18), 2);
            m.lw(map.wrapping_add(0x18))
        } else {
            mode
        };
        if mode == 3 && (m.lw(map.wrapping_add(0x24)) as i32) >= 0x40 {
            m.sw(map.wrapping_add(0x24), 0);
            m.sw(map.wrapping_add(0x18), 0);
        }
        packet
    })
}

// ---- 80101E4C .. 80101A0C: ribbon layer -------------------------------------------------------

/// 80101E4C (PAL 800EAE74, exact): the ribbon layer: mode +0x1C 1 becomes
/// 2; mode 3 counts +0x28 to 60, then ends (0); then the camera (801016A4)
/// and the ribbons (80101AD4). Returns the packet pointer.
pub fn draw_ribbon_layer(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let mut mode = m.lw(map.wrapping_add(0x1c));
        if mode == 1 {
            m.sw(map.wrapping_add(0x1c), 2);
            mode = m.lw(map.wrapping_add(0x1c));
        }
        if mode == 3 {
            let count = m.lw(map.wrapping_add(0x28));
            if (count as i32) < 0x3c {
                m.sw(map.wrapping_add(0x28), count.wrapping_add(1));
            } else {
                m.sw(map.wrapping_add(0x1c), 0);
            }
        }
        ribbon_camera(m, map, work);
        draw_ribbons(m, map, work, packet)
    })
}

/// 801016A4 (PAL 800EA6CC, exact): a fixed camera for the ribbons: angles
/// and position +0x88..+0x98 cleared, rotation +0x4C (RotMatrix),
/// translation = -position (work +0xD8, TransMatrix), copied to +0x2C and
/// transposed there, the view translation +0xA0 = transpose * -position
/// (ApplyMatrixLV) set on +0x2C (TransMatrix). Returns that last
/// TransMatrix's v0.
pub fn ribbon_camera(m: &mut Machine, map: u32, work: u32) -> u32 {
    framed(m, 0x28, |m, _| {
        for offset in [0x88, 0x8c, 0x90, 0x94, 0x98] {
            m.sw(map.wrapping_add(offset), 0);
        }
        sdk::rot_matrix(m, map.wrapping_add(0x88), map.wrapping_add(0x4c));
        for (from, to) in [(0x90, 0xd8), (0x94, 0xdc), (0x98, 0xe0)] {
            let value = m.lw(map.wrapping_add(from));
            m.sw(work.wrapping_add(to), value.wrapping_neg());
        }
        sdk::trans_matrix(m, map.wrapping_add(0x4c), work.wrapping_add(0xd8));
        for half in 0..2u32 {
            let words: Vec<u32> = (0..4u32)
                .map(|i| m.lw(map.wrapping_add(0x4c + half * 0x10 + i * 4)))
                .collect();
            for (i, word) in words.into_iter().enumerate() {
                m.sw(map.wrapping_add(0x2c + half * 0x10 + i as u32 * 4), word);
            }
        }
        sdk::transpose_matrix(m, map.wrapping_add(0x4c), map.wrapping_add(0x2c));
        sdk::apply_matrix_lv(
            m,
            map.wrapping_add(0x2c),
            work.wrapping_add(0xd8),
            map.wrapping_add(0xa0),
        );
        sdk::trans_matrix(m, map.wrapping_add(0x2c), map.wrapping_add(0xa0))
    })
}

/// 80101AD4 (PAL 800EAAFC, exact): the three ribbons: each trail's history
/// shifts by one (positions +0xF4, angles +0xF0), the head turns by three
/// random steps (rand & 0x1F) and advances by (0, 0, 0x200) rotated by it
/// (MVMVA); a trail whose positions all left the view box (80101A0C)
/// restarts (80100FDC), otherwise its 15 segments are projected (801017A8
/// of the five anchors +0xF8) and drawn as gouraud quads (801018D8) between
/// consecutive segments. A DR_MODE (blend 0) opens the layer in bucket 0
/// and one (blend 1) closes it in bucket 255. Returns the packet pointer.
pub fn draw_ribbons(m: &mut Machine, map: u32, work: u32, packet: u32) -> u32 {
    framed(m, 0x30, |m, _| {
        let ot = m.lw(map.wrapping_add(8));
        let mut packet = ribbon_mode(m, 0, ot, packet);
        let mut slot = 0u32;
        for trail in 0..3u32 {
            let positions = |m: &mut Machine| {
                let table = m.lw(map.wrapping_add(0xf4));
                m.lw(slot.wrapping_add(table))
            };
            let angles = |m: &mut Machine| {
                let table = m.lw(map.wrapping_add(0xf0));
                m.lw(slot.wrapping_add(table))
            };
            let mut entry = 0xeu32;
            loop {
                for offset in [0u32, 4, 8] {
                    let at = positions(m).wrapping_add(entry << 4);
                    let value = m.lw(at.wrapping_add(offset).wrapping_sub(0x10));
                    m.sw(at.wrapping_add(offset), value);
                }
                for offset in [0u32, 2, 4] {
                    let at = angles(m).wrapping_add(entry << 3);
                    let value = m.lhu(at.wrapping_add(offset).wrapping_sub(8));
                    m.sh(at.wrapping_add(offset), value);
                }
                entry -= 1;
                if entry == 0 {
                    break;
                }
            }
            for offset in [0u32, 2, 4] {
                let step = rand(m) as u32;
                let head = angles(m);
                let value = u32::from(m.lhu(head.wrapping_add(offset))).wrapping_add(step & 0x1f);
                m.sh(head.wrapping_add(offset), value as u16);
            }
            let head = angles(m);
            sdk::rot_matrix(m, head, work);
            let next = positions(m);
            sdk::trans_matrix(m, work, next.wrapping_add(0x10));
            load_rot_trans(m, work);
            lwc2(m, 0, map.wrapping_add(0xfc));
            lwc2(m, 1, map.wrapping_add(0x100));
            m.gte.mvmva(true, false, 0, 0, 0);
            let head = positions(m);
            swc2(m, 25, head);
            swc2(m, 26, head.wrapping_add(4));
            swc2(m, 27, head.wrapping_add(8));
            let head = positions(m);
            if ribbon_visible(m, head) == 0 {
                reset_trail(m, trail, map);
                slot += 4;
                continue;
            }
            for segment in 0..0xfu32 {
                let angle = angles(m).wrapping_add(segment << 3);
                sdk::rot_matrix(m, angle, work);
                let position = positions(m).wrapping_add(segment << 4);
                sdk::trans_matrix(m, work, position);
                sdk::comp_matrix(m, map.wrapping_add(0x2c), work, work.wrapping_add(0x40));
                let anchors = m.lw(map.wrapping_add(0xf8));
                project_anchors(m, anchors, work, 5);
                if segment != 0 {
                    packet = ribbon_quads(m, map, work.wrapping_add(0x60), segment - 1, packet);
                }
                for word in 0..5u32 {
                    let value = m.lw(work.wrapping_add(0x60 + word * 4));
                    m.sw(work.wrapping_add(0x74 + word * 4), value);
                }
            }
            slot += 4;
        }
        let ot = m.lw(map.wrapping_add(8));
        ribbon_mode(m, 1, ot.wrapping_add(0x3fc), packet)
    })
}

/// 80101514 (PAL 800EA53C, exact): a DR_MODE with blend mode `mode & 3`
/// (SetDrawMode(p, 0, 0, mode << 5, null)) added at `entry`; returns the
/// packet after it.
pub fn ribbon_mode(m: &mut Machine, mode: u32, entry: u32, prim: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        m.sw(sp + 0x10, 0);
        sdk::set_draw_mode(m, prim, 0, 0, (mode & 3) << 5, 0);
        add_prim(m, entry, prim);
        prim.wrapping_add(0xc)
    })
}

/// 801017A8 (PAL 800EA7D0, exact): project `count` SVECTOR points with the
/// matrix at `work + 0x40` (RTPT in threes; a remainder of one or two
/// still runs RTPT) into the screen XY words at `work + 0x60`. Returns the
/// last output address for a remainder of one or two, else 0 (the loop
/// test).
pub fn project_anchors(m: &mut Machine, points: u32, work: u32, count: u32) -> u32 {
    let out = work.wrapping_add(0x60);
    load_rot_trans(m, work.wrapping_add(0x40));
    let mut at = points;
    let (mut done, mut latest, mut older) = (0i32, (-4i32) as u32, (-8i32) as u32);
    loop {
        lwc2(m, 0, at);
        lwc2(m, 1, at.wrapping_add(4));
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
        lwc2(m, 3, at.wrapping_add(4));
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
        lwc2(m, 5, at.wrapping_add(4));
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

/// 801018D8 (PAL 800EA900, exact): four semi-transparent gouraud quads
/// (POLY_G4, code 0x3A) of segment `segment` fanning from the first anchor
/// of this (`points`) and the previous segment (`points + 0x14`), gray
/// levels 64 - segment * 64 / 15 and 64 - (segment + 1) * 64 / 15, into map
/// bucket 0. Returns the packet pointer.
pub fn ribbon_quads(m: &mut Machine, map: u32, points: u32, segment: u32, packet: u32) -> u32 {
    let level = |value: u32| {
        let value = (value << 6) as i32;
        let high = ((i64::from(value) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
        let fifteenth = (high.wrapping_add(value) >> 3).wrapping_sub(value >> 31);
        i32::from(0x40i32.wrapping_sub(fifteenth) as i16) as u32
    };
    let ot = m.lw(map.wrapping_add(8));
    let near = level(segment);
    let far = level(segment.wrapping_add(1));
    let near_color = near | (near << 8) | (near << 16);
    let far_color = far | (far << 8) | (far << 16);
    let mut prim = packet;
    let mut next = points.wrapping_add(4);
    for _ in 1..5 {
        m.sw(prim.wrapping_add(4), near_color);
        m.sw(prim.wrapping_add(0xc), near_color);
        m.sw(prim.wrapping_add(0x14), far_color);
        m.sw(prim.wrapping_add(0x1c), far_color);
        let xy = |m: &mut Machine, at: u32| {
            (sext16(m.lh(at.wrapping_add(2))) << 16) | u32::from(m.lhu(at))
        };
        let value = xy(m, points);
        m.sw(prim.wrapping_add(8), value);
        let value = xy(m, next);
        m.sw(prim.wrapping_add(0x10), value);
        let value = xy(m, points.wrapping_add(0x14));
        m.sw(prim.wrapping_add(0x18), value);
        let value = xy(m, next.wrapping_add(0x14));
        next = next.wrapping_add(4);
        m.sb(prim.wrapping_add(7), 0x3a);
        m.sw(prim.wrapping_add(0x20), value);
        let link = m.lw(ot);
        m.sw(prim, link | 0x0800_0000);
        m.sw(ot, (prim << 8) >> 8);
        prim = prim.wrapping_add(0x24);
    }
    prim
}

/// 80101A0C (PAL 800EAA34, exact): 1 when any of the trail's 15 positions
/// lies inside the view box (depth 0x1000..0x8000, |x| <= 0.16 depth, |y|
/// <= 0.24 depth), else 0.
pub fn ribbon_visible(m: &mut Machine, positions: u32) -> u32 {
    const MAGIC: u32 = 0x1062_4dd3;
    let mut visible = 0;
    for entry in 0..0xfu32 {
        let at = positions.wrapping_add(entry * 0x10);
        let depth = m.lw(at.wrapping_add(8)) as i32;
        let y = m.lw(at.wrapping_add(4)) as i32;
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
