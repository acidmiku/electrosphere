//! JP-only routines of the object-system pass (part 3 of the resident
//! wave): 80085204 (replacing PAL 80083B78) and its cloud layers 8008D9F4
//! (replacing PAL 8008C1C8), translated from the JP instructions (adapted
//! from the PAL translations in `render.rs`) and claimed in [`dispatch`].
//! The other object systems are `render` routines, called by JP address.
use super::res_p3_terrain::{FRAME_BODY_S0, FRAME_BODY_S1, FRAME_BODY_S2, MAIN_S3, spill};
use crate::{
    common::{CONTEXT, GAMEPLAY},
    dispatch,
    machine::{Machine, SmallData},
};
use ac3_gte::{ctrl, reg};

/// The scratchpad work area: +0 ordering table, +4 primitive cursor, +8..
/// eye position, +0x14.. camera angles, +0x1A projection distance.
const SCRATCH: u32 = 0x1f80_0000;
/// An object system of 80085204: its routine, its block offset in the
/// systems block `[gp+0x41C]`, its frame size and its prologue spill slots
/// (offset, value).
type ObjectSystem = (u32, u32, u32, [(u32, u32); 5]);
/// The cloud work area (`SCRATCH + 0x1C`).
const WORK: u32 = SCRATCH + 0x1c;
/// `[gp+0x41C]`: the object systems block (+0x5C8 sky flags, objects
/// +0x600, projectiles +0x1098, debris +0x33C0, lock marker +0x3948,
/// explosions +0x3964; cloud positions from +0).
const OBJECT_SYSTEMS: u32 = 0x41c;
/// `[gp+0x33C]`: sun state (+4 sun height).
const SUN_STATE: u32 = 0x33c;
/// 800B4388: the five ambient particle renderers by mission type.
const AMBIENT_RENDERERS: u32 = 0x800b_4388;

/// GTE command words, exactly as encoded in the original instructions.
const RTPS: u32 = 0x018_0001;
const RTPT: u32 = 0x028_0030;
const DPCS: u32 = 0x078_0010;
const AVSZ4: u32 = 0x168_002e;

fn cop2(m: &mut Machine, code: u32) {
    m.gte
        .command(code)
        .expect("translated GTE command words are valid");
}
fn lwc2(m: &mut Machine, register: usize, address: u32) {
    let value = m.lw(address);
    m.gte.mtc2(register, value);
}
fn swc2(m: &mut Machine, register: usize, address: u32) {
    let value = m.gte.mfc2(register);
    m.sw(address, value);
}
fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
fn slt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
fn abs(value: u32) -> u32 {
    if (value as i32) < 0 {
        0u32.wrapping_sub(value)
    } else {
        value
    }
}
fn link(address: u32) -> u32 {
    (address << 8) >> 8
}

