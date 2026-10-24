//! Patches of the unreached training guidance (generated `hud::tree_41ff0`)
//! whose JP instructions differ from PAL, part 3 of the resident wave:
//! 80042B20 runs 80043BF0 for the course kinds 5 and 6, and 80042D98 reads
//! the mission halfword context +0x38C (PAL the byte 0x800BAEB4). Listed in
//! [`REPLACES`](super::REPLACES); the functions keep the generated names.
use crate::dispatch::call;
use crate::hud::tree_41ff0::*;
use crate::hud::{context, entities, gameplay};
use crate::machine::{Machine, SmallData};
use crate::sdk;

/// The JP mission number: the halfword at context +0x38C (PAL reads the
/// byte 0x800BAEB4).
fn jp_mission(m: &mut Machine) -> i32 {
    let ctx = context(m);
    i32::from(m.lh(ctx + 0x38c))
}

/// 8006CB30: pad buttons word (`[[gp+0x520]]`); reads no arguments.
const PAD_BUTTONS: u32 = 0x8006_cb30;

/// 8004291C (`id`): request guidance message `id` (gp+0x464 = 1,
/// gp+0x465 = `id` + 0xC, gp+0x466 = 0); reads only a0.
const MESSAGE: u32 = 0x8004_291c;

/// 80030778 (angles, matrix): YXZ rotation of three 14-bit angles.
const ROTATION: u32 = 0x8003_0778;

/// 80030D54 (y, x): 14-bit angle of (x, y).
const ATAN2: u32 = 0x8003_0d54;

/// 800A60F8 rsin(angle).
const RSIN: u32 = 0x800a_60f8;

/// 800A61C8 rcos(angle).
const RCOS: u32 = 0x800a_61c8;

/// 800A6A08 SetMulMatrix(m0, m1): rotation registers = m0 * m1.
const SET_MUL_MATRIX: u32 = 0x800a_6a08;

/// Multiplier of the compiler's division by 100 (with shift 5) and by 50
/// (shift 4), see [`div_by`].
const BY_100: i32 = 0x51eb_851f;

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// `mult x, magic; mfhi; sra shift; subu (x >> 31)`: the compiler's
/// truncating signed division by a constant.
fn div_by(x: i32, magic: i32, shift: u32) -> i32 {
    let high = ((i64::from(x) * i64::from(magic)) >> 32) as i32;
    (high >> shift).wrapping_sub(x >> 31)
}

/// 8004291C(`id`) when the frame counter `[gameplay+0xC]` & 0x7F is
/// `phase`.
fn message_at(m: &mut Machine, id: u32, phase: u32) {
    let game = gameplay(m);
    if m.lw(game + 0xc) & 0x7f == phase {
        call(m, MESSAGE, [id, 0, 0, 0]);
    }
}

/// The next free LINE_F2: buffer + 0xC00 + 0x10 * count.
fn next_line(m: &mut Machine) -> u32 {
    let set = m.gw(SET);
    let count = m.lw(set + 0xc);
    let buffer = m.gw(BUFFER);
    buffer.wrapping_add((count << 4).wrapping_add(0xc00))
}

/// Fill the LINE_F2 at `line` with color `rgb` and (x0, y0)-(x1, y1),
/// then count it in set +0xC.
fn line_f2(m: &mut Machine, line: u32, rgb: [u8; 3], xy: [u32; 4]) {
    m.sb(line + 4, rgb[0]);
    m.sb(line + 5, rgb[1]);
    m.sb(line + 6, rgb[2]);
    m.sh(line + 8, xy[0] as u16);
    m.sh(line + 0xa, xy[1] as u16);
    m.sh(line + 0xc, xy[2] as u16);
    m.sh(line + 0xe, xy[3] as u16);
    let set = m.gw(SET);
    let count = m.lw(set + 0xc);
    m.sw(set + 0xc, count.wrapping_add(1));
}

/// Fill consecutive LINE_F2 from `line` with (color, (x0, y0, x1, y1))
/// each, as [`line_f2`].
fn lines_f2(m: &mut Machine, line: u32, lines: &[([u8; 3], [u32; 4])]) {
    let mut at = line;
    for &(rgb, xy) in lines {
        line_f2(m, at, rgb, xy);
        at = at.wrapping_add(0x10);
    }
}

/// Bright and dim green line colors.
const GREEN: [u8; 3] = [0, 0xff, 0];

const DIM: [u8; 3] = [0, 0x40, 0];

/// Project `count` points of a gate (x from `xs`; y from `ys` minus the
/// drop `8 * gate^2`; z from `zs` or 0, plus the depth `-0x400 * gate`)
/// with RotTransPers into the screen words at frame +0x38..; false at the
/// first point whose FLAG word is negative (an error).
fn project(
    m: &mut Machine,
    frame: u32,
    xs: u32,
    ys: u32,
    zs: Option<u32>,
    count: u32,
    gate: u32,
) -> bool {
    let drop = gate.wrapping_mul(gate) << 3;
    let depth = gate.wrapping_mul(0x400).wrapping_neg();
    let vector = frame + 0x30;
    for point in 0..count {
        let x = m.lhu(xs + point * 2);
        m.sh(vector, x);
        let y = u32::from(m.lhu(ys + point * 2)).wrapping_sub(drop);
        m.sh(vector + 2, y as u16);
        let z = match zs {
            Some(zs) => u32::from(m.lhu(zs + point * 2)).wrapping_add(depth),
            None => depth,
        };
        m.sh(vector + 4, z as u16);
        let sxy = frame + 0x38 + point * 4;
        sdk::rot_trans_pers(m, vector, sxy, frame + 0x70, frame + 0x74);
        if (m.lw(frame + 0x74) as i32) < 0 {
            return false;
        }
    }
    true
}

