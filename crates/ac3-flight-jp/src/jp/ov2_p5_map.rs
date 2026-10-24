//! JP overlay 2, part 5: the map routines that PAL overlay 2 also has
//! (800FF100..800FF95C; PAL `ov2_core_map` and `ov2_aircraft`): the map
//! ordering table, the background fades and their sound, the map set-up
//! and the ring path the map camera flies along. The map block is
//! `[0x80112DA8]`.
use super::ov2_p5::{MODULE, alloc, call, ctx, framed, hi_s, hub, lh, lhu, lw, sh, sra, sw};
use crate::common::loaded_module;
use crate::machine::Machine;

/// `[0x80112DA8]`: the map block (+0/+4 the two 256-entry ordering
/// tables, +8 the current one, +0xC the frame counter, +0x14 the fade mode
/// (1 reveal, 3 hide), +0x18 the star mode, +0x20/+0x24 their counters,
/// +0x2C/+0x4C the camera matrices, +0x6C.. the ring path, +0x80 ring
/// angles, +0x84 ring positions).
const MAP: u32 = 0x8011_2da8;

fn map(m: &mut Machine) -> u32 {
    m.lw(MAP)
}

/// 800FF100 (JP; PAL 800E7AB4 `prepare_ot`, exact): select the map
/// ordering table of the display buffer (`[map + buffer*4]`, 256 entries,
/// at +8), clear it and chain it under the entry at `a0` (entry 255
/// first). Returns the link to entry 255 (`(ot + 0x3FC) & 0xFFFFFF`).
pub fn prepare_ot(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let entry = a0;
        let context = ctx(m);
        let buffer = lw(m, context, 4);
        let block = map(m);
        let ot = lw(m, block, buffer << 2);
        sw(m, block, 8, ot);
        call(m, 0x800a_8680, [ot, 0x100, 0, 0]);
        let a3 = map(m);
        let a1 = lw(m, a3, 8);
        let v0 = lw(m, entry, 0);
        let v1 = lw(m, a1, 0);
        sw(m, a1, 0, (v1 & 0xff00_0000) | (v0 & 0x00ff_ffff));
        let v1 = lw(m, entry, 0);
        let link = lw(m, a3, 8).wrapping_add(0x3fc) & 0x00ff_ffff;
        sw(m, entry, 0, (v1 & 0xff00_0000) | link);
        link
    })
}

/// The five background voices of the hub block (+0x130..+0x140).
const VOICES: [u32; 5] = [0x130, 0x134, 0x138, 0x13c, 0x140];

/// 800FF19C (JP; PAL 800E7B50 `background_fade_800e7b50`, changed): set
/// the map fade mode `a0` (counter reset); mode 1 plays sound 0x62 and
/// fades the five background voices in (8002DF40(voice, 100, 120)), mode 3
/// plays 0x63 and fades them out (8002DF40(voice, 0, 120)). Returns the
/// last fade's v0 (3 for other modes).
pub fn background_fade(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = map(m);
        sw(m, block, 0x14, a0);
        sw(m, block, 0x20, 0);
        let (sound, level) = match a0 {
            1 => (0x62, 0x64),
            3 => (0x63, 0),
            _ => return 3,
        };
        call(m, 0x8002_db4c, [sound, 0, 0, 0]);
        let mut v0 = 0;
        for offset in VOICES {
            let hub = hub(m);
            let voice = lw(m, hub, offset);
            v0 = call(m, 0x8002_df40, [voice, level, 0x78, 0]);
        }
        v0
    })
}

/// 800FF2BC: play sound 0x62 and start revealing the map (mode 1, counter
/// reset). Returns 1.
pub fn reveal(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, 0x8002_db4c, [0x62, 0, 0, 0]);
        let block = map(m);
        sw(m, block, 0x14, 1);
        sw(m, block, 0x20, 0);
        1
    })
}

/// 800FF2F0 (JP; PAL 800E7D84, changed): start hiding the map (mode 3,
/// counter reset), play sound 0x63 and stop the five background voices
/// (8002DDA8). Returns the last call's v0.
pub fn hide(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = map(m);
        sw(m, block, 0x14, 3);
        sw(m, block, 0x20, 0);
        call(m, 0x8002_db4c, [0x63, 0, 0, 0]);
        let mut v0 = 0;
        for offset in VOICES {
            let hub = hub(m);
            let voice = lw(m, hub, offset);
            v0 = call(m, 0x8002_dda8, [voice, 0, 0, 0]);
        }
        v0
    })
}