/// 80085204 (JP only; replaces PAL 80083B78; `ot`, `primitives`): draw the
/// object systems: scratch work area (OT, primitive cursor, eye position,
/// camera angles, projection distance), the sun (8008BA8C) and moon
/// (8008B310; the moon is hidden in mission 0x19 with camera mode 2), the
/// cloud layers (8008D9F4), the ambient particle renderer of the mission,
/// objects, projectiles, debris, the lock-on marker and explosions, and
/// the 8008FFAC overlay when the context switch +0x3A5 or the camera flag
/// 0x200 asks for it (PAL: the camera flag only). Returns the new primitive
/// cursor.
pub fn draw_object_systems_85204(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    let frame = m.push_frame(0x20);
    // the prologue spill (the caller's s0..s2, `ra` into 80033324); the
    // callees spill s0 / s1 (the caller's until the sun branch loads the
    // systems block and its +0x5C8), s2 = SCRATCH, s3 and their `ra`
    m.sw(frame + 0x18, FRAME_BODY_S2);
    m.sw(frame + 0x1c, 0x8003_33bc);
    m.sw(frame + 0x14, FRAME_BODY_S1);
    m.sw(frame + 0x10, FRAME_BODY_S0);
    let (mut s0, mut s1, s2, s3) = (FRAME_BODY_S0, FRAME_BODY_S1, SCRATCH, MAIN_S3);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(SCRATCH, ot);
    m.sw(SCRATCH + 4, primitives);
    let camera = m.lw(gameplay + 0x28);
    for (to, from) in [(8, 4), (0xc, 8), (0x10, 0xc)] {
        let value = m.lw(camera + from);
        m.sw(SCRATCH + to, value);
    }
    for (to, from) in [(0x14, 0x14), (0x16, 0x16), (0x18, 0x18), (0x1a, 0x24)] {
        let value = m.lhu(camera + from);
        m.sh(SCRATCH + to, value);
    }
    let mode = m.lw(camera + 0x50);
    let set = mode & 0x870 != 0 || {
        let context = m.gw(CONTEXT);
        m.lw(context) & 0x2000_0000 != 0
    };
    if set {
        let systems = m.gw(OBJECT_SYSTEMS);
        let flags = m.lw(systems + 0x5c8);
        m.sw(systems + 0x5c8, flags | 1);
    }
    let gameplay = m.gw(GAMEPLAY);
    let sky = m.lw(gameplay + 0x244);
    let sun = m.lh(sky + 0x6c) as i32 as u32;
    if sun != 0 {
        let systems = m.gw(OBJECT_SYSTEMS);
        (s0, s1) = (systems, systems + 0x5c8);
        let state = m.gw(SUN_STATE);
        let height = m.lw(state + 4);
        let slots = [
            (0x1c, s3),
            (0x34, 0x8008_5340),
            (0x18, s2),
            (0x14, s1),
            (0x10, s0),
        ];
        spill(m, 0x38, &slots);
        dispatch::call(
            m,
            0x8008_ba8c,
            [systems + 0x5c8, sun, 0u32.wrapping_sub(height), 0],
        );
        let gameplay = m.gw(GAMEPLAY);
        let sky = m.lw(gameplay + 0x244);
        let moon = m.lw(sky + 0x70);
        if moon != 0 {
            let context = m.gw(CONTEXT);
            let hidden = m.lh(context + 0x38e) == 0x19 && {
                let camera = m.lw(gameplay + 0x28);
                m.lw(camera + 0x50) == 2
            };
            if hidden {
                let flags = m.lw(systems + 0x5c8);
                m.sw(systems + 0x5c8, flags | 4);
            } else {
                let slots = [
                    (0x1c, s3),
                    (0x24, 0x8008_53ac),
                    (0x18, s2),
                    (0x14, s1),
                    (0x10, s0),
                ];
                spill(m, 0x28, &slots);
                dispatch::call(m, 0x8008_b310, [systems + 0x5c8, moon, 0, 0]);
            }
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    let sky = m.lw(gameplay + 0x244);
    if m.lbu(sky + 0x6e) != 0 {
        spill(m, 0x18, &[(0xc, s3), (8, s2), (4, s1), (0, s0)]);
        cloud_layers_8d9f4(m);
    }
    let gameplay = m.gw(GAMEPLAY);
    let sky = m.lw(gameplay + 0x244);
    let ambient = u32::from(m.lbu(sky + 0x6f));
    if (ambient as i32) > 0 && slt(ambient, 6) {
        let renderer = m.lw(AMBIENT_RENDERERS + ((ambient - 1) << 2));
        // The renderers read only globals; a1 is the type byte (as PAL).
        dispatch::call(m, renderer, [0, ambient, 0, 0]);
    }
    let objects: [ObjectSystem; 5] = [
        (
            0x8008_54b8,
            0x600,
            0x40,
            [
                (0x24, s3),
                (0x1c, s1),
                (0x3c, 0x8008_5428),
                (0x20, s2),
                (0x18, s0),
            ],
        ),
        (
            0x8008_700c,
            0x1098,
            0x38,
            [
                (0x18, s2),
                (0x30, 0x8008_5434),
                (0x1c, s3),
                (0x14, s1),
                (0x10, s0),
            ],
        ),
        (
            0x8008_b048,
            0x33c0,
            0x28,
            [
                (0x20, 0x8008_5440),
                (0x1c, s3),
                (0x18, s2),
                (0x14, s1),
                (0x10, s0),
            ],
        ),
        (
            0x8009_044c,
            0x3948,
            0x30,
            [
                (0x10, s0),
                (0x2c, 0x8008_544c),
                (0x1c, s3),
                (0x18, s2),
                (0x14, s1),
            ],
        ),
        (
            0x8009_2850,
            0x3964,
            0x28,
            [
                (0x20, 0x8008_5458),
                (0x1c, s3),
                (0x18, s2),
                (0x14, s1),
                (0x10, s0),
            ],
        ),
    ];
    for (routine, offset, size, slots) in objects {
        let systems = m.gw(OBJECT_SYSTEMS);
        spill(m, size, &slots);
        dispatch::call(m, routine, [systems + offset, 0, 0, 0]);
    }
    let context = m.gw(CONTEXT);
    let overlay = m.lbu(context + 0x3a5) != 0 || {
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay + 0x28);
        m.lw(camera + 0x50) & 0x200 != 0
    };
    if overlay {
        spill(m, 0x20, &[(0x14, s1), (0x18, 0x8008_549c), (0x10, s0)]);
        dispatch::call(m, 0x8008_ffac, [SCRATCH, 0, 0, 0]);
    }
    let next = m.lw(SCRATCH + 4);
    m.pop_frame(0x20);
    next
}

/// 8008D9F4 (JP only; replaces PAL 8008C1C8): two cloud layers: each
/// layer's height (relative to the eye, halved per layer), count and drift
/// step come from the mission colour block (gameplay +0x268); clouds are
/// POLY_FT4 sprites around the camera, depth-cued by height, fading toward
/// the near plane (AVSZ4 OTZ 300..556). JP adds a second clut row (0x59
/// instead of 0x39, when the colour word's top byte is set) whose clouds
/// carry a DR_TPAGE word (0xE1000220) after the quad (11-word packets), and
/// clips the centre to |x| < 241, |y| < 201 (PAL |y| < 209).
pub fn cloud_layers_8d9f4(m: &mut Machine) {
    m.push_frame(0x18);
    let mut packet = m.lw(SCRATCH + 4);
    let eye_y = sra(m.lw(SCRATCH + 0xc), 12);
    let gameplay = m.gw(GAMEPLAY);
    let layers = m.lw(gameplay + 0x268) + 8;
    let below = 0u32.wrapping_sub(eye_y);
    for layer in 0..2u32 {
        let entry = layers + (layer << 2);
        let word = m.lw(entry);
        let mut count = (word << 8) >> 24;
        let height = m.lh(entry) as i32 as u32;
        let slot = WORK + (layer << 2);
        m.sw(slot + 0x38, sra(below.wrapping_add(height), layer + 1));
        if !slt(count, 0x81) {
            count = 0x80;
        }
        m.sw(slot + 0x40, 0u32.wrapping_sub(word >> 25));
        m.sw(slot + 0x30, count);
    }
    let colour = m.lw(layers + 0xc);
    m.sw(WORK + 0x2c, link(colour) | 0x2e00_0000);
    let row: u32 = if sra(colour, 24) != 0 { 0x59 } else { 0x39 };
    let clut = row << 16;
    let tpage = row == 0x59;
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0);
    }
    for layer in 0..2u32 {
        let layer_slot = WORK + (layer << 2);
        let eye_x = m.lw(SCRATCH + 8);
        let eye_z = m.lw(SCRATCH + 0x10);
        let mut ox = sra(eye_x, 13);
        let mut oz = sra(eye_z, 13);
        if layer == 0 {
            ox = ox.wrapping_add(sra(eye_x, 15));
            oz = oz.wrapping_add(sra(eye_z, 15));
        }
        let mut height = m.lw(layer_slot + 0x38);
        let mut colour = m.lw(WORK + 0x2c);
        let fade = sra(height, 2);
        if !slt(fade, 0x1000) {
            continue;
        }
        if (fade as i32) > 0 {
            m.gte.mtc2(reg::RGBC, colour);
            m.gte.mtc2(reg::IR0, fade);
            cop2(m, DPCS);
            colour = m.gte.mfc2(reg::RGB2);
        }
        if colour << 8 == 0 {
            continue;
        }
        m.gte.mtc2(reg::RGBC, colour);
        m.sw(WORK + 0x28, colour);
        let mut count = m.lw(layer_slot + 0x30);
        let mut position = m.gw(OBJECT_SYSTEMS);
        if (count as i32) <= 0 {
            continue;
        }
        loop {
            let xz = m.lw(position);
            m.sh(WORK + 2, height as u16);
            let z = sra(xz, 16);
            let x = (xz.wrapping_sub(ox) & 0x7fff).wrapping_sub(0x4000);
            m.sh(WORK, x as u16);
            let z = (z.wrapping_sub(oz) & 0x7fff).wrapping_sub(0x4000);
            m.sh(WORK + 4, z as u16);
            lwc2(m, reg::VXY0, WORK);
            lwc2(m, reg::VZ0, WORK + 4);
            let mut spread = ((count & 7) << 4) + 0x26a;
            cop2(m, RTPS);
            let base_z = m.lw(WORK + 4);
            if count & 1 != 0 {
                spread = 0u32.wrapping_sub(spread);
            }
            let far_z = spread.wrapping_add(base_z);
            let step = ((count & 0xf) << 4) + 0x555;
            m.sw(WORK + 0x1c, far_z);
            m.sw(WORK + 0x14, far_z);
            let base_xy = m.lw(WORK);
            m.sw(WORK + 0xc, base_z);
            let side_xy = base_xy.wrapping_add(step);
            m.sw(WORK + 0x10, base_xy);
            m.sw(WORK + 0x18, side_xy);
            m.sw(WORK + 8, side_xy);
            'cloud: {
                if (m.gte.cfc2(ctrl::FLAG) as i32) < 0 {
                    break 'cloud;
                }
                let centre = m.gte.mfc2(reg::SXY2);
                if !slt(abs(sra(centre << 16, 16)), 0xf1) || !slt(abs(sra(centre, 16)), 0xc9) {
                    break 'cloud;
                }
                lwc2(m, reg::VXY0, WORK + 8);
                lwc2(m, reg::VZ0, WORK + 0xc);
                lwc2(m, reg::VXY1, WORK + 0x10);
                lwc2(m, reg::VZ1, WORK + 0x14);
                lwc2(m, reg::VXY2, WORK + 0x18);
                lwc2(m, reg::VZ2, WORK + 0x1c);
                m.sw(packet + 8, centre);
                let mut page = (layer << 6) + 0x4dd6;
                cop2(m, RTPT);
                if (height as i32) > 0 {
                    page += 0x80;
                }
                let variant = count & 3;
                let u = variant << 5;
                let near_uv = (page << 16) | u | 0x2000;
                let far_uv = clut | (u + 0x1f) | 0x2000;
                m.sw(packet + 0xc, near_uv);
                m.sw(packet + 0x14, far_uv);
                m.sh(packet + 0x1c, (u | 0x2f00) as u16);
                m.sh(packet + 0x24, ((u + 0x1f) | 0x2f00) as u16);
                let u2 = variant << 6;
                m.sw(WORK + 0x64, (page << 16) | u2);
                m.sw(WORK + 0x6c, clut | (u2 + 0x3f));
                m.sh(WORK + 0x74, (u2 | 0x1f00) as u16);
                m.sh(WORK + 0x7c, ((u2 + 0x3f) | 0x1f00) as u16);
                if (m.gte.cfc2(ctrl::FLAG) as i32) < 0 {
                    break 'cloud;
                }
                cop2(m, AVSZ4);
                let length = if tpage {
                    m.sw(packet + 0x28, 0xe100_0220);
                    0x0a00_0000
                } else {
                    0x0900_0000
                };
                let mut otz = m.gte.mfc2(reg::OTZ);
                if !slt(otz, 0x22d) {
                    let colour = m.lw(WORK + 0x28);
                    m.sw(packet + 4, colour);
                    if !slt(otz, 0xfff) {
                        otz = 0xffe;
                    }
                } else {
                    if slt(otz, 0x12c) {
                        break 'cloud;
                    }
                    m.gte.mtc2(reg::IR0, 0x22cu32.wrapping_sub(otz) << 4);
                    cop2(m, DPCS);
                    swc2(m, reg::RGB2, packet + 4);
                }
                let offset = otz << 2;
                let ot = m.lw(SCRATCH);
                let tag = m.lw(offset.wrapping_add(ot)) | length;
                m.sw(packet, tag);
                let ot = m.lw(SCRATCH);
                m.sw(offset.wrapping_add(ot), link(packet));
                if slt(otz, 0x600) {
                    let a = m.lw(WORK + 0x64);
                    let b = m.lw(WORK + 0x6c);
                    m.sw(packet + 0xc, a);
                    m.sw(packet + 0x14, b);
                    let a = m.lw(WORK + 0x74);
                    let b = m.lw(WORK + 0x7c);
                    m.sw(packet + 0x1c, a);
                    m.sw(packet + 0x24, b);
                }
                swc2(m, reg::SXY0, packet + 0x10);
                swc2(m, reg::SXY1, packet + 0x18);
                swc2(m, reg::SXY2, packet + 0x20);
                packet += if tpage { 0x2c } else { 0x28 };
            }
            count = count.wrapping_sub(1);
            let drift = m.lw(layer_slot + 0x40);
            position += 4;
            height = height.wrapping_add(drift);
            if (count as i32) <= 0 {
                break;
            }
        }
    }
    m.sw(SCRATCH + 4, packet);
    m.pop_frame(0x18);
}

/// The routines of this file by JP address (a0..a3 in, v0 out; `None`: not
/// claimed).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, ..] = args;
    Some(match target {
        0x8008_5204 => draw_object_systems_85204(m, a0, a1),
        0x8008_d9f4 => {
            cloud_layers_8d9f4(m);
            0
        }
        _ => return None,
    })
}
