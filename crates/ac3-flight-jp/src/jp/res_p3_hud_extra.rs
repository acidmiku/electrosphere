//! Patches of unreached `hud` routines (generated `hud::extras` and
//! `hud::tree_62ea0`) whose JP instructions differ from PAL, part 3 of the
//! resident wave: the radar marker 8003E274 (radar centre y 0x2E, PAL
//! 0x2C), the impact marker 80066590 (mission halfword context +0x38C,
//! weapon mode bytes +0x3AA/+0x3AB, PAL +0x3AE/+0x3AF) and the attitude
//! dart 8006477C (geometry offset y 0x78, PAL 0x80). Listed in
//! [`REPLACES`](super::REPLACES); the functions keep the generated names.
use crate::hud::extras::fall_steps_652a4;
use crate::hud::tree_62ea0::dart_packets;
use crate::hud::{context, entities, gameplay, mips_div};
use crate::machine::{GP, Machine, SmallData};
use crate::{sdk, shared};

/// The radar centre y on the 240-line screen (PAL [`RADAR_CENTRE`]).
const RADAR_CENTRE_Y: i32 = 0x2e;

/// The JP mission number: the halfword at context +0x38C (PAL reads the
/// byte 0x800BAEB4).
fn jp_mission(m: &mut Machine) -> i32 {
    let ctx = context(m);
    i32::from(m.lh(ctx + 0x38c))
}

/// `[gp+0x434]`: the HUD primitive block (+0x18 optional group flags:
/// bit 0 the two lines at area +0xC54, bit 1 the three triangles at area
/// +0xC74, linked by 8003C690).
const HUD_BLOCK: u32 = 0x434;

/// `[gp+0x438]`: this frame's HUD primitive area.
const HUD_AREA: u32 = 0x438;

/// `[gp+0x43C]`: pointer to the radar basis (+0 x and +4 z of the
/// player, position >> 12; +8 sin(yaw) / 4; +0xC cos(yaw) / 4; +0x10
/// yaw).
const RADAR_BASIS: u32 = 0x43c;

/// 800C6D7C: radar range (position units that map to the 32-pixel
/// radar radius).
const RADAR_RANGE: u32 = 0x800c_6d7c;

/// `gp+0xF4`: angle offsets (two halfwords) of the marker's radar lines.
const LINE_ANGLES: u32 = GP + 0xf4;

/// `gp+0xF8`: per radar line the shift (byte) that sets its length
/// (`sine >> shift`).
const LINE_SHIFTS: u32 = GP + 0xf8;

/// 800104AC: 1.14 sine halfwords, 0x4000 steps per turn (cosine of step
/// `a` at entry `a + 0x1000`).
const SINE: u32 = 0x8001_04ac;

/// Radar lines: two `LINE_F2` (0x10 bytes) from HUD area +0xC54.
const MARKER_LINES: u32 = 0xc54;

/// Marker triangles: three `POLY_F3` (0x14 bytes) from HUD area +0xC74.
const MARKER_TRIANGLES: u32 = 0xc74;

/// Centre of the radar in screen pixels (both axes).
const RADAR_CENTRE: i32 = 0x2c;

/// 800C6D6E: locked target slot (0xFF: none).
const TARGET_SLOT: u32 = 0x800c_6d6e;

/// 800C6FD0: per entity slot flag word (0x400: not a valid aim point).
const SLOT_FLAGS: u32 = 0x800c_6fd0;

/// 800C6D60: HUD state block (+0x08..+0x0A line color).
const STATE: u32 = 0x800c_6d60;

/// `[gp+0x500]`: the HUD packet pool descriptor (+0x14 count of lines
/// used this frame).
const POOL: u32 = 0x500;

/// `[gp+0x504]`: the pool buffer of this frame (`LINE_F2` packets of
/// 0x10 bytes from +0x3D8).
const BUFFER: u32 = 0x504;

const LINE_COUNT: u32 = 0x14;

const LINE_BASE: u32 = 0x3d8;

/// Weapon mode whose bombs keep the aircraft's horizontal speed; the
/// other modes reaching 80066590 travel 1.5 times as far.
const DIRECT_MODE: u8 = 9;

