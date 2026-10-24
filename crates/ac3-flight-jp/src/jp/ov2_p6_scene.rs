//! JP overlay 2: the 3D story scene (`[0x80112DAC]`, 0xA7C bytes), its
//! camera and its script commands, translated from the JP instructions.
//!
//! The scene block: +0/+2/+4 camera angles, +8..+0x10 the camera position
//! (from the orbit), +0x18..+0x1C the orbit angles, +0x20..+0x28 the
//! looked-at point, +0x30 the camera matrix and +0x50 its transposed,
//! halved view matrix, +0x70 the orbit distance, +0x74/+0x78 the pan speed
//! and frames, +0x7E the pan heading, +0x84..+0x8C the gauge, +0x90/+0x92,
//! +0x894..+0x8D0 16 colors, +0x9BC..+0x9C4 the marker, +0x9C8 the current
//! script command, +0x9CC/+0x9CE the labels' mask and count, +0x9D0 the
//! placed objects' mask, +0x9D4 the camera override, +0x9DC 32 object
//! pointers, +0xA5C the label pointers.
//!
//! An object (`[+0x9DC + i*4]`): +0 its number, +2 its state (1 appearing,
//! 2 moving), +4 frames left, +6 frames in all, +8 speed, +0xC..+0x10 its
//! position, +0x14 its angles (+0x16 the heading), +0x1C/+0x1E its screen
//! position. Its POLY_FT4s are prebuilt in the hub's two primitive tables
//! (`[0x80112DC4]` +0x38/+0x3C, the current one +0x40) at +0xB8C + i*0x28.
//!
//! A script command (16 bytes): +2 opcode, +3 slot, +4.. halfword
//! arguments (positions as 0x80-centred bytes scaled by 128).
use super::ov2_p6::{HUB, SCENE, call, div_trap, divide_pow2, framed, half, lwc2, swc2};
use crate::{
    arena,
    machine::{Machine, SmallData},
    sdk,
};

/// 800DEE08 (another part): set visual slot `a0`'s parameters.
const SET_VISUAL: u32 = 0x800d_ee08;
/// 800DEF94 (another part): set visual slot `a0`'s style.
const SET_VISUAL_STYLE: u32 = 0x800d_ef94;
/// 8002DB4C: start a sound.
const START_SOUND: u32 = 0x8002_db4c;

fn scene(m: &mut Machine) -> u32 {
    m.lw(SCENE)
}
fn command(m: &mut Machine) -> u32 {
    let scene = scene(m);
    m.lw(scene.wrapping_add(0x9c8))
}
/// The object pointer of slot `slot`.
fn object(m: &mut Machine, slot: u32) -> u32 {
    let scene = scene(m);
    m.lw(scene.wrapping_add(slot << 2).wrapping_add(0x9dc))
}
fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
fn sext16(value: u32) -> u32 {
    value as u16 as i16 as i32 as u32
}
/// A 0x80-centred argument scaled by 128.
fn position(value: u32) -> u32 {
    value.wrapping_sub(0x80) << 7
}
fn abs(value: u32) -> u32 {
    if (value as i32) < 0 {
        value.wrapping_neg()
    } else {
        value
    }
}

// ---- 801020C0 .. 80102564: the block and the camera ---------------------------------------

/// 801020C0: allocate the scene block (0xA7C bytes) with no marker (+0x9C4
/// = -1) into `[0x80112DAC]`. Returns the block.
pub fn allocate_scene(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = arena::alloc(m, 0xa7c);
        m.sw(block.wrapping_add(0x9c4), u32::MAX);
        m.sw(SCENE, block);
        block
    })
}

/// 801020F4: reset the pan: +0x90/+0x92 0x80, speed, frames and angles
/// +0x74..+0x80 0. Returns the block.
pub fn reset_pan(m: &mut Machine) -> u32 {
    let scene = scene(m);
    m.sh(scene.wrapping_add(0x90), 0x80);
    m.sh(scene.wrapping_add(0x92), 0x80);
    m.sw(scene.wrapping_add(0x74), 0);
    m.sw(scene.wrapping_add(0x78), 0);
    m.sh(scene.wrapping_add(0x7c), 0);
    m.sh(scene.wrapping_add(0x7e), 0);
    m.sh(scene.wrapping_add(0x80), 0);
    scene
}

/// 80102120: halve the first and last rows of the MATRIX at `matrix`
/// (m00, m01, m02, m20, m21, m22, arithmetic shifts). Returns the last
/// value.
pub fn halve_rows(m: &mut Machine, matrix: u32) -> u32 {
    let halved = |value: u16| ((u32::from(value) << 16) as i32 >> 17) as u32;
    let a = halved(m.lhu(matrix));
    let b = halved(m.lhu(matrix.wrapping_add(2)));
    let f = halved(m.lhu(matrix.wrapping_add(0xe)));
    m.sh(matrix, a as u16);
    let c = halved(m.lhu(matrix.wrapping_add(4)));
    m.sh(matrix.wrapping_add(2), b as u16);
    let d = halved(m.lhu(matrix.wrapping_add(0xc)));
    m.sh(matrix.wrapping_add(0xe), f as u16);
    m.sh(matrix.wrapping_add(4), c as u16);
    let e = halved(m.lhu(matrix.wrapping_add(0x10)));
    m.sh(matrix.wrapping_add(0xc), d as u16);
    m.sh(matrix.wrapping_add(0x10), e as u16);
    e
}

