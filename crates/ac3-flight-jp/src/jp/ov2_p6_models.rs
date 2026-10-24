//! JP overlay 2: the hub's entity model pass (the aircraft viewer's 3D
//! models), translated from the JP instructions. It is the overlay's own
//! version of the flight engine's pass (PAL resident 80066B64 with
//! 80065974 and 80065CC4, `render`): the entities only, a wider distance
//! limit (0x2C000), no plain records, no draw callbacks; the resident list
//! preparation and drawing routines do the rest.
use super::ov2_p6::{call, div_trap, framed, swc2};
use crate::{
    common::{ENTITIES, GAMEPLAY},
    machine::{Machine, SmallData},
};

/// `[0x800BD090]`: the model light/color parameters (PAL 0x800BAF18).
const MODEL_PARAMETERS: u32 = 0x800b_d090;
/// The scratchpad (the drawing routines' work area).
const SCRATCH: u32 = 0x1f80_0000;

fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
fn slt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
fn abs(value: u32) -> u32 {
    if (value as i32) < 0 {
        value.wrapping_neg()
    } else {
        value
    }
}

/// 801073B0: list the visible entities (flags 0x14000) into 16-byte
/// records at `list`, skipping the camera's own entity: +0 model, +1 side,
/// +2 id, +4 kind (1 ground; 2 the locked target while the camera's
/// +0x50 has bits 6, else 4), +5 1 for the target, +6 the shape class
/// (3/4/5 for shapes 0x2B/0x23/0x24, else 0), +8 the position pointer
/// (entity +8), +0xC the flags. Terminates the list with 0xFF and returns
/// the terminator.
pub fn list_entities(m: &mut Machine, list: u32) -> u32 {
    let mut out = list;
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay.wrapping_add(0x28));
    let own = m.lw(camera.wrapping_add(0xc4));
    let target = m.lw(camera.wrapping_add(0xc8));
    for index in 0..0x20u32 {
        let table = m.gw(ENTITIES);
        let entity = m.lw((index << 2).wrapping_add(table));
        if m.lw(entity) & 0x1_4000 != 0x1_4000 || own == index {
            continue;
        }
        let kind = if m.lbu(entity.wrapping_add(5)) & 1 == 0 {
            1
        } else if target != index {
            4
        } else {
            let gameplay = m.gw(GAMEPLAY);
            let camera = m.lw(gameplay.wrapping_add(0x28));
            if m.lw(camera.wrapping_add(0x50)) & 6 != 0 {
                2
            } else {
                4
            }
        };
        m.sb(out.wrapping_add(4), kind);
        let model = m.lbu(entity.wrapping_add(4));
        m.sb(out, model);
        let side = m.lbu(entity.wrapping_add(5));
        m.sb(out.wrapping_add(1), side);
        let id = m.lhu(entity.wrapping_add(6));
        m.sw(out.wrapping_add(8), entity.wrapping_add(8));
        m.sh(out.wrapping_add(2), id);
        let flags = m.lw(entity);
        m.sw(out.wrapping_add(0xc), flags);
        let shape = match m.lh(entity.wrapping_add(0x48)) {
            0x2b => 3,
            0x23 => 4,
            0x24 => 5,
            _ => 0,
        };
        m.sh(out.wrapping_add(6), shape);
        m.sb(out.wrapping_add(5), u8::from(target == index));
        out = out.wrapping_add(0x10);
    }
    m.sb(out, 0xff);
    out
}

/// Rotate column `from` (three halfwords 6 apart) by the GTE rotation
/// (MVMVA sf=1 IR, no translation) into `to`.
fn rotate_column(m: &mut Machine, from: u32, to: u32) {
    let t4 = lhu(m, from);
    let t5 = lhu(m, from.wrapping_add(6));
    let t6 = lhu(m, from.wrapping_add(0xc));
    m.gte.mtc2(9, t4);
    m.gte.mtc2(10, t5);
    m.gte.mtc2(11, t6);
    m.gte.mvmva(true, false, 0, 3, 3);
    let t4 = m.gte.mfc2(9);
    let t5 = m.gte.mfc2(10);
    let t6 = m.gte.mfc2(11);
    m.sh(to, t4 as u16);
    m.sh(to.wrapping_add(6), t5 as u16);
    m.sh(to.wrapping_add(0xc), t6 as u16);
}