/// The impact marker of 80066590: (x0, y0, x1, y1) of each of the six
/// lines relative to the projected point: a cross, then a diamond.
const IMPACT_LINES: [[i16; 4]; 6] = [
    [-10, -10, 10, 10],
    [-10, 10, 10, -10],
    [0, -6, 6, 0],
    [6, 0, 0, 6],
    [0, 6, -6, 0],
    [-6, 0, 0, -6],
];

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// `sra`.
fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}

/// 800B2C18: the four SVECTOR vertices of the dart (8 bytes each).
const VERTICES: u32 = 0x800b_2c18;

/// 800A7418 NormalClip(sxy0, sxy1, sxy2): SXY0..SXY2 = the arguments,
/// NCLIP, returns MAC0 (not in `sdk.rs`).
const NORMAL_CLIP: u32 = 0x800a_7418;

/// The dart's faces in the original order: vertex indices (NormalClip
/// argument and packet order) and the green level of each vertex (the
/// nose, vertex 1, is bright).
const FACES: [([u32; 3], [u8; 3]); 4] = [
    ([0, 2, 1], [0x10, 0x10, 0x40]),
    ([0, 1, 3], [0x10, 0x40, 0x10]),
    ([0, 3, 2], [0x10, 0x10, 0x10]),
    ([1, 2, 3], [0x40, 0x10, 0x10]),
];

/// 8003E274 (JP; PAL 8003DAB4, near, `imm:1`; radar centre y 0x2E, PAL 0x2C) (`marker`): draw the fixed map marker. `marker` points to
/// six words (8003DE5C copies them from 80023DEC into its frame): a
/// position VECTOR (+0 x, +4 y, +8 z) and a heading halfword at +0x12.
///
/// Its radar offset from the player (`x >> 12 - basis.x`, `basis.z -
/// z >> 12`) has length SquareRoot0(dx² + dz²) (with both components
/// `>> 4` and the root `<< 4` when one is 0x4000 or more); beyond the
/// radar range nothing is drawn. Rotated by the radar basis and scaled
/// by `32 / range` it must lie within 31 pixels; then two red lines
/// (`LINE_F2` at area +0xC54, +0xC64) start at the radar point, angled
/// by `[gp+0xF4..]` + yaw - heading and shortened by `[gp+0xF8..]`,
/// and group flag word `[gp+0x434]+0x18` becomes 1. Within distance
/// 0x7FFF (80083DFC) and with a valid RotTransPers (camera-relative
/// position `>> 12`, rotation gameplay +0x4C, no translation), three
/// `POLY_F3` (area +0xC74..) stack above the screen point, `size =
/// clamp((0x8000 - distance) >> 10, 2, 8)` wide, colored by gameplay
/// +0xC (bits 2..3) plus the triangle index, and the flag word gains 2.
/// Returns 0 on every path.
pub fn radar_marker_3dab4(m: &mut Machine, marker: u32) -> u32 {
    const FRAME: u32 = 0x58;
    let frame = m.push_frame(FRAME);
    marker_body_3dab4(m, marker, frame);
    m.pop_frame(FRAME);
    0
}