/// 80102184: the scene camera. The geometry offset follows the hub's
/// current object (`[[0x80112DC4]+0x16C]` +0x40/+0x42, plus 0x9A). Unless
/// overridden (+0x9D4), hub frames 0x52..0x71 (`[0x80112DC4]+0x50`) swing
/// the orbit in (angles 0x400 - 16 t, 0x570 + 8 f, 0; distance 0x8000 -
/// 877 t / 1) with the offset moving by the looked-at point; otherwise
/// the orbit is (0x200, 0x900, 0) at 0x125C. The camera sits on the orbit
/// (RotMatrixYXZ, MVMVA of (0, 0, distance) about +0x20) at +8, looks at
/// +0x20 (ratan2 of the deltas into +0..+4, RotMatrixYXZ +0x30, translation
/// -camera), and +0x50 = transpose(+0x30) with rows 0 and 2 halved and the
/// view translation (ApplyMatrixLV). Returns the last TransMatrix's v0.
pub fn update_camera(m: &mut Machine) -> u32 {
    framed(m, 0x78, |m, sp| {
        let hub = m.lw(HUB);
        let current = m.lw(hub.wrapping_add(0x16c));
        let x = lh(m, current.wrapping_add(0x40)).wrapping_add(0x9a);
        let y = lh(m, current.wrapping_add(0x42)).wrapping_add(0x9a);
        sdk::set_geom_offset(m, x, y);
        let block = scene(m);
        let (x, y) = 'offset: {
            if m.lbu(block.wrapping_add(0x9d4)) == 0 {
                let hub = m.lw(HUB);
                let frame = m.lw(hub.wrapping_add(0x50)) as i32;
                if (0x52..0x72).contains(&frame) {
                    let t = (frame - 0x52) as u32;
                    m.sh(
                        block.wrapping_add(0x18),
                        0x400u32.wrapping_sub(t << 4) as u16,
                    );
                    let frame = m.lw(hub.wrapping_add(0x50));
                    m.sh(block.wrapping_add(0x1c), 0);
                    m.sh(
                        block.wrapping_add(0x1a),
                        (frame << 3).wrapping_add(0x570) as u16,
                    );
                    let t = m.lw(hub.wrapping_add(0x50)).wrapping_sub(0x52);
                    let swing = divide_pow2(t.wrapping_mul(28068) as i32, 5) as u32;
                    let look_x = m.lw(block.wrapping_add(0x20)) as i32;
                    m.sw(block.wrapping_add(0x70), 0x8000u32.wrapping_sub(swing));
                    let look_x = divide_pow2(look_x, 8) as u32;
                    let left = 0x72u32.wrapping_sub(m.lw(hub.wrapping_add(0x50)));
                    let dx = divide_pow2(look_x.wrapping_mul(left) as i32, 5) as u32;
                    let current = m.lw(hub.wrapping_add(0x16c));
                    let look_z = m.lw(block.wrapping_add(0x28)) as i32;
                    let ox = lh(m, current.wrapping_add(0x40)).wrapping_add(0x9a);
                    let oy = lh(m, current.wrapping_add(0x42)).wrapping_add(0x9a);
                    let x = dx.wrapping_add(ox);
                    let look_z = divide_pow2(look_z, 8) as u32;
                    let dz = divide_pow2(look_z.wrapping_mul(left) as i32, 5) as u32;
                    break 'offset (x, oy.wrapping_sub(dz << 1));
                }
            }
            let block = scene(m);
            m.sh(block.wrapping_add(0x18), 0x200);
            m.sh(block.wrapping_add(0x1a), 0x900);
            let hub = m.lw(HUB);
            m.sh(block.wrapping_add(0x1c), 0);
            m.sw(block.wrapping_add(0x70), 0x125c);
            let current = m.lw(hub.wrapping_add(0x16c));
            let x = lh(m, current.wrapping_add(0x40)).wrapping_add(0x9a);
            let y = lh(m, current.wrapping_add(0x42)).wrapping_add(0x9a);
            (x, y)
        };
        sdk::set_geom_offset(m, x, y);
        let matrix = sp + 0x38;
        let block = scene(m);
        let vector = sp + 0x30;
        m.sh(vector, 0);
        m.sh(vector + 2, 0);
        let distance = m.lhu(block.wrapping_add(0x70));
        m.sh(vector + 4, distance);
        sdk::rot_matrix_yxz(m, block.wrapping_add(0x18), matrix);
        let block = scene(m);
        sdk::trans_matrix(m, matrix, block.wrapping_add(0x20));
        super::ov2_p6::load_rot_trans(m, matrix);
        lwc2(m, 0, vector);
        lwc2(m, 1, vector + 4);
        m.gte.mvmva(true, false, 0, 0, 0);
        let block = scene(m);
        let eye = block.wrapping_add(8);
        swc2(m, 25, eye);
        swc2(m, 26, eye.wrapping_add(4));
        swc2(m, 27, eye.wrapping_add(8));
        let flag = m.gte.cfc2(31);
        m.sw(sp + 0x58, flag);
        let block = scene(m);
        let dx = m
            .lw(block.wrapping_add(0x20))
            .wrapping_sub(m.lw(block.wrapping_add(8)));
        let dz = m
            .lw(block.wrapping_add(0x28))
            .wrapping_sub(m.lw(block.wrapping_add(0x10)));
        let dy = m
            .lw(block.wrapping_add(0x24))
            .wrapping_sub(m.lw(block.wrapping_add(0xc)));
        let squares = dx.wrapping_mul(dx).wrapping_add(dz.wrapping_mul(dz));
        let distance = sdk::square_root0(m, squares);
        let pitch = sdk::ratan2(m, dy, distance);
        let block = scene(m);
        m.sh(block, pitch.wrapping_neg() as u16);
        let yaw = sdk::ratan2(m, dx, dz);
        let block = scene(m);
        m.sh(block.wrapping_add(2), yaw as u16);
        m.sh(block.wrapping_add(4), 0);
        sdk::rot_matrix_yxz(m, block, block.wrapping_add(0x30));
        let block = scene(m);
        let negated = sp + 0x10;
        for k in 0..3u32 {
            let value = m.lw(block.wrapping_add(8 + k * 4));
            m.sw(negated + k * 4, value.wrapping_neg());
        }
        sdk::trans_matrix(m, block.wrapping_add(0x30), negated);
        let block = scene(m);
        for half in 0..2u32 {
            let words: Vec<u32> = (0..4u32)
                .map(|i| m.lw(block.wrapping_add(0x30 + half * 0x10 + i * 4)))
                .collect();
            for (i, word) in words.into_iter().enumerate() {
                m.sw(block.wrapping_add(0x50 + half * 0x10 + i as u32 * 4), word);
            }
        }
        sdk::transpose_matrix(m, block.wrapping_add(0x30), block.wrapping_add(0x50));
        let block = scene(m);
        halve_rows(m, block.wrapping_add(0x50));
        let block = scene(m);
        let view = sp + 0x20;
        sdk::apply_matrix_lv(m, block.wrapping_add(0x50), negated, view);
        let block = scene(m);
        sdk::trans_matrix(m, block.wrapping_add(0x50), view)
    })
}