/// 800FF390 (JP; PAL 800E7E58, changed): set up the map block `a0`: path
/// speed and counters, the palette rectangle (0x800CD76C; StoreImage into
/// +0xBC of the CLUT 0x347F record at +0xB0), two 0x400-byte ordering
/// tables, and the 64 rings (angles cleared, positions at depth 0x5DA8 -
/// 0xC35 * n / 8). Returns 0.
pub fn map_create(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        let s2 = a0;
        sw(m, s2, 0x78, 0xc8);
        sh(m, s2, 0x70, 0x100);
        sh(m, s2, 0x108, 0x200);
        sh(m, s2, 0x6c, 0);
        sw(m, s2, 0x7c, 0);
        sh(m, s2, 0x72, 0);
        sh(m, s2, 0x10a, 0);
        sw(m, s2, 0xc, 0);
        let (w0, w1) = (m.lw(0x800c_d76c), m.lw(0x800c_d770));
        sw(m, sp, 0x10, w0);
        sw(m, sp, 0x14, w1);
        let v1 = s2.wrapping_add(0xb0);
        sh(m, s2, 0xb0, 0xf);
        sh(m, v1, 2, 0x347f);
        sh(m, v1, 6, 0xc0);
        sh(m, v1, 4, 0);
        sh(m, v1, 8, 0x1f);
        sh(m, v1, 0xa, 0x1f);
        call(m, 0x800a_84a0, [sp + 0x10, s2.wrapping_add(0xbc), 0, 0]);
        for k in 0..2u32 {
            let ot = alloc(m, 0x400);
            sw(m, s2, k << 2, ot);
        }
        let mut a1 = 0u32;
        for s0 in 0..0x40u32 {
            let angles = lw(m, s2, 0x80);
            sw(m, angles, s0 << 3, 0);
            let angles = lw(m, s2, 0x80);
            sw(m, angles.wrapping_add(s0 << 3), 4, 0);
            let positions = lw(m, s2, 0x84);
            sw(m, positions, s0 << 4, 0);
            let positions = lw(m, s2, 0x84);
            sw(m, positions.wrapping_add(s0 << 4), 4, 0);
            let positions = lw(m, s2, 0x84);
            let mut depth = a1 << 3;
            if (depth as i32) < 0 {
                depth = depth.wrapping_add(0x3f);
            }
            let z = 0x5da8u32.wrapping_sub(sra(depth, 6));
            sw(m, positions.wrapping_add(s0 << 4), 8, z);
            a1 = a1.wrapping_add(0xc35);
        }
        0
    })
}

/// Signed quotient by 390 (`0xA80A80A9` multiply-high, `sra 8`).
fn div390(value: u32) -> u32 {
    sra(hi_s(value, 0xa80a_80a9).wrapping_add(value), 8).wrapping_sub(sra(value, 31))
}
/// `swc2 reg, address` (the `mfc2` read with its storage side effect).
fn swc2(m: &mut Machine, reg: usize, address: u32) {
    let value = m.gte.mfc2(reg);
    m.sw(address, value);
}
/// `lwc2 reg, address`.
fn lwc2(m: &mut Machine, reg: usize, address: u32) {
    let value = m.lw(address);
    m.gte.mtc2(reg, value);
}