/// The body of [`radar_marker_3dab4`] inside its frame (`frame` +0x10
/// the translation-only MATRIX, +0x30 the SVECTOR, +0x38 the screen xy,
/// +0x40 the depth-cue word, +0x44 the GTE flag).
fn marker_body_3dab4(m: &mut Machine, marker: u32, frame: u32) {
    let basis = m.gw(RADAR_BASIS);
    let x = sra(m.lw(marker), 12).wrapping_sub(m.lw(basis)) as i32;
    let z = m
        .lw(at(basis, 4))
        .wrapping_sub(sra(m.lw(at(marker, 8)), 12)) as i32;
    let length = if x.wrapping_abs() < 0x4000 && z.wrapping_abs() < 0x4000 {
        let sum = x.wrapping_mul(x).wrapping_add(z.wrapping_mul(z));
        sdk::square_root0(m, sum as u32)
    } else {
        let (x, z) = (x >> 4, z >> 4);
        let sum = x.wrapping_mul(x).wrapping_add(z.wrapping_mul(z));
        sdk::square_root0(m, sum as u32) << 4
    };
    let range = m.lw(RADAR_RANGE) as i32;
    if range < length as i32 {
        return;
    }
    let basis = m.gw(RADAR_BASIS);
    let cos = m.lw(at(basis, 0xc)) as i32;
    let sin = m.lw(at(basis, 8)) as i32;
    // `break 7`/`break 6` guard both divisions in the original (a zero
    // range is reachable only for a zero length).
    let across = (x.wrapping_mul(cos).wrapping_add(z.wrapping_mul(sin)) >> 12) << 5;
    let rx = mips_div(across, range);
    let along = (x
        .wrapping_neg()
        .wrapping_mul(sin)
        .wrapping_add(z.wrapping_mul(cos))
        >> 12)
        << 5;
    let rz = mips_div(along, range);
    if rx.wrapping_add(0x1f) as u32 >= 0x3f || rz.wrapping_add(0x1f) as u32 >= 0x3f {
        return;
    }
    let (px, pz) = (
        rx.wrapping_add(RADAR_CENTRE),
        rz.wrapping_add(RADAR_CENTRE_Y),
    );
    for i in 0..2u32 {
        let offset = m.lhu(LINE_ANGLES + i * 2);
        let area = m.gw(HUD_AREA);
        let basis = m.gw(RADAR_BASIS);
        let heading = m.lhu(at(marker, 0x12));
        let shift = u32::from(m.lbu(LINE_SHIFTS + i)) & 31;
        let line = at(area, MARKER_LINES + i * 0x10);
        let yaw = m.lhu(at(basis, 0x10));
        m.sb(at(line, 5), 0);
        m.sb(at(line, 6), 0);
        m.sh(at(line, 8), px as u16);
        m.sh(at(line, 0xa), pz as u16);
        let turn = u32::from(yaw).wrapping_sub(u32::from(heading));
        let angle = u32::from(offset).wrapping_add(turn) & 0x3fff;
        let sine = i32::from(m.lh(SINE + angle * 2));
        m.sb(at(line, 4), 0xff);
        let cosine = i32::from(m.lh(SINE + (angle + 0x1000) * 2));
        m.sh(at(line, 0xe), pz.wrapping_add(cosine >> shift) as u16);
        m.sh(at(line, 0xc), px.wrapping_add(sine >> shift) as u16);
    }
    let hud = m.gw(HUD_BLOCK);
    let game = gameplay(m);
    m.sw(at(hud, 0x18), 1);
    let camera = m.lw(at(game, 0x28));
    let distance = shared::f_800827b4(m, [marker, at(camera, 4), 0, 0]) as i32;
    if 0x7fff < distance {
        return;
    }
    let game = gameplay(m);
    for k in 0..3 {
        let camera = m.lw(at(game, 0x28));
        let position = m.lw(at(marker, k * 4));
        let eye = m.lw(at(camera, 4 + k * 4));
        m.sh(
            frame + 0x30 + k * 2,
            sra(position.wrapping_sub(eye), 12) as u16,
        );
    }
    for offset in [0x2c, 0x28, 0x24] {
        m.sw(frame + offset, 0);
    }
    sdk::set_rot_matrix(m, at(game, 0x4c));
    sdk::set_trans_matrix(m, frame + 0x10);
    sdk::rot_trans_pers(m, frame + 0x30, frame + 0x38, frame + 0x40, frame + 0x44);
    if (m.lw(frame + 0x44) as i32) < 0 {
        return;
    }
    let size = (0x8000i32.wrapping_sub(distance) >> 10).clamp(2, 8) as u32;
    let area = m.gw(HUD_AREA);
    for i in 0..3u32 {
        let triangle = at(area, MARKER_TRIANGLES + i * 0x14);
        let game = gameplay(m);
        let phase = sra(m.lw(at(game, 0xc)), 2) & 3;
        m.sb(at(triangle, 6), 0);
        let red = phase.wrapping_add(i) << 6;
        m.sb(at(triangle, 4), red as u8);
        m.sb(at(triangle, 5), ((red & 0xc0) >> 1) as u8);
        let sx = u32::from(m.lhu(frame + 0x38));
        m.sh(at(triangle, 8), sx as u16);
        let sy = u32::from(m.lhu(frame + 0x3a));
        let tip = sy.wrapping_sub((i * 2 + 1).wrapping_mul(size));
        m.sh(at(triangle, 0xa), tip as u16);
        m.sh(at(triangle, 0xc), sx.wrapping_add(size) as u16);
        m.sh(at(triangle, 0x10), sx.wrapping_sub(size) as u16);
        let base = sy.wrapping_sub((i * 2 + 2).wrapping_mul(size));
        m.sh(at(triangle, 0x12), base as u16);
        m.sh(at(triangle, 0xe), base as u16);
    }
    let hud = m.gw(HUD_BLOCK);
    let flags = m.lw(at(hud, 0x18));
    m.sw(at(hud, 0x18), flags | 2);
}