/// Control registers 0..4 (the rotation) from the MATRIX at `matrix`.
fn load_rotation(m: &mut Machine, matrix: u32) {
    for index in 0..5u32 {
        let value = m.lw(matrix.wrapping_add(index * 4));
        m.gte.ctc2(index as usize, value);
    }
}

/// MAC1..MAC3 at `at` (three `swc2`).
fn store_mac123(m: &mut Machine, at: u32) {
    swc2(m, 25, at);
    swc2(m, 26, at.wrapping_add(4));
    swc2(m, 27, at.wrapping_add(8));
}

/// The loop state of 80107518 (its saved registers and stack slots).
struct Lists {
    frame: u32,
    entry: u32,
    textured: u32,
    target: u32,
    aircraft: u32,
    camera: u32,
    lod: u32,
    lod_range: u32,
    eye: u32,
    view: u32,
    light: u32,
    half_w: u32,
    half_h: u32,
    parameters: u32,
}

/// 80107518: transform each listed entity (`list`, 16-byte records) into
/// its display list: textured records (56 bytes, `textured`, kinds 1 and
/// 8), the locked target's lit records (160 bytes, `target`, kind 2) and
/// the other lit records (`[sp+0x10]`, the 5th argument); entities beyond
/// 0x2C000 (in 1024ths) or of no lit kind are skipped. The level of detail
/// follows the camera's +0x24; the screen position list is the 7th
/// argument. Returns the 6th argument (whose +0x10 it clears).
pub fn build_lists(m: &mut Machine, models: u32, list: u32, textured: u32, target: u32) -> u32 {
    framed(m, 0x90, |m, frame| {
        m.sw(frame + 0x90, models);
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay.wrapping_add(0x28));
        let lod = lh(m, camera.wrapping_add(0x24));
        let aircraft = m.lw(frame + 0xa0);
        let lod_range = (lod << 2).wrapping_add(lod << 3);
        m.sw(frame + 0x44, lod);
        m.sw(frame + 0x38, camera.wrapping_add(4));
        m.sw(frame + 0x3c, gameplay.wrapping_add(0x4c));
        let half_w = sra(lhu(m, camera.wrapping_add(0x20)) << 16, 17);
        m.sw(frame + 0x40, gameplay.wrapping_add(0x6c));
        let half_h = sra(lhu(m, camera.wrapping_add(0x22)) << 16, 17);
        m.sw(frame + 0x48, half_w);
        m.sw(frame + 0x4c, half_h);
        let mut c = Lists {
            frame,
            entry: list,
            textured,
            target,
            aircraft,
            camera,
            lod,
            lod_range,
            eye: camera.wrapping_add(4),
            view: gameplay.wrapping_add(0x4c),
            light: gameplay.wrapping_add(0x6c),
            half_w,
            half_h,
            parameters: 0,
        };
        if m.lbu(c.entry) != 0xff {
            loop {
                build_entry(m, &mut c);
                c.entry = c.entry.wrapping_add(0x10);
                if m.lbu(c.entry) == 0xff {
                    break;
                }
            }
        }
        m.sw(c.textured.wrapping_add(8), u32::MAX);
        m.sw(c.target, u32::MAX);
        m.sw(c.aircraft, u32::MAX);
        let plain = m.lw(frame + 0xa4);
        m.sw(plain.wrapping_add(0x10), 0);
        plain
    })
}