/// 80102564: the 16 scene colors (+0x894..+0x8D0). Returns the block.
pub fn set_colors(m: &mut Machine) -> u32 {
    const COLORS: [u32; 16] = [
        0x0080_4040,
        0x0060_6040,
        0x0060_8040,
        0x0040_8040,
        0x0040_8060,
        0x0040_8080,
        0x0040_6080,
        0x0040_4080,
        0x0040_4060,
        0x0084_9494,
        0x0090_a0a0,
        0x00ac_b4b4,
        0x00b4_c4c4,
        0x00c8_d2d2,
        0x00d2_dcdc,
        0x00e6_f0f0,
    ];
    let block = scene(m);
    for (i, color) in COLORS.into_iter().enumerate() {
        m.sw(block.wrapping_add(0x894 + i as u32 * 4), color);
    }
    block
}

// ---- 80102658 .. 80102F9C: script commands ------------------------------------------------

/// 80102658 (command): place object `slot` (+0x9D0 |= 1 << slot) with
/// number arg0, kind `a0 + 15` (+4 and +6), opcode at +2, position (arg1,
/// -arg2 / 10, arg3), heading arg4 * 4096 / 360; its POLY_FT4s in both hub
/// tables get CLUT 0x303F and the digit texture of its number (u = (n mod
/// 10) * 16 + 0x20, v = (n / 10) * 16 - 0x40, 16x15 texels... as the
/// original computes them). Returns 0.
pub fn command_place(m: &mut Machine, kind: u32) -> u32 {
    let block = scene(m);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let slot = u32::from(m.lbu(cmd.wrapping_add(3)));
    let object = m.lw(block.wrapping_add(slot << 2).wrapping_add(0x9dc));
    let mask = m.lw(block.wrapping_add(0x9d0));
    let cmd = m.lw(block.wrapping_add(0x9c8));
    m.sw(block.wrapping_add(0x9d0), mask | (1u32 << (slot & 31)));
    let opcode = m.lbu(cmd.wrapping_add(2));
    let kind = kind.wrapping_add(0xf);
    m.sh(object.wrapping_add(4), kind as u16);
    m.sh(object.wrapping_add(6), kind as u16);
    m.sh(object.wrapping_add(2), u16::from(opcode));
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let number = m.lhu(cmd.wrapping_add(4));
    m.sh(object, number);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let x = lh(m, cmd.wrapping_add(6));
    m.sh(object.wrapping_add(0xc), position(x) as u16);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let y = lh(m, cmd.wrapping_add(8)).wrapping_neg();
    let tenth = super::ov2_p6::magic_quotient(y as i32, 0x6666_6667, 2);
    m.sh(object.wrapping_add(0xe), tenth as u16);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let z = lh(m, cmd.wrapping_add(0xa));
    m.sh(object.wrapping_add(0x14), 0);
    m.sh(object.wrapping_add(0x10), position(z) as u16);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let heading = lh(m, cmd.wrapping_add(0xc)) << 12;
    m.sh(object.wrapping_add(0x18), 0);
    let high = ((i64::from(heading as i32) * i64::from(0xb60b_60b7u32 as i32)) >> 32) as i32;
    let heading = (high.wrapping_add(heading as i32) >> 8).wrapping_sub(heading as i32 >> 31);
    m.sh(object.wrapping_add(0x16), heading as u16);
    // the number's digits: n mod 10 and n / 10, each times 16 (16-bit)
    let digits = |m: &mut Machine| {
        let n = i32::from(m.lhu(object) as i16);
        let tens = super::ov2_p6::magic_quotient(n, 0x6666_6667, 2);
        (n.wrapping_sub(tens.wrapping_mul(10)), tens)
    };
    let scaled = |value: i32| ((value as u32) << 16) as i32 >> 12;
    for table in 0..2u32 {
        let hub = m.lw(HUB);
        let block = scene(m);
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let base = m.lw(hub.wrapping_add(table << 2).wrapping_add(0x38));
        let prim = |m: &mut Machine| {
            let block = scene(m);
            let cmd = m.lw(block.wrapping_add(0x9c8));
            let slot = u32::from(m.lbu(cmd.wrapping_add(3)));
            base.wrapping_add(slot.wrapping_mul(0x28))
        };
        let slot = u32::from(m.lbu(cmd.wrapping_add(3)));
        let first = base.wrapping_add(slot.wrapping_mul(0x28));
        m.sh(first.wrapping_add(0xb9a), 0x303f);
        for (offset, units, add) in [
            (0xb98u32, true, 0x20i32),
            (0xb99, false, -0x40),
            (0xba0, true, 0x2f),
            (0xba1, false, -0x40),
            (0xba8, true, 0x20),
            (0xba9, false, -0x31),
            (0xbb0, true, 0x2f),
            (0xbb1, false, -0x31),
        ] {
            let (unit, ten) = digits(m);
            let at = prim(m);
            let value = scaled(if units { unit } else { ten }).wrapping_add(add);
            m.sb(at.wrapping_add(offset), value as u8);
        }
    }
    0
}