/// 800FF4EC (JP; PAL 800E80D8 `update_path`, near): advance the camera
/// along the ring path `a0` (work area `a1`). The speed +0x70 is always
/// 0x80 + |frame mod 512 - 256| (PAL: 0x200 while revealing or hiding)
/// and accumulates in +0x72; every 390 units shift the 64-entry ring
/// history by one and generate the new rings with RotMatrix_gte and MVMVA;
/// the reveal (+0x20) and star (+0x24) counters advance by the same steps.
/// The camera interpolates between rings 62 and 63 (GPF, INTPL toward ring
/// 42/43), looks along the path (two ratan2) and stores its matrices and
/// translations. Returns the view translation's z (work +0x1B4).
pub fn update_path(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 0x38, |m, _| {
        let (map, work) = (a0, a1);
        let frame = lw(m, map, 0xc);
        let sum = lhu(m, map, 0x72);
        let offset = (frame & 0x1ff).wrapping_sub(0x100);
        let speed = if (offset as i32) < 0 {
            offset.wrapping_neg()
        } else {
            offset
        }
        .wrapping_add(0x80);
        let sum = sum.wrapping_add(speed);
        let value = sum << 16;
        sh(m, map, 0x72, sum);
        let a2 = sra(sum << 16, 16);
        sh(m, map, 0x70, speed);
        sh(m, map, 0x74, div390(sra(value, 16)));
        let steps = lh(m, map, 0x74);
        let remainder = a2.wrapping_sub(div390(a2).wrapping_mul(390));
        sh(m, map, 0x72, remainder);
        if steps != 0 {
            let phase = lw(m, map, 0x7c).wrapping_add(steps);
            let mode = lw(m, map, 0x14);
            sw(m, map, 0x7c, phase & 7);
            if mode == 1 || mode == 3 {
                let steps = lh(m, map, 0x74);
                let reveal = lw(m, map, 0x20).wrapping_add(steps);
                sw(m, map, 0x20, reveal);
            }
            let stars = lw(m, map, 0x18);
            if stars == 1 || stars == 3 {
                let steps = lh(m, map, 0x74);
                let counter = lw(m, map, 0x24).wrapping_add(steps);
                sw(m, map, 0x24, counter);
            }
            shift_rings(m, map);
            generate_rings(m, map, work);
        }
        camera(m, map, work);
        lw(m, work, 0x1b4)
    })
}

/// 800FF62C..800FF6AC: shift the ring history by `[+0x74]` entries.
fn shift_rings(m: &mut Machine, map: u32) {
    let steps = lh(m, map, 0x74) as i32;
    let angles = lw(m, map, 0x80);
    let positions = lw(m, map, 0x84);
    let base = positions.wrapping_add(0x3e0);
    let origin = [lw(m, base, 0), lw(m, base, 4), lw(m, base, 8)];
    let mut index = 0x40i32;
    if steps >= index {
        return;
    }
    loop {
        // the original walks pointers down; a negative step count (a
        // corrupt +0x72) runs below entry 0, hence the wrapping indices
        index -= 1;
        let to = index as u32;
        let (from_a, to_a) = (to.wrapping_sub(1) << 3, to << 3);
        let first = lw(m, angles, from_a);
        let second = lw(m, angles, from_a.wrapping_add(4));
        sw(m, angles, to_a, first);
        sw(m, angles, to_a.wrapping_add(4), second);
        let (from_p, to_p) = (to.wrapping_sub(1) << 4, to << 4);
        let x = lw(m, positions, from_p);
        let y = lw(m, positions, from_p.wrapping_add(4));
        let z = lw(m, positions, from_p.wrapping_add(8));
        sw(m, positions, to_p, x.wrapping_sub(origin[0]));
        sw(
            m,
            positions,
            to_p.wrapping_add(4),
            y.wrapping_sub(origin[1]),
        );
        sw(
            m,
            positions,
            to_p.wrapping_add(8),
            z.wrapping_sub(origin[2]),
        );
        if steps >= index {
            break;
        }
    }
}

/// 800FF6B0..800FF79C: generate the `[+0x74]` newest rings, newest last:
/// angles (rsin of the running counter +0x6C, x >> 6, y >> 5) and
/// positions (previous ring + rotation of (0, 0, 0x186)).
fn generate_rings(m: &mut Machine, map: u32, work: u32) {
    let mut count = lh(m, map, 0x74);
    let mut angle = lh(m, map, 0x6c);
    let angles = lw(m, map, 0x80);
    let mut ring = angles.wrapping_add(count << 3).wrapping_sub(8);
    let positions = lw(m, map, 0x84);
    let mut position = positions.wrapping_add(count << 4);
    m.gte.mtc2(0, 0);
    m.gte.mtc2(1, 0x186);
    let matrix = work.wrapping_add(0x1bc);
    angle = angle.wrapping_add(1);
    loop {
        let sine = call(m, 0x800a_60f8, [angle << 4, 0, 0, 0]);
        sh(m, ring, 0, sra(sine, 6));
        sh(m, ring, 2, sra(sine, 5));
        sh(m, ring, 4, 0);
        call(m, 0x800a_79a8, [ring.wrapping_add(8), matrix, 0, 0]);
        for (index, offset) in [(0, 0), (1, 4), (2, 8), (3, 0xc), (4, 0x10)] {
            let value = lw(m, matrix, offset);
            m.gte.ctc2(index, value);
        }
        let y = lw(m, position, 4);
        let z = lw(m, position, 8);
        let x = lw(m, position, 0);
        m.gte.ctc2(5, x);
        m.gte.ctc2(6, y);
        m.gte.ctc2(7, z);
        count = count.wrapping_sub(1);
        ring = ring.wrapping_sub(8);
        m.gte.mvmva(true, false, 0, 0, 0);
        position = position.wrapping_sub(0x10);
        swc2(m, 25, position);
        swc2(m, 26, position.wrapping_add(4));
        swc2(m, 27, position.wrapping_add(8));
        if (count as i32) <= 0 {
            break;
        }
        angle = angle.wrapping_add(1);
    }
    sh(m, map, 0x6c, angle);
}