/// One iteration of the 80107518 loop (801075D0..8010811C).
fn build_entry(m: &mut Machine, c: &mut Lists) {
    let entry = c.entry;
    let frame = c.frame;
    let position = m.lw(entry.wrapping_add(8));
    let dx = m.lw(position).wrapping_sub(m.lw(c.eye));
    let dy = m
        .lw(position.wrapping_add(4))
        .wrapping_sub(m.lw(c.eye.wrapping_add(4)));
    let dz = m
        .lw(position.wrapping_add(8))
        .wrapping_sub(m.lw(c.eye.wrapping_add(8)));
    let mut x = sra(dx, 10);
    let mut y = sra(dy, 10);
    let mut z = sra(dz, 10);
    let distance = abs(x).wrapping_add(abs(y)).wrapping_add(abs(z));
    if slt(0x2_c000, distance) {
        return;
    }
    if m.lbu(entry.wrapping_add(4)) & 0xf == 0 {
        return;
    }
    let mut index = u32::from(m.lbu(entry)) << 2;
    c.parameters = m.lw(MODEL_PARAMETERS);
    if slt(c.lod_range << 2, distance) {
        index = index.wrapping_add(2);
    } else if slt(c.lod_range << 1, distance) {
        index = index.wrapping_add(1);
    }
    let models = m.lw(frame + 0x90);
    let table = m.lw(models.wrapping_add(0xc));
    let slot = (index << 2).wrapping_add(table);
    let model = m.lw(slot);
    let level = if slt(c.lod_range, distance) {
        0
    } else if slt(sra(c.lod_range, 1), distance) {
        1
    } else {
        2
    };
    let mesh = m.lw(model.wrapping_add(4 * (level + 1)));
    let shift = 2 - level;
    x = sra(x, shift);
    y = sra(y, shift);
    z = sra(z, shift);
    load_rotation(m, c.view);
    m.gte.mtc2(0, (x & 0xffff) | (y << 16));
    m.gte.mtc2(1, z);
    m.gte.rtps(true, false);
    let kind = m.lbu(entry.wrapping_add(4));
    let lit_offset = 4 * (level + 1);
    if kind & 9 != 0 {
        let record = c.textured;
        let side = m.lbu(entry.wrapping_add(1));
        m.sb(record, side);
        let id = m.lhu(entry.wrapping_add(2));
        m.sh(record.wrapping_add(2), id);
        let models = m.lw(frame + 0x90);
        let table = m.lw(models.wrapping_add(0xc));
        m.sw(record.wrapping_add(8), level);
        m.sw(record.wrapping_add(0x10), model);
        m.sw(record.wrapping_add(0xc), mesh);
        m.sw(record.wrapping_add(0x34), 0);
        m.sw(record.wrapping_add(4), table.wrapping_add(index << 2));
        store_mac123(m, record.wrapping_add(0x28));
    } else if kind & 2 != 0 {
        let record = c.target;
        m.sw(c.aircraft.wrapping_add(0x98), 0);
        let models = m.lw(frame + 0x90);
        let table = m.lw(models.wrapping_add(0xc));
        let parts = m.lw(table.wrapping_add(0xc));
        for i in 0..2u32 {
            let part = m.lw(parts.wrapping_add(i * 4));
            let value = m.lw(part.wrapping_add(lit_offset));
            m.sw(record.wrapping_add(i * 4 + 4), value);
            m.sw(record.wrapping_add(i * 4 + 0x18), part);
        }
        for (from, to) in [(8u32, 0x20u32), (0x1c, 0x24), (0x24, 0x28)] {
            let value = m.lw(parts.wrapping_add(from));
            m.sw(record.wrapping_add(to), value);
        }
        for i in 0..10u32 {
            let part = m.lw(parts.wrapping_add(i * 4 + 8));
            let value = m.lw(part.wrapping_add(lit_offset));
            m.sw(record.wrapping_add(i * 4 + 0x2c), value);
            match i {
                0 => m.sw(record.wrapping_add(0xc), value),
                5 => m.sw(record.wrapping_add(0x10), value),
                7 => m.sw(record.wrapping_add(0x14), value),
                _ => {}
            }
        }
        m.sw(record, level);
        store_mac123(m, record.wrapping_add(0x6c));
    } else {
        let record = c.aircraft;
        m.sw(record.wrapping_add(0x98), 0);
        let models = m.lw(frame + 0x90);
        let table = m.lw(models.wrapping_add(0xc));
        let parts = m.lw(table.wrapping_add(0xc));
        for i in 0..2u32 {
            let part = m.lw(parts.wrapping_add(i * 4));
            let value = m.lw(part.wrapping_add(lit_offset));
            m.sw(record.wrapping_add(i * 4 + 4), value);
            m.sw(record.wrapping_add(i * 4 + 0x18), part);
        }
        for (from, to) in [(8u32, 0x20u32), (0x1c, 0x24), (0x24, 0x28)] {
            let value = m.lw(parts.wrapping_add(from));
            m.sw(record.wrapping_add(to), value);
        }
        let p0 = m.lw(parts.wrapping_add(8));
        let p5 = m.lw(parts.wrapping_add(0x1c));
        let p7 = m.lw(parts.wrapping_add(0x24));
        let v0 = m.lw(p0.wrapping_add(lit_offset));
        let v5 = m.lw(p5.wrapping_add(lit_offset));
        let v7 = m.lw(p7.wrapping_add(lit_offset));
        m.sw(record.wrapping_add(0x14), v7);
        m.sw(record.wrapping_add(0xc), v0);
        m.sw(record.wrapping_add(0x10), v5);
        m.sw(record, level);
        store_mac123(m, record.wrapping_add(0x6c));
    }
    // 801079E4: depth and screen tests
    let depth = sra(m.gte.mfc2(27), level);
    let radius = sra(lh(m, model.wrapping_add(2)), 2 - level);
    if slt(depth, radius.wrapping_neg()) {
        return;
    }
    let sxy = m.gte.mfc2(14);
    let kind = m.lbu(entry.wrapping_add(4));
    if kind & 2 != 0 {
        m.sw(c.target.wrapping_add(0x54), depth);
    } else if kind & 4 != 0 {
        m.sw(c.aircraft.wrapping_add(0x54), depth);
    }
    let depth = if (depth as i32) > 0 { depth } else { 1 };
    let mut size = div_trap(m, radius.wrapping_mul(c.lod), depth, 0x8010_7a60);
    let sx = sra(sxy << 16, 16);
    let sy = sra(sxy, 16);
    if slt(radius << 4, size) {
        size = radius << 2;
    }
    if slt(c.half_w.wrapping_add(size), abs(sx)) || slt(c.half_h.wrapping_add(size), abs(sy)) {
        return;
    }
    let position = m.lw(entry.wrapping_add(8));
    call(
        m,
        0x8003_0778,
        [position.wrapping_add(0x10), frame + 0x10, 0, 0],
    );
    let flags = m.lw(entry.wrapping_add(0xc));
    if flags != 0 {
        let positions = m.lw(frame + 0xa8);
        let count = m.lw(positions);
        let record = positions.wrapping_add(count << 3);
        m.sw(record.wrapping_add(4), flags);
        m.sh(record.wrapping_add(8), sx as u16);
        m.sh(record.wrapping_add(0xa), sy as u16);
        m.sw(positions, count.wrapping_add(1));
    }
    let kind = m.lbu(entry.wrapping_add(4));
    if kind & 9 != 0 {
        let record = c.textured;
        rotate_column(m, frame + 0x10, record.wrapping_add(0x14));
        rotate_column(m, frame + 0x12, record.wrapping_add(0x16));
        rotate_column(m, frame + 0x14, record.wrapping_add(0x18));
        c.textured = record.wrapping_add(0x38);
        return;
    }
    let record = if kind & 2 != 0 { c.target } else { c.aircraft };
    if kind & 2 != 0 && m.lbu(entry.wrapping_add(5)) != 0 {
        locked_target_matrix(m, c, record, level);
    } else {
        rotate_column(m, frame + 0x10, record.wrapping_add(0x58));
        rotate_column(m, frame + 0x12, record.wrapping_add(0x5a));
        rotate_column(m, frame + 0x14, record.wrapping_add(0x5c));
    }
    load_rotation(m, c.light);
    rotate_column(m, frame + 0x10, record.wrapping_add(0x78));
    rotate_column(m, frame + 0x12, record.wrapping_add(0x7a));
    rotate_column(m, frame + 0x14, record.wrapping_add(0x7c));
    if kind & 2 != 0 {
        c.target = record.wrapping_add(0xa0);
    } else {
        c.aircraft = record.wrapping_add(0xa0);
    }
}