/// 80102AA8 (PAL 800EB1D8 opcode 2, changed): start moving object `slot`
/// towards arg1/arg3 over arg0 frames: heading `ratan2` of the 16-bit
/// deltas (+0x16), speed = their distance / arg0 (+8).
pub fn command_move(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let block = scene(m);
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let slot = u32::from(m.lbu(cmd.wrapping_add(3)));
        let object = m.lw(block.wrapping_add(slot << 2).wrapping_add(0x9dc));
        let opcode = m.lbu(cmd.wrapping_add(2));
        m.sh(object.wrapping_add(2), u16::from(opcode));
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let frames = m.lhu(cmd.wrapping_add(4));
        m.sh(object.wrapping_add(4), frames);
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let x_raw = lhu(m, object.wrapping_add(0xc));
        let frames = m.lhu(cmd.wrapping_add(4));
        m.sh(object.wrapping_add(6), frames);
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let x_arg = lh(m, cmd.wrapping_add(6)).wrapping_sub(0x80);
        let z_arg = lh(m, cmd.wrapping_add(0xa)).wrapping_sub(0x80);
        let dx = sext16(x_arg << 7).wrapping_sub(sext16(x_raw));
        let dx_wide = (x_arg << 7).wrapping_sub(x_raw);
        let z_raw = lhu(m, object.wrapping_add(0x10));
        let dz_wide = (z_arg << 7).wrapping_sub(z_raw);
        let dz = sext16(z_arg << 7).wrapping_sub(sext16(z_raw));
        let heading = sdk::ratan2(m, dx, dz);
        let a = sext16(dx_wide);
        let b = sext16(dz_wide);
        m.sh(object.wrapping_add(0x16), heading as u16);
        let squares = a.wrapping_mul(a).wrapping_add(b.wrapping_mul(b));
        let distance = sdk::square_root0(m, squares);
        let cmd = command(m);
        let frames = lh(m, cmd.wrapping_add(4));
        let speed = div_trap(m, distance, frames, 0x8010_2bac);
        m.sh(object.wrapping_add(8), speed as u16);
        speed
    })
}

/// 80102BFC (command): add a label (+0x9CC |= 1 << count; the label record
/// `[+0xA5C + count*4]` gets style arg0 and slot); visual slots 2, 3, 4
/// show the newest one, two and three labels (800DEE08(n, 1, 0, 30) and
/// 800DEF94(n + 3, style)); then sound 100. Returns the sound's v0.
pub fn command_label(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = scene(m);
        let count = u32::from(m.lbu(block.wrapping_add(0x9ce)));
        let index = u32::from(m.lbu(block.wrapping_add(0x9ce)));
        let bit = 1u32 << (count & 31);
        let mask = lhu(m, block.wrapping_add(0x9cc));
        m.sh(block.wrapping_add(0x9cc), (mask | bit) as u16);
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let label = m.lw(block.wrapping_add(index << 2).wrapping_add(0xa5c));
        let style = m.lbu(cmd.wrapping_add(4));
        m.sb(label, style);
        let cmd = command(m);
        let slot = m.lbu(cmd.wrapping_add(3));
        m.sb(label.wrapping_add(1), slot);
        let block = scene(m);
        let count = m.lbu(block.wrapping_add(0x9ce)).wrapping_add(1);
        m.sb(block.wrapping_add(0x9ce), count);
        call(m, SET_VISUAL, [2, 1, 0, 0x1e]);
        let style = u32::from(m.lbu(label));
        call(m, SET_VISUAL_STYLE, [5, style, 0, 0]);
        let block = scene(m);
        if m.lbu(block.wrapping_add(0x9ce)) >= 2 {
            call(m, SET_VISUAL, [3, 1, 0, 0x1e]);
            let style = u32::from(m.lbu(label.wrapping_sub(4)));
            call(m, SET_VISUAL_STYLE, [6, style, 0, 0]);
        }
        let block = scene(m);
        if m.lbu(block.wrapping_add(0x9ce)) >= 3 {
            call(m, SET_VISUAL, [4, 1, 0, 0x1e]);
            let style = u32::from(m.lbu(label.wrapping_sub(8)));
            call(m, SET_VISUAL_STYLE, [7, style, 0, 0]);
        }
        call(m, START_SOUND, [0x64, 0, 0, 0])
    })
}