/// 800FF7A0..800FF928: the camera between rings 62 and 63.
fn camera(m: &mut Machine, map: u32, work: u32) {
    let fraction = lh(m, map, 0x72) << 12;
    let positions = lw(m, map, 0x84);
    let base = positions.wrapping_add(0x3e0);
    let x = lw(m, base, 0);
    let z = lw(m, base, 8);
    let next_z = lw(m, base, 0x18);
    let next_x = lw(m, base, 0x10);
    let dz = z.wrapping_sub(next_z);
    let y = lw(m, base, 4);
    let next_y = lw(m, base, 0x14);
    let dx = x.wrapping_sub(next_x);
    let dy = y.wrapping_sub(next_y);
    m.gte.mtc2(8, div390(fraction));
    m.gte.mtc2(9, dx);
    m.gte.mtc2(10, dy);
    m.gte.mtc2(11, dz);
    sh(m, work, 0x198, 0);
    sh(m, map, 0x8c, 0);
    m.gte.gpf(true, false);
    let positions = lw(m, map, 0x84);
    let ahead = positions.wrapping_add(0x2a0);
    let px = m.gte.mfc2(25);
    let py = m.gte.mfc2(26);
    let pz = m.gte.mfc2(27);
    lwc2(m, 9, ahead);
    lwc2(m, 10, ahead.wrapping_add(4));
    lwc2(m, 11, ahead.wrapping_add(8));
    let far = positions.wrapping_add(0x2b0);
    for (index, offset) in [(21, 0), (22, 4), (23, 8)] {
        let value = lw(m, far, offset);
        m.gte.ctc2(index, value);
    }
    sw(m, map, 0x90, px);
    sw(m, map, 0x94, py);
    m.gte.intpl(true, false);
    sw(m, map, 0x98, pz);
    for (offset, scratch, value) in [(0x60, 0x1ac, px), (0x64, 0x1b0, py), (0x68, 0x1b4, pz)] {
        let negated = value.wrapping_neg();
        sw(m, map, offset, negated);
        sw(m, work, scratch, negated);
    }
    let tx = m.gte.mfc2(25);
    let ty = m.gte.mfc2(26);
    let tz = m.gte.mfc2(27);
    let along = tz.wrapping_sub(pz);
    let pitch = call(m, 0x800a_7bb8, [ty.wrapping_sub(py), along, 0, 0]);
    sh(m, work, 0x194, pitch);
    sh(m, map, 0x88, pitch);
    let yaw = call(m, 0x800a_7bb8, [tx.wrapping_sub(px), along, 0, 0]);
    sh(m, work, 0x196, yaw);
    sh(m, map, 0x8a, yaw);
    let (angles, rotation, view) = (
        work.wrapping_add(0x194),
        map.wrapping_add(0x4c),
        map.wrapping_add(0x2c),
    );
    call(m, 0x800a_7488, [angles, rotation, 0, 0]);
    call(m, 0x800a_7448, [rotation, view, 0, 0]);
    let translation = work.wrapping_add(0x1ac);
    call(m, 0x800a_68a8, [view, translation, translation, 0]);
    let x = lw(m, work, 0x1ac);
    let y = lw(m, work, 0x1b0);
    let z = lw(m, work, 0x1b4);
    sw(m, map, 0x40, x);
    sw(m, map, 0xa0, x);
    sw(m, map, 0x44, y);
    sw(m, map, 0xa4, y);
    sw(m, map, 0x48, z);
    sw(m, map, 0xa8, z);
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, _, _] = args;
    Some(match target {
        0x800f_f100 => prepare_ot(m, a0),
        0x800f_f19c => background_fade(m, a0),
        0x800f_f2bc => reveal(m),
        0x800f_f2f0 => hide(m),
        0x800f_f390 => map_create(m, a0),
        0x800f_f4ec => update_path(m, a0, a1),
        _ => return None,
    })
}