/// 80107C54..80107D8C: the locked target's matrix from the camera angles
/// minus the model parameter offsets (scaled down as the camera rolls),
/// through 800308F0; its translation the negated camera offset shifted by
/// the level of detail.
fn locked_target_matrix(m: &mut Machine, c: &Lists, record: u32, level: u32) {
    let camera = c.camera;
    let frame = c.frame;
    let ax = lh(m, camera.wrapping_add(0xb4));
    let ay = lh(m, camera.wrapping_add(0xb6));
    let pitch = lhu(m, camera.wrapping_add(0xb4));
    let mut roll = abs(ax).wrapping_add(abs(ay)) << 2;
    if !slt(roll, 0x1001) {
        roll = 0x1000;
    }
    let scale = 0x1000u32.wrapping_sub(roll);
    let parameters = c.parameters;
    let mut a0 = sra(lhu(m, parameters.wrapping_add(2)) << 16, 20).wrapping_mul(scale);
    if slt(a0, 0) {
        a0 = a0.wrapping_add(0xfff);
    }
    let mut v1 = sra(lhu(m, parameters.wrapping_add(4)) << 16, 21).wrapping_mul(scale);
    let a3 = sra(a0, 12);
    if slt(v1, 0) {
        v1 = v1.wrapping_add(0xfff);
    }
    let mut a2 = sra(lhu(m, parameters) << 16, 19).wrapping_mul(scale);
    let v1 = sra(v1, 12);
    if slt(a2, 0) {
        a2 = a2.wrapping_add(0xfff);
    }
    m.sh(frame + 0x30, pitch.wrapping_sub(a3) as u16);
    let yaw = lhu(m, camera.wrapping_add(0xb6));
    m.sh(frame + 0x32, yaw.wrapping_sub(v1) as u16);
    let roll_angle = lhu(m, camera.wrapping_add(0xb8));
    m.sh(frame + 0x34, roll_angle.wrapping_sub(sra(a2, 12)) as u16);
    call(
        m,
        0x8003_08f0,
        [frame + 0x30, record.wrapping_add(0x58), a2, a3],
    );
    let shift = 6u32.wrapping_sub(level);
    for (i, offset) in [0x2cu32, 0x2e, 0x30].into_iter().enumerate() {
        let value = sra(lh(m, camera.wrapping_add(offset)).wrapping_neg(), shift);
        m.sw(record.wrapping_add(0x6c + i as u32 * 4), value);
    }
}