/// 80066590 (JP; PAL 80064DC4, near, `del:3 imm:3 ins:4`; `view`): the bomb
/// impact marker (JP: mission halfword context +0x38C, weapon mode bytes
/// +0x3AA/+0x3AB). The ground level is the y
/// word (+0xC) of the locked target (800C6D6E; in missions other than
/// 0xD only without slot flag 0x400) when it is not above the player
/// (entity 0), else view +0x2C. The fall time [`fall_steps_652a4`] over
/// the height above that level, starting at 1.5 times the player's
/// vertical speed (+0x3C), scales the horizontal speed (+0x38, +0x40;
/// 1.5 times unless weapon mode byte context +0x3AA is 9, or 8003A5C0
/// returns 1 and context +0x3AB is 9) into the drop offset. The impact
/// point (player x/z + offset, the ground level), relative to the camera
/// `[gameplay+0x28]` (+4..) and `>> 12`, goes through RotTransPers with
/// the camera rotation (800308F0 of camera +0x14) and no translation;
/// the six `LINE_F2` of [`IMPACT_LINES`] at the screen point take the
/// next pool lines, colored by HUD state +8..+0xA, whatever the GTE flag.
pub fn impact_marker_64dc4(m: &mut Machine, view: u32) {
    const FRAME: u32 = 0x70;
    let frame = m.push_frame(FRAME);
    let table = entities(m);
    let player = m.lw(table);
    let level = target_level(m, table, player).unwrap_or_else(|| m.lw(at(view, 0x2c)));
    let height = m.lw(at(player, 0xc));
    let drop = if (height as i32) < (level as i32) {
        level.wrapping_sub(height)
    } else {
        0
    };
    let climb = m.lw(at(player, 0x3c));
    let steps = fall_steps_652a4(drop, climb.wrapping_add(sra(climb, 1)));
    let direct = weapon_mode(m, 0x3aa) == DIRECT_MODE
        // 8003A5C0 reads no arguments (the original passes stale ones).
        || (shared::f_8003a08c(m, [0; 4]) == 1 && weapon_mode(m, 0x3ab) == DIRECT_MODE);
    let speed = |value: u32| {
        if direct {
            value
        } else {
            value.wrapping_add(sra(value, 1))
        }
    };
    let vx = m.lw(at(player, 0x38));
    let dx = speed(vx).wrapping_mul(steps);
    let vz = m.lw(at(player, 0x40));
    let dz = speed(vz).wrapping_mul(steps);
    let game = gameplay(m);
    let camera = m.lw(at(game, 0x28));
    let point = [
        m.lw(at(player, 8)).wrapping_add(dx),
        level,
        m.lw(at(player, 0x10)).wrapping_add(dz),
    ];
    for (k, value) in (0..3u32).zip(point) {
        let eye = m.lw(at(camera, 4 + k * 4));
        m.sh(
            frame + 0x10 + k * 2,
            sra(value.wrapping_sub(eye), 12) as u16,
        );
    }
    let camera = m.lw(at(game, 0x28));
    shared::f_800306ac(m, [at(camera, 0x14), frame + 0x20, 0, 0]);
    for offset in [0x3c, 0x38, 0x34] {
        m.sw(frame + offset, 0);
    }
    sdk::set_rot_matrix(m, frame + 0x20);
    sdk::set_trans_matrix(m, frame + 0x20);
    sdk::rot_trans_pers(m, frame + 0x10, frame + 0x18, frame + 0x50, frame + 0x54);
    let pool = m.gw(POOL);
    let buffer = m.gw(BUFFER);
    let used = m.lw(at(pool, LINE_COUNT));
    let mut line = at(buffer, (used << 4).wrapping_add(LINE_BASE));
    for [x0, y0, x1, y1] in IMPACT_LINES {
        for c in 0..3 {
            let value = m.lbu(STATE + 8 + c);
            m.sb(at(line, 4 + c), value);
        }
        let sx = m.lhu(frame + 0x18);
        let sy = m.lhu(frame + 0x1a);
        m.sh(at(line, 8), sx.wrapping_add(x0 as u16));
        m.sh(at(line, 0xa), sy.wrapping_add(y0 as u16));
        m.sh(at(line, 0xc), sx.wrapping_add(x1 as u16));
        m.sh(at(line, 0xe), sy.wrapping_add(y1 as u16));
        let pool = m.gw(POOL);
        let used = m.lw(at(pool, LINE_COUNT));
        m.sw(at(pool, LINE_COUNT), used.wrapping_add(1));
        line = at(line, 0x10);
    }
    m.pop_frame(FRAME);
}