/// 80042B20 (JP; PAL 80041FF0, near, `call+:1 imm:1 ins:4 op:1`) (`view`): select this frame's buffer (80042BF4), draw the
/// guidance of the current course kind unless pad buttons 0x240 are
/// held, and link the packets (80042C2C).
pub fn guidance_41ff0(m: &mut Machine, view: u32) {
    const FRAME: u32 = 0x20;
    m.push_frame(FRAME);
    let mode = m.gw(MODE);
    let course = (mode << 2).wrapping_add(COURSES);
    begin_420b4(m);
    let buttons = call(m, PAD_BUTTONS, [view, 0, 0, 0]);
    if buttons & 0x240 == 0 {
        match m.lbu(course) {
            0 | 1 => approach_42258(m, view),
            2 | 3 => {
                alignment_42a3c(m, view);
            }
            4 => attitude_430b0(m),
            // JP: course kinds 5 and 6 run 80043BF0 (PAL: nothing)
            5 | 6 => {
                call(m, 0x8004_3bf0, [view, 0, 0, 0]);
            }
            _ => {}
        }
    }
    link_420ec(m);
    m.pop_frame(FRAME);
}

/// 80042D98 (JP; PAL 80042258, near, `del:1 ins:1 op:1`; the mission is the
/// halfword context +0x38C) (`view`): approach guidance to the guide point `P =
/// [view+0x38]` (+0/+4/+8 position, +0x10 angles, +0x18 range,
/// +0x1C/+0x20/+0x24 offsets, +0x2E heading, +0x34 state).
///
/// The camera sits at `(P - own) >> 12` (own = `[gameplay+0x28]` +4..,
/// x/z moved by rsin/rcos of angle +0x12 `>> 8`, or `>> 3` in mission 25)
/// rotated by `[gameplay+0x4C]`, under `[gameplay+0x4C] * rotation(P
/// angles)`. 16 gates at depth `-0x400 * gate`, dropping `8 * gate^2`,
/// are projected: left and right brackets (80043E0C) and a chevron
/// (80043E74), each drawn only when all its points project without a
/// FLAG error. Turn arrows follow `2 * atan2(+0x1C, -+0x24) + heading`
/// (left for 0x201..0x1FFF with message 0xD from 0x801, right for
/// 0x2000..0x3DFF with message 0xC below 0x3800). With state 1 message 3
/// replaces the pitch cue; otherwise the player pitch (`+0x18`) against
/// `atan2(+0x20, -+0x24)` gives a down arrow when over 0x200 inside range
/// 0x801..0x3FFF (message 0xB from 0x801 with +0x24 negative), else an up
/// arrow (message 0xA) when view +0x30 is at most 0xFFFFF or the error is
/// below -0x200. Last, the player position in the point's frame (angle y
/// mirrored to `0x4000 - y`) places a cross at `(-x / 100, (-y - 0x46) /
/// 100)` (each clamped to ±0x40) from (0xA0, 0x78) over a dim center
/// cross.
///
/// Frame (0xA8): +0x10 MATRIX, +0x30 SVECTOR, +0x38.. screen words,
/// +0x48 VECTOR, +0x58 its transform, +0x68 angles, +0x70/+0x74 depth
/// cue and FLAG, +0x78 the player entity; `view` is saved at +0xA8 (the
/// caller's argument slot).
pub fn approach_42258(m: &mut Machine, view: u32) {
    const FRAME: u32 = 0xa8;
    let game = gameplay(m);
    let frame = m.push_frame(FRAME);
    let matrix = frame + 0x10;
    let relative = frame + 0x48;
    let result = frame + 0x58;
    let angles = frame + 0x68;
    m.sw(frame + 0xa8, view);
    let point = m.lw(view + 0x38);
    let table = entities(m);
    let player = m.lw(table);
    m.sw(frame + 0x78, player);
    for (offset, own_offset) in [(0, 4), (4, 8), (8, 0xc)] {
        let own = m.lw(game + 0x28);
        let value = m.lw(point + offset);
        let own_value = m.lw(own + own_offset);
        m.sw(relative + offset, sra(value.wrapping_sub(own_value), 12));
    }
    let shift = if jp_mission(m) == 0x19 { 3 } else { 8 };
    let angle = m.lh(point + 0x12) as i32 as u32;
    let sine = call(m, RSIN, [angle, 0, 0, 0]);
    let x = m.lw(relative);
    m.sw(relative, x.wrapping_add(sra(sine, shift)));
    let angle = m.lh(point + 0x12) as i32 as u32;
    let cosine = call(m, RCOS, [angle, 0, 0, 0]);
    let z = m.lw(relative + 8);
    m.sw(relative + 8, z.wrapping_add(sra(cosine, shift)));
    let game = gameplay(m);
    sdk::apply_matrix_lv(m, game + 0x4c, relative, result);
    for offset in [0, 4, 8] {
        let word = m.lw(result + offset);
        m.sw(matrix + 0x14 + offset, word);
    }
    call(m, ROTATION, [point + 0x10, matrix, 0, 0]);
    let game = gameplay(m);
    call(m, SET_MUL_MATRIX, [game + 0x4c, matrix, 0, 0]);
    sdk::set_trans_matrix(m, matrix);

    for xs in [LEFT_X, RIGHT_X] {
        for gate in 0..16 {
            if project(m, frame, xs, BRACKET_Y, Some(BRACKET_Z), 3, gate) {
                let [a, b, c] = [0x38, 0x3c, 0x40].map(|offset| m.lw(frame + offset));
                gate_432c4(m, a, b, c, gate);
            }
        }
    }
    for gate in 0..16 {
        if project(m, frame, CHEVRON_X, CHEVRON_Y, None, 4, gate) {
            let [a, b, c, d] = [0x38, 0x3c, 0x40, 0x44].map(|offset| m.lw(frame + offset));
            chevron_4332c(m, a, b, c, d);
        }
    }

    // The player relative to the point, in the point's frame.
    let first = m.lw(point + 0x10);
    let second = m.lw(point + 0x14);
    m.sw(angles, first);
    m.sw(angles + 4, second);
    let yaw = m.lhu(angles + 2);
    m.sh(angles + 2, 0x4000u16.wrapping_sub(yaw));
    let table = entities(m);
    for offset in [0, 4, 8] {
        let player = m.lw(table);
        let mine = m.lw(player + 8 + offset);
        let theirs = m.lw(point + offset);
        m.sw(relative + offset, sra(mine.wrapping_sub(theirs), 12));
    }
    call(m, ROTATION, [angles, matrix, 0, 0]);
    sdk::apply_matrix_lv(m, matrix, relative, result);

    // Turn cue.
    let y = sra(m.lw(point + 0x1c), 12);
    let x = sra(m.lw(point + 0x24).wrapping_neg(), 12);
    let bearing = call(m, ATAN2, [y, x, 0, 0]);
    let heading = (bearing << 1).wrapping_add(u32::from(m.lhu(point + 0x2e))) & 0x3fff;
    if heading.wrapping_sub(0x201) < 0x1dff {
        if (heading as i32) >= 0x801 {
            message_at(m, 0xd, 0);
        }
        arrow_433b4(m, 0, 0x50, 0x78);
    } else if heading.wrapping_sub(0x2000) < 0x1e00 {
        if (heading as i32) < 0x3800 {
            message_at(m, 0xc, 0);
        }
        arrow_433b4(m, 1, 0xf0, 0x78);
    }

    // Pitch cue.
    if m.lw(point + 0x34) == 1 {
        message_at(m, 3, 0);
    } else {
        let y = sra(m.lw(point + 0x20), 12);
        let x = sra(m.lw(point + 0x24).wrapping_neg(), 12);
        let slope = call(m, ATAN2, [y, x, 0, 0]);
        let player = m.lw(frame + 0x78);
        let pitch = u32::from(m.lhu(player + 0x18)).wrapping_sub(slope);
        let error = i32::from(pitch as u16 as i16);
        let high = error >= 0x201 && {
            let range = m.lw(point + 0x18) as i32;
            (0x801..0x4000).contains(&range)
        };
        if high {
            if (m.lw(point + 0x24) as i32) < 0 && error >= 0x801 {
                message_at(m, 0xb, 0x40);
            }
            arrow_433b4(m, 3, 0xa0, 0xb4);
        } else {
            let view = m.lw(frame + 0xa8);
            if (m.lw(view + 0x30) as i32) <= 0xf_ffff || error < -0x200 {
                message_at(m, 0xa, 0x40);
                arrow_433b4(m, 2, 0xa0, 0x3c);
            }
        }
    }

    // Deviation cross over the dim center cross.
    let x = m.lw(result) as i32;
    let dx = div_by(x.wrapping_neg(), BY_100, 5).clamp(-0x40, 0x40);
    let y = m.lw(result + 4) as i32;
    let dy = div_by(y.wrapping_neg().wrapping_sub(0x46), BY_100, 5).clamp(-0x40, 0x40);
    let line = next_line(m);
    let x = dx.wrapping_add(0xa0) as u32;
    let y = dy.wrapping_add(0x78) as u32;
    let cross = [
        (GREEN, [x, 0x64, x, 0x8c]),
        (GREEN, [0x8c, y, 0xb4, y]),
        (DIM, [0xa0, 0x64, 0xa0, 0x8c]),
        (DIM, [0x8c, 0x78, 0xb4, 0x78]),
    ];
    lines_f2(m, line, &cross);
    m.pop_frame(FRAME);
}