/// 8010817C: the entity model pass in its 0x1740-byte frame: the entity
/// list (801073B0), the display lists (80107518: textured +0x420, target
/// +0x1220, lit +0x1360, plain +0x14A0), then the resident preparation
/// (80066C80, 80066DCC, 80067088) and drawing (SetRotMatrix of the view,
/// 800626E0, 80062BD4, 800637B0 twice) with the ordering table `ot` and
/// primitive cursor `primitives` in the scratchpad; `positions` gets the
/// screen positions. Returns the new primitive cursor.
pub fn model_pass(m: &mut Machine, models: u32, ot: u32, primitives: u32, positions: u32) -> u32 {
    framed(m, 0x1740, |m, sp| {
        let list = sp + 0x20;
        m.sw(positions, 0);
        list_entities(m, list);
        let aircraft = sp + 0x1360;
        let plain = sp + 0x14a0;
        m.sw(sp + 0x18, positions);
        let textured = sp + 0x420;
        let target = sp + 0x1220;
        m.sw(sp + 0x10, aircraft);
        m.sw(sp + 0x14, plain);
        build_lists(m, models, list, textured, target);
        m.sw(SCRATCH, ot);
        m.sw(SCRATCH + 4, primitives);
        call(m, 0x8006_6c80, [plain, SCRATCH, 0, 0]);
        call(m, 0x8006_6dcc, [textured, SCRATCH, 0, 0]);
        call(m, 0x8006_7088, [target, SCRATCH, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        call(m, 0x800a_7048, [gameplay.wrapping_add(0x4c), 0, 0, 0]);
        call(m, 0x8006_26e0, [plain, SCRATCH, 0, 0]);
        call(m, 0x8006_2bd4, [textured, SCRATCH, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let shade = m.lhu(gameplay.wrapping_add(0xc));
        m.sh(SCRATCH + 0x3f4, shade);
        call(m, 0x8006_37b0, [target, SCRATCH, 0, 0]);
        call(m, 0x8006_37b0, [aircraft, SCRATCH, 0, 0]);
        m.lw(SCRATCH + 4)
    })
}