/// 80102D1C (command, not reached): the gauge: +0x8C = arg0, +0x88 = 0,
/// +0x84 = arg4; in both hub tables the gauge primitive at +0x126C (x 0x20
/// / 0x120 / 0x20 + 24 * value, y 0x12C, u 0 / 12 * fill...) as the
/// original computes it from +0x84/+0x86. Returns 0.
pub fn command_gauge(m: &mut Machine) -> u32 {
    let block = scene(m);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let cmd2 = m.lw(block.wrapping_add(0x9c8));
    let frames = lh(m, cmd.wrapping_add(4));
    m.sw(block.wrapping_add(0x8c), frames);
    let value = m.lhu(cmd2.wrapping_add(0xc));
    m.sw(block.wrapping_add(0x88), 0);
    m.sh(block.wrapping_add(0x84), value);
    for table in 0..2u32 {
        let hub = m.lw(HUB);
        let block = scene(m);
        let p = m.lw(hub.wrapping_add(table << 2).wrapping_add(0x38));
        m.sh(p.wrapping_add(0x1274), 0x20);
        m.sh(p.wrapping_add(0x1276), 0x12c);
        m.sh(p.wrapping_add(0x127c), 0x120);
        m.sh(p.wrapping_add(0x127e), 0x12c);
        m.sh(p.wrapping_add(0x1284), 0x20);
        let v = lh(m, block.wrapping_add(0x84));
        m.sh(p.wrapping_add(0x128c), 0x120);
        m.sh(
            p.wrapping_add(0x1286),
            v.wrapping_mul(24).wrapping_add(0x12c) as u16,
        );
        let v = lh(m, block.wrapping_add(0x84));
        m.sb(p.wrapping_add(0x1278), 0);
        let block2 = scene(m);
        m.sh(
            p.wrapping_add(0x128e),
            v.wrapping_mul(24).wrapping_add(0x12c) as u16,
        );
        let w = lh(m, block2.wrapping_add(0x86));
        m.sb(p.wrapping_add(0x1280), 0xff);
        m.sb(p.wrapping_add(0x1279), w.wrapping_mul(12) as u8);
        let block = scene(m);
        let w = lh(m, block.wrapping_add(0x86));
        m.sb(p.wrapping_add(0x1288), 0);
        m.sb(p.wrapping_add(0x1281), w.wrapping_mul(12) as u8);
        let block = scene(m);
        let w = lh(m, block.wrapping_add(0x86));
        let v = lh(m, block.wrapping_add(0x84));
        m.sb(p.wrapping_add(0x1290), 0xff);
        m.sb(
            p.wrapping_add(0x1289),
            w.wrapping_mul(12)
                .wrapping_add(v.wrapping_mul(12).wrapping_sub(2)) as u8,
        );
        let block = scene(m);
        let w = lh(m, block.wrapping_add(0x86));
        let v = lh(m, block.wrapping_add(0x84));
        m.sb(
            p.wrapping_add(0x1291),
            w.wrapping_mul(12)
                .wrapping_add(v.wrapping_mul(12).wrapping_sub(2)) as u8,
        );
    }
    0
}

/// 80102E78 (PAL 800EB414 opcode 5, near): pan the looked-at point to
/// arg1/arg3 over arg0 frames (+0x78): speed = distance / frames (+0x74),
/// heading ratan2 (+0x7E). Returns the heading.
pub fn command_pan(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = scene(m);
        let cmd = m.lw(block.wrapping_add(0x9c8));
        let cmd2 = m.lw(block.wrapping_add(0x9c8));
        let frames = lh(m, cmd.wrapping_add(4));
        m.sw(block.wrapping_add(0x78), frames);
        let x = lh(m, cmd2.wrapping_add(6));
        let dx = position(x).wrapping_sub(m.lw(block.wrapping_add(0x20)));
        let z = lh(m, cmd2.wrapping_add(0xa));
        let dz = position(z).wrapping_sub(m.lw(block.wrapping_add(0x28)));
        let squares = dx.wrapping_mul(dx).wrapping_add(dz.wrapping_mul(dz));
        let distance = sdk::square_root0(m, squares);
        let block = scene(m);
        let frames = m.lw(block.wrapping_add(0x78));
        let speed = div_trap(m, distance, frames, 0x8010_2efc);
        m.sw(block.wrapping_add(0x74), speed);
        let heading = sdk::ratan2(m, dx, dz);
        let block = scene(m);
        m.sh(block.wrapping_add(0x7e), heading as u16);
        heading
    })
}

/// 80102F5C (command): look at arg1/arg3 (y 0). Returns the z value.
pub fn command_look(m: &mut Machine) -> u32 {
    let block = scene(m);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let cmd2 = m.lw(block.wrapping_add(0x9c8));
    let x = lh(m, cmd.wrapping_add(6));
    m.sw(block.wrapping_add(0x24), 0);
    m.sw(block.wrapping_add(0x20), position(x));
    let z = position(lh(m, cmd2.wrapping_add(0xa)));
    m.sw(block.wrapping_add(0x28), z);
    z
}

/// 80102F9C (command): place the marker at arg1/arg3 with height arg2
/// (+0x9BC..+0x9C0) showing arg0 (+0x9C4). Returns arg0.
pub fn command_marker(m: &mut Machine) -> u32 {
    let block = scene(m);
    let cmd = m.lw(block.wrapping_add(0x9c8));
    let x = lh(m, cmd.wrapping_add(6));
    let cmd = m.lw(block.wrapping_add(0x9c8));
    m.sh(block.wrapping_add(0x9bc), position(x) as u16);
    let y = m.lhu(cmd.wrapping_add(8));
    m.sh(block.wrapping_add(0x9be), y);
    let z = lh(m, cmd.wrapping_add(0xa));
    m.sh(block.wrapping_add(0x9c0), position(z) as u16);
    let id = lh(m, cmd.wrapping_add(4));
    m.sw(block.wrapping_add(0x9c4), id);
    id
}

// ---- 80102FF0 .. 80103834: objects --------------------------------------------------------

/// 80102FF0 (PAL 800EB654, changed): advance every moving object (state 2
/// with frames left) by its speed along its heading. Returns 0 (the loop
/// test).
pub fn tick_objects(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        for slot in 0..0x20u32 {
            let object = object(m, slot);
            if lh(m, object.wrapping_add(2)) != 2 || (lh(m, object.wrapping_add(4)) as i32) <= 0 {
                continue;
            }
            let angle = lh(m, object.wrapping_add(0x16));
            let sine = sdk::rsin(m, angle);
            let speed = lh(m, object.wrapping_add(8));
            let dx = divide_pow2(speed.wrapping_mul(sine) as i32, 12) as u32;
            let angle = lh(m, object.wrapping_add(0x16));
            let cosine = sdk::rcos(m, angle);
            let speed = lh(m, object.wrapping_add(8));
            let dz = divide_pow2(speed.wrapping_mul(cosine) as i32, 12) as u32;
            let x = lhu(m, object.wrapping_add(0xc));
            let frames = lhu(m, object.wrapping_add(4));
            m.sh(object.wrapping_add(0xc), x.wrapping_add(dx) as u16);
            let z = lhu(m, object.wrapping_add(0x10));
            m.sh(object.wrapping_add(4), frames.wrapping_sub(1) as u16);
            m.sh(object.wrapping_add(0x10), z.wrapping_add(dz) as u16);
        }
        0
    })
}