/// Weapon mode byte `[gp+0x2EC] + offset` (+0x3AA, or +0x3AB which
/// 8003EF40 selects while 8003A5C0 reports an available target).
fn weapon_mode(m: &mut Machine, offset: u32) -> u8 {
    let context = context(m);
    m.lbu(at(context, offset))
}

/// The ground level of [`impact_marker_64dc4`] from the locked target:
/// its y word (+0xC) when the slot (800C6D6E) is not 0xFF, slot flag
/// 0x400 is clear (not checked in mission 0xD) and the target is not
/// above the `player` (signed y, larger is lower); otherwise `None`.
fn target_level(m: &mut Machine, table: u32, player: u32) -> Option<u32> {
    let slot = m.lbu(TARGET_SLOT);
    if slot == 0xff {
        return None;
    }
    let index = u32::from(slot);
    if jp_mission(m) != 0xd && m.lw(SLOT_FLAGS + index * 4) & 0x400 != 0 {
        return None;
    }
    let target = m.lw(at(table, index << 2));
    let level = m.lw(at(target, 0xc));
    ((level as i32) >= (m.lw(at(player, 0xc)) as i32)).then_some(level)
}

/// 8006477C (JP; PAL 80062FB0, near, `imm:1`) (`x`, `y`): SetGeomOffset(`x`, `y`), RotTransPers of the four
/// vertices at 800B2C18 into the screen words of its 0x50-byte frame
/// (+0x10..+0x1C; depth cue +0x38 and flag +0x3C shared), SetGeomOffset
/// back to (0xA0, 0x78) (PAL 0x80), the NormalClip of each face into +0x28..+0x34,
/// then the packets ([`dart_packets`]). Returns the original v0: the
/// line count after the last edge, or the (negative) NormalClip of face 3
/// when that edge is culled. Only caller: 8006466C, with (0xA0, 0xC8).
pub fn dart_62fb0(m: &mut Machine, x: u32, y: u32) -> u32 {
    const FRAME: u32 = 0x50;
    let frame = m.push_frame(FRAME);
    let screen = frame + 0x10; // four sxy words
    let depth_cue = frame + 0x38;
    let flag = frame + 0x3c;
    let areas = frame + 0x28; // four NormalClip results
    sdk::set_geom_offset(m, x, y);
    for i in 0..4 {
        sdk::rot_trans_pers(m, VERTICES + 8 * i, screen + 4 * i, depth_cue, flag);
    }
    sdk::set_geom_offset(m, 0xa0, 0x78);
    for (face, (vertices, _)) in FACES.into_iter().enumerate() {
        let [a, b, c] = vertices.map(|v| m.lw(screen + 4 * v));
        // a3 is a leftover; NormalClip reads only a0..a2.
        let area = crate::dispatch::call(m, NORMAL_CLIP, [a, b, c, 0]);
        m.sw(areas + 4 * face as u32, area);
    }
    let v0 = dart_packets(m, frame);
    m.pop_frame(FRAME);
    v0
}