/// 801030E0 (PAL ov1 800D02B0, changed): the per-frame object step
/// (80102FF0). Returns its v0.
pub fn step_objects(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| tick_objects(m))
}

/// The eight corner divisions of 80103100: (stack halfword, negated, `div`
/// address). The corners are (-s, 0, s), (s, 0, s), (-s, 0, -s), (s, 0,
/// -s), with `s` = (frames in all - frames left) * 128 / frames in all
/// while the object appears; the zero y halfword follows each x.
const CORNER_DIVISIONS: [(u32, bool, u32); 8] = [
    (0x10, true, 0x8010_320c),
    (0x14, false, 0x8010_3254),
    (0x18, false, 0x8010_32a0),
    (0x1c, false, 0x8010_32e4),
    (0x20, true, 0x8010_3330),
    (0x24, true, 0x8010_337c),
    (0x28, false, 0x8010_33c4),
    (0x2c, true, 0x8010_3410),
];

/// 80103100 (PAL 800ECE7C, changed): every placed object (+0x9D0) within
/// 0x400 of the looked-at point is projected as its prebuilt POLY_FT4 (the
/// current hub table `[0x80112DC4]+0x40`, +0xB8C + slot*0x28): corners
/// scaled up while it appears (state 1), its rotation (RotMatrix_gte of
/// +0x14) and position, the view matrix +0x50 applied by inline MVMVAs,
/// RTPT/RTPS of the four corners into the quad's XY words, the screen
/// position (+0x1C/+0x1E) the midpoint of the first and fourth corners;
/// the quad is added to the hub's ordering-table entry `[+0x14]`. Returns
/// 0 (the loop test).
pub fn draw_objects(m: &mut Machine) -> u32 {
    framed(m, 0xd8, |m, sp| {
        let hub = m.lw(HUB);
        let table = m.lw(hub.wrapping_add(0x40));
        let corners = sp + 0x10;
        let rotation = sp + 0x50;
        let product = sp + 0x70;
        for slot in 0..0x20u32 {
            let block = scene(m);
            let mask = m.lw(block.wrapping_add(0x9d0));
            if ((mask as i32) >> (slot & 31)) & 1 == 0 {
                continue;
            }
            let object = m.lw(block.wrapping_add(slot << 2).wrapping_add(0x9dc));
            m.sh(object.wrapping_add(0x1c), 0xffff);
            m.sh(object.wrapping_add(0x1e), 0xffff);
            let x = lh(m, object.wrapping_add(0xc));
            let look_x = m.lw(block.wrapping_add(0x20));
            if abs(x.wrapping_sub(look_x)) as i32 >= 0x401 {
                continue;
            }
            let z = lh(m, object.wrapping_add(0x10));
            let look_z = m.lw(block.wrapping_add(0x28));
            if abs(z.wrapping_sub(look_z)) as i32 >= 0x401 {
                continue;
            }
            if lh(m, object.wrapping_add(2)) == 1 && (lh(m, object.wrapping_add(4)) as i32) > 0 {
                for (offset, negated, pc) in CORNER_DIVISIONS {
                    let total = lh(m, object.wrapping_add(6));
                    let left = lh(m, object.wrapping_add(4));
                    let mut scaled = total.wrapping_sub(left) << 7;
                    if negated {
                        scaled = scaled.wrapping_neg();
                    }
                    let s = div_trap(m, scaled, total, pc);
                    if offset % 8 == 0 {
                        m.sh(sp + offset + 2, 0);
                    }
                    m.sh(sp + offset, s as u16);
                }
                let left = m.lhu(object.wrapping_add(4));
                m.sh(object.wrapping_add(4), left.wrapping_sub(1));
            } else {
                for (offset, value) in [
                    (0x10u32, 0xff80u16),
                    (0x12, 0),
                    (0x14, 0x80),
                    (0x18, 0x80),
                    (0x1a, 0),
                    (0x1c, 0x80),
                    (0x20, 0xff80),
                    (0x22, 0),
                    (0x24, 0xff80),
                    (0x28, 0x80),
                    (0x2a, 0),
                    (0x2c, 0xff80),
                ] {
                    m.sh(sp + offset, value);
                }
            }
            sdk::rot_matrix_gte(m, object.wrapping_add(0x14), rotation);
            let at = sp + 0x90;
            for k in 0..3u32 {
                let value = lh(m, object.wrapping_add(0xc + k * 2));
                m.sw(at + k * 4, value);
            }
            sdk::trans_matrix(m, rotation, at);
            // inline MulMatrix: product = view rotation x object rotation
            let view = scene(m).wrapping_add(0x50);
            for index in 0..5u32 {
                let value = m.lw(view.wrapping_add(index * 4));
                m.gte.ctc2(index as usize, value);
            }
            for column in 0..3u32 {
                let from = rotation + column * 2;
                let t4 = lhu(m, from);
                let t5 = lhu(m, from + 6);
                let t6 = lhu(m, from + 0xc);
                m.gte.mtc2(9, t4);
                m.gte.mtc2(10, t5);
                m.gte.mtc2(11, t6);
                m.gte.mvmva(true, false, 0, 3, 3);
                let t4 = m.gte.mfc2(9);
                let t5 = m.gte.mfc2(10);
                let t6 = m.gte.mfc2(11);
                let to = product + column * 2;
                m.sh(to, t4 as u16);
                m.sh(to + 6, t5 as u16);
                m.sh(to + 0xc, t6 as u16);
            }
            // the object's translation through the view matrix
            let view = scene(m).wrapping_add(0x50);
            for index in 5..8u32 {
                let value = m.lw(view.wrapping_add(index * 4));
                m.gte.ctc2(index as usize, value);
            }
            let t5 = lhu(m, rotation + 0x18);
            let t4 = lhu(m, rotation + 0x14);
            m.gte.mtc2(0, t4 | (t5 << 16));
            lwc2(m, 1, rotation + 0x1c);
            m.gte.mvmva(true, false, 0, 0, 0);
            swc2(m, 25, product + 0x14);
            swc2(m, 26, product + 0x18);
            swc2(m, 27, product + 0x1c);
            super::ov2_p6::load_rot_trans(m, product);
            lwc2(m, 0, corners);
            lwc2(m, 1, corners + 4);
            lwc2(m, 2, corners + 8);
            lwc2(m, 3, corners + 0xc);
            lwc2(m, 4, corners + 0x10);
            lwc2(m, 5, corners + 0x14);
            m.gte.rtpt(true, false);
            let screen = sp + 0x30;
            swc2(m, 12, screen);
            swc2(m, 13, screen + 8);
            swc2(m, 14, screen + 0x10);
            depth(m, sp);
            lwc2(m, 0, corners + 0x18);
            lwc2(m, 1, corners + 0x1c);
            m.gte.rtps(true, false);
            swc2(m, 14, screen + 0x18);
            depth(m, sp);
            let x = (m.lh(screen) as i32).wrapping_add(i32::from(m.lh(screen + 0x18)));
            m.sh(object.wrapping_add(0x1c), half(x) as u16);
            let y = (m.lh(screen + 2) as i32).wrapping_add(i32::from(m.lh(screen + 0x1a)));
            m.sh(object.wrapping_add(0x1e), half(y) as u16);
            let quad = table
                .wrapping_add(slot.wrapping_mul(0x28))
                .wrapping_add(0xb8c);
            for k in 0..4u32 {
                let sx = m.lhu(screen + k * 8);
                m.sh(quad.wrapping_add(8 + k * 8), sx);
                let sy = m.lhu(screen + k * 8 + 2);
                m.sh(quad.wrapping_add(0xa + k * 8), sy);
            }
            let hub = m.lw(HUB);
            let entry = m.lw(hub.wrapping_add(0x14));
            let tag = m.lw(quad);
            let link = m.lw(entry);
            m.sw(quad, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
            let entry = m.lw(hub.wrapping_add(0x14));
            let link = m.lw(entry);
            m.sw(entry, (link & 0xff00_0000) | (quad & 0x00ff_ffff));
        }
        0
    })
}

/// The depth words after a projection: IR0 (+0xA0), FLAG (+0xA4) and SZ3
/// / 4 (+0xA8) on the stack.
fn depth(m: &mut Machine, sp: u32) {
    swc2(m, 8, sp + 0xa0);
    let flag = m.gte.cfc2(31);
    m.sw(sp + 0xa4, flag);
    let sz = m.gte.mfc2(19) as i32 >> 2;
    m.sw(sp + 0xa8, sz as u32);
}

/// 80103834 (PAL ov1 800D204C, changed): the per-frame object drawing
/// (80103100). Returns its v0.
pub fn step_draw_objects(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| draw_objects(m))
}

// ---- 80103854: the label panels -----------------------------------------------------------

/// 800DEE84 (another part): visual slot `a0`'s mode.
const VISUAL_MODE: u32 = 0x800d_ee84;
/// 800DEEA8 (another part): visual slot `a0`'s value.
const VISUAL_VALUE: u32 = 0x800d_eea8;
/// 800AC708: GetClut(x, y) (JP SDK).
const GET_CLUT: u32 = 0x800a_c708;

/// `(count - k) mod 4` and `(count - k) / 4`, truncated as compiled.
fn quarter(count: u32, k: u32) -> (u32, u32) {
    let d = count.wrapping_sub(k) as i32;
    let q = divide_pow2(d, 2);
    (d.wrapping_sub(q << 2) as u32, q as u32)
}

/// `value / 30` as compiled (multiply-high by 0x88888889, plus the
/// dividend, `sra 4`, minus its sign).
fn thirtieth(value: u32) -> u32 {
    let high = ((i64::from(value as i32) * i64::from(0x8888_8889u32 as i32)) >> 32) as i32;
    (high.wrapping_add(value as i32) >> 4).wrapping_sub(value as i32 >> 31) as u32
}

/// The two sprites of a label panel: `top` (+0xC u = 60 r, v 0xBB, CLUT
/// row 0x10 + q, x 0x11) and `bottom` (+0xC u = 56 r + 0x20, v 0xB4, CLUT
/// row 0x33 + q, x 0x11) for the label `count - k`; `y` the top sprite's
/// y, `y + 0x4A` the bottom's; `clut_call` takes the bottom CLUT from
/// GetClut (the original's second variant).
fn label_sprites(m: &mut Machine, top: u32, bottom: u32, k: u32, y: u32, clut_call: bool) {
    let block = scene(m);
    let (r, _) = quarter(u32::from(m.lbu(block.wrapping_add(0x9ce))), k);
    m.sb(top.wrapping_add(0xc), r.wrapping_mul(60) as u8);
    m.sb(top.wrapping_add(0xd), 0xbb);
    let block = scene(m);
    let (_, q) = quarter(u32::from(m.lbu(block.wrapping_add(0x9ce))), k);
    m.sh(
        top.wrapping_add(0xe),
        ((q.wrapping_add(0x10) << 6) | 0x1b) as u16,
    );
    m.sh(top.wrapping_add(8), 0x11);
    m.sh(top.wrapping_add(0xa), y as u16);
    let (r, _) = quarter(u32::from(m.lbu(block.wrapping_add(0x9ce))), k);
    m.sb(
        bottom.wrapping_add(0xc),
        r.wrapping_mul(56).wrapping_add(0x20) as u8,
    );
    m.sb(bottom.wrapping_add(0xd), 0xb4);
    let block = scene(m);
    let (_, q) = quarter(u32::from(m.lbu(block.wrapping_add(0x9ce))), k);
    let clut = if clut_call {
        call(m, GET_CLUT, [0x190, q.wrapping_add(0x33), 0, 0])
    } else {
        (q.wrapping_add(0x33) << 6) | 0x19
    };
    m.sh(bottom.wrapping_add(0xe), clut as u16);
    m.sh(bottom.wrapping_add(8), 0x11);
    m.sh(bottom.wrapping_add(0xa), y.wrapping_add(0x4a) as u16);
}

/// 80103854: the label panels: visual slots 2..4 give the panels' modes
/// (800DEE84) and slots 3/4 their slide (800DEEA8); the newest label (and
/// the one before) get their two sprites from the display buffer's
/// sprite sets (+0x8D4 / +0x96C, 0x28 per buffer; label count `+0x9CE`,
/// the panel shown unless its mode says it is gone), a fixed panel at
/// +0x924 + 0x24 * buffer; all are added to the hub's ordering-table entry
/// `[[0x80112DC4]+8] + 4` unless +0x9D6 hides the labels. Returns the
/// entry's last word.
pub fn draw_labels(m: &mut Machine) -> u32 {
    framed(m, 0x50, |m, sp| {
        let first = call(m, VISUAL_MODE, [2, 0, 0, 0]);
        let second = call(m, VISUAL_MODE, [3, 0, 0, 0]);
        let third = call(m, VISUAL_MODE, [4, 0, 0, 0]);
        m.sw(sp + 0x20, third);
        let second_slide = call(m, VISUAL_VALUE, [3, 0, 0, 0]);
        let third_slide = call(m, VISUAL_VALUE, [4, 0, 0, 0]);
        let context = m.gw(crate::common::CONTEXT);
        let buffer = m.lw(context.wrapping_add(4));
        let block = scene(m);
        let set = buffer.wrapping_mul(40);
        let mut top = block.wrapping_add(set).wrapping_add(0x8d4);
        let mut panel = block
            .wrapping_add(buffer.wrapping_mul(36))
            .wrapping_add(0x924);
        let mut bottom = block.wrapping_add(set).wrapping_add(0x96c);
        let hub = m.lw(HUB);
        let ot = m.lw(hub.wrapping_add(8)).wrapping_add(4);
        super::ov2_p6::add_prim_masked(m, ot, panel);
        m.sb(sp + 0x18, 0x80);
        m.sb(sp + 0x19, 0x80);
        m.sb(sp + 0x1a, 0x80);
        m.sb(sp + 0x1b, 0);
        let count = u32::from(m.lbu(block.wrapping_add(0x9ce)));
        panel = panel.wrapping_add(0xc);
        if count >= 2 || (count == 1 && first != 1) {
            if second == 0 || second == 2 {
                label_sprites(m, top, bottom, 1, 0x49, false);
                let block = scene(m);
                let count = u32::from(m.lbu(block.wrapping_add(0x9ce)));
                let label = m.lw(block
                    .wrapping_add(count.wrapping_sub(1) << 2)
                    .wrapping_add(0xa5c));
                let style = u32::from(m.lbu(label));
                let object = m.lw(block.wrapping_add(style << 2).wrapping_add(0x9dc));
                m.sh(sp + 0x10, 0x4d);
                m.sh(sp + 0x12, 0x6d);
                let x = m.lhu(object.wrapping_add(0x1c));
                m.sh(sp + 0x14, x);
                let y = m.lhu(object.wrapping_add(0x1e));
                m.sh(sp + 0x16, y);
            } else {
                let y = thirtieth(second_slide.wrapping_mul(107));
                label_sprites(m, top, bottom, 2, y.wrapping_add(0x49), true);
            }
        }
        let block = scene(m);
        top = top.wrapping_add(0x14);
        let count = u32::from(m.lbu(block.wrapping_add(0x9ce)));
        bottom = bottom.wrapping_add(0x14);
        if count >= 3 || (count == 2 && second != 1) {
            let third = m.lw(sp + 0x20);
            if third == 0 || third == 2 {
                label_sprites(m, top, bottom, 2, 0xb4, false);
            } else {
                let y = thirtieth(third_slide.wrapping_mul(311));
                label_sprites(m, top, bottom, 3, y.wrapping_add(0xb4), false);
            }
        }
        // link the shown sprites, the top set first
        for base in [0x8d4u32, 0x96c] {
            let context = m.gw(crate::common::CONTEXT);
            let buffer = m.lw(context.wrapping_add(4));
            let block = scene(m);
            let mut sprite = block
                .wrapping_add(buffer.wrapping_mul(40))
                .wrapping_add(base);
            let count = u32::from(m.lbu(block.wrapping_add(0x9ce)));
            if (count >= 2 || (count == 1 && first != 1)) && m.lbu(block.wrapping_add(0x9d6)) == 0 {
                super::ov2_p6::add_prim_masked(m, ot, sprite);
                sprite = sprite.wrapping_add(0x14);
            }
            let block = scene(m);
            let count = u32::from(m.lbu(block.wrapping_add(0x9ce)));
            if (count >= 3 || (count == 2 && second != 1)) && m.lbu(block.wrapping_add(0x9d6)) == 0
            {
                super::ov2_p6::add_prim_masked(m, ot, sprite);
            }
            if base == 0x8d4 {
                let block = scene(m);
                if m.lbu(block.wrapping_add(0x9d6)) == 0 {
                    super::ov2_p6::add_prim_masked(m, ot, panel);
                    panel = panel.wrapping_add(0xc);
                }
            }
        }
        super::ov2_p6::add_prim_masked(m, ot, panel)
    })
}
