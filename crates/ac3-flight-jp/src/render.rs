//! Camera and 3D object rendering called by the flight frame body 800339E0:
//! the camera update 8007B438, the view matrix 800332E4, the lit object
//! systems 80033324 (8007DB00, 80085204), the sorted object pass 800333D8
//! (8006C5EC) and the entity model pass 8003342C (80068334), with every
//! routine of their observed dynamic subtrees.
//!
//! The renderers build GPU primitive packets in RAM and link them into the
//! 4096-entry depth ordering table, using GTE projection and lighting. All
//! GTE use is translated instruction by instruction through `m.gte`; none of
//! these routines issues a command within two instructions of a register
//! write (checked statically for every command), so Redux's write-to-use
//! pipeline never applies.
//!
//! Stack buffers whose addresses the original passes to callees live in the
//! original frame (`Machine::push_frame`); scalar locals stay in Rust. The
//! projectile pass 8008700C runs its callees on a scratchpad stack, so the
//! callee-saved registers they spill there are written too (the scratchpad
//! is compared).
//!
//! Camera modes: 1 and 1024 (80077A30), 2 (80077DD0), 4 (8007895C), 16
//! (80078B28), 32 (80078D70), 64 (80078FDC), 256 (800791E8), 512 (the replay
//! camera 8007AFFC with its scripts, reached through the tables at
//! 800B40AC), 2048 (80078234) and the debug cameras 0x20000000 (8007754C)
//! and 0x40000000 (80077004) with the readout 80077460; the camera setup
//! 80073928, 80076DBC and 8007961C; the lens flare 8008B7AC, the heat
//! shimmer 8008C644 and the explosion trail renderer 8009236C with its
//! clipping and glow layers (800916A8..80092168). The loading-screen
//! visuals (80036440, 800364C4 with the starfield 80083FE0, 80036A84 with
//! 800365FC and 8005E40C) and the debug hex printer 800277F0 also live here.
//! The table-reached renderers live here too: the object classes 2/3
//! (80085EAC, 800860A4), the debris kinds 0/2 (80089F44, 8008ABF0), the
//! ambient types 2-5 (rain, snow, motes, leaves), the lit callbacks
//! (afterburner 8008C0A0, engine glow 8008C9D4) and the draw callbacks
//! (sparkle 8008D678, glints 8008D140/8008D244/8008D348).
//! Routines no capture reaches are validated against the original code with
//! the test-only oracle (`tests/render.rs`).
use crate::{
    common::{ENTITIES, GAMEPLAY},
    dispatch,
    machine::{Fault, Machine, SmallData},
    sdk, shared, widescreen,
};
use ac3_gte::{ctrl, reg};

/// Scratchpad base: the renderers keep their working state there.
const SCRATCH: u32 = 0x1f80_0000;

// GTE command words, exactly as encoded in the original instructions.
/// RTPS sf=1.
const RTPS: u32 = 0x018_0001;
/// RTPT sf=1.
const RTPT: u32 = 0x028_0030;
/// NCLIP.
const NCLIP: u32 = 0x140_0006;
/// AVSZ3 (encoded with sf=1, which AVSZ3 ignores).
const AVSZ3: u32 = 0x158_002d;
/// MVMVA sf=1, rotation matrix x IR vector, no translation.
const MVMVA_RT_IR: u32 = 0x049_e012;
/// INTPL sf=1.
const INTPL: u32 = 0x098_0011;

fn cop2(m: &mut Machine, code: u32) {
    m.gte
        .command(code)
        .expect("translated GTE command words are valid");
}

// ---- register-level helpers ----------------------------------------------------

fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
/// `sra`/`srav`: arithmetic right shift by the low five bits.
fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
/// `sllv`: left shift by the low five bits.
fn sllv(value: u32, shift: u32) -> u32 {
    value << (shift & 31)
}
/// `slt`: signed less-than.
fn slt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `subu` from zero when negative (the compiled `abs`).
fn abs(value: u32) -> u32 {
    if (value as i32) < 0 {
        0u32.wrapping_sub(value)
    } else {
        value
    }
}
/// `mult` + `mflo`.
fn mul_lo(a: u32, b: u32) -> u32 {
    (a as i32).wrapping_mul(b as i32) as u32
}
/// `div` + `mflo` for divisors the original has already proven nonzero.
fn div_lo(a: u32, b: u32) -> u32 {
    let (a, b) = (a as i32, b as i32);
    if b == 0 {
        if a < 0 { 1 } else { u32::MAX }
    } else {
        a.wrapping_div(b) as u32
    }
}
/// Low 24 bits: a primitive address inside an ordering-table link.
fn link(address: u32) -> u32 {
    (address << 8) >> 8
}

// ---- GTE helpers (inline libgte macros in the original) ------------------------

/// Inline `gte_SetRotMatrix`: five words into R11R12..R33.
fn load_rotation(m: &mut Machine, matrix: u32) {
    let t4 = m.lw(matrix);
    let t5 = m.lw(matrix + 4);
    m.gte.ctc2(ctrl::R11R12, t4);
    m.gte.ctc2(ctrl::R13R21, t5);
    let t4 = m.lw(matrix + 8);
    let t5 = m.lw(matrix + 12);
    let t6 = m.lw(matrix + 16);
    m.gte.ctc2(ctrl::R22R23, t4);
    m.gte.ctc2(ctrl::R31R32, t5);
    m.gte.ctc2(ctrl::R33, t6);
}

/// Inline `gte_SetTransMatrix` on a word vector: TRX/TRY/TRZ.
fn load_translation(m: &mut Machine, vector: u32) {
    let t4 = m.lw(vector);
    let t5 = m.lw(vector + 4);
    m.gte.ctc2(ctrl::TRX, t4);
    let t6 = m.lw(vector + 8);
    m.gte.ctc2(ctrl::TRY, t5);
    m.gte.ctc2(ctrl::TRZ, t6);
}

/// Inline `gte_SetLightMatrix`: five words into L11L12..L33.
fn load_light(m: &mut Machine, matrix: u32) {
    let t4 = m.lw(matrix);
    let t5 = m.lw(matrix + 4);
    m.gte.ctc2(ctrl::L11L12, t4);
    m.gte.ctc2(ctrl::L11L12 + 1, t5);
    let t4 = m.lw(matrix + 8);
    let t5 = m.lw(matrix + 12);
    let t6 = m.lw(matrix + 16);
    m.gte.ctc2(ctrl::L11L12 + 2, t4);
    m.gte.ctc2(ctrl::L11L12 + 3, t5);
    m.gte.ctc2(ctrl::L33, t6);
}

/// One column of the inline matrix product (`gte_MulMatrix0` body): the
/// halfwords at `src`+0/+6/+12 into IR1..IR3, rotation x IR, and IR1..IR3
/// back as halfwords at `dst`+0/+6/+12.
fn rotate_column(m: &mut Machine, src: u32, dst: u32) {
    let t4 = lhu(m, src);
    let t5 = lhu(m, src + 6);
    let t6 = lhu(m, src + 12);
    m.gte.mtc2(reg::IR1, t4);
    m.gte.mtc2(reg::IR2, t5);
    m.gte.mtc2(reg::IR3, t6);
    cop2(m, MVMVA_RT_IR);
    let t4 = m.gte.mfc2(reg::IR1);
    let t5 = m.gte.mfc2(reg::IR2);
    let t6 = m.gte.mfc2(reg::IR3);
    m.sh(dst, t4 as u16);
    m.sh(dst + 6, t5 as u16);
    m.sh(dst + 12, t6 as u16);
}

/// `swc2 MAC1..MAC3` to three consecutive words.
fn store_mac123(m: &mut Machine, out: u32) {
    for i in 0..3 {
        let value = m.gte.mfc2(reg::MAC1 + i);
        m.sw(out + i as u32 * 4, value);
    }
}

/// `swc2` of one data register (no extension side effect beyond `mfc2`).
fn swc2(m: &mut Machine, register: usize, address: u32) {
    let value = m.gte.mfc2(register);
    m.sw(address, value);
}

/// `lwc2` of one data register.
fn lwc2(m: &mut Machine, register: usize, address: u32) {
    let value = m.lw(address);
    m.gte.mtc2(register, value);
}

/// Inline addPrim: link `packet` (tag length `code`) at `slot`.
fn add_prim(m: &mut Machine, slot: u32, packet: u32, code: u32) {
    let tag = m.lw(slot) | code;
    m.sw(packet, tag);
    m.sw(slot, link(packet));
}

// ==== 8003342C: entity model pass ===================================================

/// `[0x800BD090]`: model light/colour parameters copied into entity lists.
const MODEL_PARAMETERS: u32 = 0x800b_d090;
/// Per-model-type draw callbacks (80067494 stores them in display entries,
/// 80062BD4 calls them).
const DRAW_CALLBACKS: u32 = 0x800b_2c48;
/// Per-shading-type callbacks for the lit entity lists (800637B0).
const LIT_CALLBACKS: u32 = 0x800b_2c60;

/// 8003342C: draw the entity models. A 0x220-byte frame holds the screen
/// position list that 80068334 fills; the new primitive pointer is stored in
/// gameplay +0x14 and returned.
pub fn draw_entity_models(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x220);
    let gameplay = m.gw(GAMEPLAY);
    let ot = m.lw(gameplay + 0x10) + 4;
    let primitives = m.lw(gameplay + 0x14);
    let next = entity_model_pass(m, gameplay + 0x244, ot, primitives, frame + 0x10);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
    m.pop_frame(0x220);
    next
}

/// 80068334: build the entity display lists in its 0x1AD0-byte frame, then
/// draw them. `a0` is the model table, `ot` the ordering table, `primitives`
/// the primitive cursor and `positions` the screen position list
/// (count word + 8-byte entries). Returns the new primitive cursor.
pub fn entity_model_pass(
    m: &mut Machine,
    models: u32,
    ot: u32,
    primitives: u32,
    positions: u32,
) -> u32 {
    let frame = m.push_frame(0x1ad0);
    let list = frame + 32;
    m.sw(positions, 0);
    let end = list_entities(m, list);
    list_objects(m, end);
    // Display lists: 56-byte textured records (kinds 1 and 8), 160-byte lit
    // records for the locked target (kind 2) and the other aircraft (kind
    // 4), 20-byte records for plain objects (kind 16).
    let textured = frame + 1568;
    let target = frame + 5208;
    let aircraft = frame + 5688;
    let plain = frame + 6168;
    m.sw(frame + 24, positions);
    m.sw(frame + 16, aircraft);
    m.sw(frame + 20, plain);
    build_display_lists(m, models, list, textured, target);
    m.sw(SCRATCH, ot);
    m.sw(SCRATCH + 4, primitives);
    prepare_plain_list(m, plain, SCRATCH);
    prepare_textured_list(m, textured, SCRATCH);
    prepare_lit_list(m, target, SCRATCH);
    let gameplay = m.gw(GAMEPLAY);
    sdk::set_rot_matrix(m, gameplay + 76);
    draw_plain_list(m, plain, SCRATCH);
    draw_textured_list(m, textured, SCRATCH);
    let gameplay = m.gw(GAMEPLAY);
    let shade = m.lhu(gameplay + 12);
    m.sh(SCRATCH + 1012, shade);
    draw_lit_list(m, target, SCRATCH);
    draw_lit_list(m, aircraft, SCRATCH);
    let next = m.lw(SCRATCH + 4);
    m.pop_frame(0x1ad0);
    next
}

// JP-REVIEW(R0345): near function: PAL 80065974 -> JP 80067144, imm:5, 5 instructions differ (whole routine)
/// 80067144: list the visible entities (flags 0x14000) into 16-byte
/// records, skipping the camera's own entity; returns the end of the list
/// (not terminated).
// JP-PATCH: replaced by `crate::jp::res_p2_render::list_entities` (src/jp/mod.rs REPLACES, PATCHES.md)
fn list_entities(m: &mut Machine, list: u32) -> u32 {
    crate::jp::res_p2_render::list_entities(m, list)
}

// JP-REVIEW(R0346): near function: PAL 80065BB0 -> JP 80067380, imm:3, 3 instructions differ (whole routine)
/// 80067380: append the visible secondary objects (`[gp+0x2D8]`, flags
/// 0x14000) and terminate the list with 0xFF; returns the terminator.
// JP-PATCH: replaced by `crate::jp::res_p2_render::list_objects` (src/jp/mod.rs REPLACES, PATCHES.md)
fn list_objects(m: &mut Machine, list: u32) -> u32 {
    crate::jp::res_p2_render::list_objects(m, list)
}

/// Append `(flags, sx, sy)` to the screen position list (count word, then
/// 8-byte records from +4).
fn push_screen_position(m: &mut Machine, positions: u32, flags: u32, sx: u32, sy: u32) {
    let count = m.lw(positions);
    let record = positions.wrapping_add(count << 3);
    m.sw(record + 4, flags);
    m.sh(record + 8, sx as u16);
    m.sh(record + 10, sy as u16);
    m.sw(positions, count.wrapping_add(1));
}

/// Projected size test shared by both paths of 80067494: `radius` scaled by
/// the projection distance over the view depth, clamped to `radius << 2`
/// when it exceeds `radius << 4`; false when the bounding square lies
/// outside the half-screen extents.
fn on_screen(depth: u32, radius: u32, lod: u32, sxy: u32, half_w: u32, half_h: u32) -> bool {
    let depth = if (depth as i32) > 0 { depth } else { 1 };
    let mut size = div_lo(mul_lo(radius, lod), depth);
    let sx = sra(sxy << 16, 16);
    let sy = sra(sxy, 16);
    if slt(radius << 4, size) {
        size = radius << 2;
    }
    !slt(half_w.wrapping_add(size), abs(sx)) && !slt(half_h.wrapping_add(size), abs(sy))
}

/// Loop state of 80067494 (its saved registers and stack slots).
struct DisplayCursors {
    frame: u32,
    models: u32,
    entry: u32,
    camera: u32,
    lod: u32,
    lod_range: u32,
    eye: u32,
    view: u32,
    light: u32,
    half_w: u32,
    half_h: u32,
    positions: u32,
    textured: u32,
    target: u32,
    aircraft: u32,
    plain: u32,
    parameters: u32,
}

/// 80067494: transform each listed entity into its display list. `models`
/// is the model table, `list` the 0xFF-terminated entity list, `textured`
/// and `target` the first two output lists; the 5th..7th arguments (the
/// aircraft list, the plain list and the screen position list) are the
/// caller's stack words +0x10..+0x18.
fn build_display_lists(m: &mut Machine, models: u32, list: u32, textured: u32, target: u32) {
    let frame = m.push_frame(0x98);
    // The original homes a0 in the caller's argument slot.
    m.sw(frame + 0x98, models);
    let aircraft = m.lw(frame + 0xa8);
    let plain = m.lw(frame + 0xac);
    let positions = m.lw(frame + 0xb0);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    let lod = lh(m, camera + 36);
    let half_w = sra(lhu(m, camera + 32) << 16, 17);
    let half_h = sra(lhu(m, camera + 34) << 16, 17);
    let mut c = DisplayCursors {
        frame,
        models,
        entry: list,
        camera,
        lod,
        lod_range: (lod << 2).wrapping_add(lod << 3),
        eye: camera + 4,
        view: gameplay + 76,
        light: gameplay + 108,
        half_w,
        half_h,
        positions,
        textured,
        target,
        aircraft,
        plain,
        parameters: 0,
    };
    if m.lbu(c.entry) != 255 {
        loop {
            build_display_entry(m, &mut c);
            c.entry += 16;
            if m.lbu(c.entry) == 255 {
                break;
            }
        }
    }
    m.sw(c.textured + 8, u32::MAX);
    m.sw(c.target, u32::MAX);
    m.sw(c.aircraft, u32::MAX);
    m.sw(c.plain + 16, 0);
    m.pop_frame(0x98);
}

/// One iteration of the 80067494 loop (80067560..800682D4).
fn build_display_entry(m: &mut Machine, c: &mut DisplayCursors) {
    let entry = c.entry;
    let frame = c.frame;
    let position = m.lw(entry + 8);
    let dx = m.lw(position).wrapping_sub(m.lw(c.eye));
    let dy = m.lw(position + 4).wrapping_sub(m.lw(c.eye + 4));
    let dz = m.lw(position + 8).wrapping_sub(m.lw(c.eye + 8));
    let mut x = sra(dx, 10);
    let mut y = sra(dy, 10);
    let mut z = sra(dz, 10);
    let distance = abs(x).wrapping_add(abs(y)).wrapping_add(abs(z));
    if slt(0x2_2018, distance) {
        return;
    }
    if m.lbu(entry + 4) & 0xf == 0 {
        build_plain_entry(m, c, dx, dy, dz);
        return;
    }
    let mut index = lbu(m, entry) << 2;
    c.parameters = m.lw(MODEL_PARAMETERS);
    if slt(c.lod_range << 2, distance) {
        index += 2;
    } else if slt(c.lod_range << 1, distance) {
        index += 1;
    }
    let slot = (index << 2).wrapping_add(m.lw(c.models + 12));
    let model = m.lw(slot);
    let level = if slt(c.lod_range, distance) {
        0
    } else if slt(sra(c.lod_range, 1), distance) {
        1
    } else {
        2
    };
    let mesh = m.lw(model + 4 * (level + 1));
    let shift = 2 - level;
    x = sra(x, shift);
    y = sra(y, shift);
    z = sra(z, shift);
    load_rotation(m, c.view);
    m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
    m.gte.mtc2(reg::VZ0, z);
    cop2(m, RTPS);
    if m.lbu(entry + 4) & 9 != 0 {
        let record = c.textured;
        let side = m.lbu(entry + 1);
        m.sb(record, side);
        let id = m.lhu(entry + 2);
        m.sh(record + 2, id);
        let table = m.lw(c.models + 12);
        m.sw(record + 8, level);
        m.sw(record + 16, model);
        m.sw(record + 12, mesh);
        m.sw(record + 4, table.wrapping_add(index << 2));
        let kind = lbu(m, entry + 6);
        let callback = m.lw(DRAW_CALLBACKS + (kind << 2));
        m.sw(record + 52, callback);
        store_mac123(m, record + 40);
    }
    // The lit record: the target list for kind 2, otherwise the aircraft
    // list (written for every other kind; only kind 4 advances it).
    let lit_target = m.lbu(entry + 4) & 2 != 0;
    let record = if lit_target { c.target } else { c.aircraft };
    if lit_target {
        m.sw(record + 152, c.parameters);
        let shade = lbu(m, entry + 7);
        let callback = m.lw(LIT_CALLBACKS + (shade << 2));
        m.sw(record + 156, callback);
    } else {
        m.sw(record + 152, 0);
        m.sw(record + 156, 0);
    }
    let table = m.lw(c.models + 12);
    let parts = m.lw(table + 12);
    let offset = 4 * (level + 1);
    for i in 0..2u32 {
        let part = m.lw(parts + i * 4);
        let value = m.lw(part + offset);
        m.sw(record + i * 4 + 4, value);
        m.sw(record + i * 4 + 24, part);
    }
    let value = m.lw(parts + 8);
    m.sw(record + 32, value);
    let value = m.lw(parts + 28);
    m.sw(record + 36, value);
    let value = m.lw(parts + 36);
    m.sw(record + 40, value);
    let a3 = if lit_target {
        for i in 0..10u32 {
            let part = m.lw(parts + i * 4 + 8);
            let value = m.lw(part + offset);
            m.sw(record + i * 4 + 44, value);
            match i {
                0 => m.sw(record + 12, value),
                5 => m.sw(record + 16, value),
                7 => m.sw(record + 20, value),
                _ => {}
            }
        }
        m.sw(record, level);
        7
    } else {
        let p0 = m.lw(parts + 8);
        let p5 = m.lw(parts + 28);
        let p7 = m.lw(parts + 36);
        let v0 = m.lw(p0 + offset);
        let v5 = m.lw(p5 + offset);
        let v7 = m.lw(p7 + offset);
        m.sw(record + 20, v7);
        m.sw(record + 12, v0);
        m.sw(record + 16, v5);
        m.sw(record, level);
        record + 8
    };
    store_mac123(m, record + 108);
    // 800679D0: depth and screen tests.
    let depth = sra(m.gte.mfc2(reg::MAC3), level);
    let radius = sra(lh(m, model + 2), 2 - level);
    if slt(depth, 0u32.wrapping_sub(radius)) {
        return;
    }
    let sxy = m.gte.mfc2(reg::SXY2);
    let kind = m.lbu(entry + 4);
    if kind & 2 != 0 {
        m.sw(c.target + 84, depth);
    } else if kind & 4 != 0 {
        m.sw(c.aircraft + 84, depth);
    }
    if !on_screen(depth, radius, c.lod, sxy, c.half_w, c.half_h) {
        return;
    }
    let position = m.lw(entry + 8);
    shared::f_80030534(m, [position + 16, frame + 16, radius, a3]);
    let flags = m.lw(entry + 12);
    if flags != 0 {
        push_screen_position(m, c.positions, flags, sra(sxy << 16, 16), sra(sxy, 16));
    }
    let kind = m.lbu(entry + 4);
    if kind & 9 != 0 {
        let record = c.textured;
        rotate_column(m, frame + 16, record + 20);
        rotate_column(m, frame + 18, record + 22);
        rotate_column(m, frame + 20, record + 24);
        c.textured = record + 56;
        return;
    }
    if kind & 2 != 0 {
        let record = c.target;
        if m.lbu(entry + 5) != 0 {
            locked_target_matrix(m, c, record, level);
        } else {
            rotate_column(m, frame + 16, record + 88);
            rotate_column(m, frame + 18, record + 90);
            rotate_column(m, frame + 20, record + 92);
        }
        load_rotation(m, c.light);
        rotate_column(m, frame + 16, record + 120);
        rotate_column(m, frame + 18, record + 122);
        rotate_column(m, frame + 20, record + 124);
        c.target = record + 160;
        return;
    }
    let record = c.aircraft;
    rotate_column(m, frame + 16, record + 88);
    rotate_column(m, frame + 18, record + 90);
    rotate_column(m, frame + 20, record + 92);
    load_rotation(m, c.light);
    rotate_column(m, frame + 16, record + 120);
    rotate_column(m, frame + 18, record + 122);
    rotate_column(m, frame + 20, record + 124);
    c.aircraft = record + 160;
}

/// 80067C28..80067D84: the locked target's matrix comes from the camera
/// angles minus the model parameter offsets (scaled down as the camera
/// rolls), through 800308F0; its translation is the negated camera offset
/// shifted by the level of detail.
fn locked_target_matrix(m: &mut Machine, c: &DisplayCursors, record: u32, level: u32) {
    let camera = c.camera;
    let frame = c.frame;
    let ax = lh(m, camera + 180);
    let ay = lh(m, camera + 182);
    let pitch = lhu(m, camera + 180);
    let mut roll = abs(ax).wrapping_add(abs(ay)) << 2;
    if !slt(roll, 4097) {
        roll = 4096;
    }
    let scale = 4096u32.wrapping_sub(roll);
    let parameters = c.parameters;
    let mut a0 = mul_lo(sra(lhu(m, parameters + 2) << 16, 20), scale);
    if slt(a0, 0) {
        a0 = a0.wrapping_add(4095);
    }
    let mut v1 = mul_lo(sra(lhu(m, parameters + 4) << 16, 22), scale);
    let a3 = sra(a0, 12);
    if slt(v1, 0) {
        v1 = v1.wrapping_add(4095);
    }
    let mut a2 = mul_lo(sra(lhu(m, parameters) << 16, 19), scale);
    let v1 = sra(v1, 12);
    if slt(a2, 0) {
        a2 = a2.wrapping_add(4095);
    }
    m.sh(frame + 48, pitch.wrapping_sub(a3) as u16);
    let yaw = lhu(m, camera + 182);
    m.sh(frame + 50, yaw.wrapping_sub(v1) as u16);
    let roll_angle = lhu(m, camera + 184);
    m.sh(frame + 52, roll_angle.wrapping_sub(sra(a2, 12)) as u16);
    shared::f_800306ac(m, [frame + 48, record + 88, a2, a3]);
    let shift = 6u32.wrapping_sub(level);
    for (i, offset) in [44u32, 46, 48].into_iter().enumerate() {
        let value = sra(0u32.wrapping_sub(lh(m, camera + offset)), shift);
        m.sw(record + 108 + i as u32 * 4, value);
    }
}

/// 80068114..800682D0: a plain object (kind 16) gets a 20-byte record with
/// its full-precision view position.
fn build_plain_entry(m: &mut Machine, c: &mut DisplayCursors, dx: u32, dy: u32, dz: u32) {
    let entry = c.entry;
    let x = sra(dx, 12);
    let y = sra(dy, 12);
    let z = sra(dz, 12);
    load_rotation(m, c.view);
    m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
    m.gte.mtc2(reg::VZ0, z);
    cop2(m, RTPS);
    let record = c.plain;
    let side = m.lbu(entry + 1);
    m.sb(record, side);
    let id = m.lhu(entry + 2);
    m.sh(record + 2, id);
    let index = lbu(m, entry);
    let slot = (index << 2).wrapping_add(m.lw(c.models + 20));
    let model = m.lw(slot);
    m.sw(record + 4, slot);
    m.sw(record + 16, model);
    let mx = m.gte.mfc2(reg::MAC1);
    let my = m.gte.mfc2(reg::MAC2);
    let mz = m.gte.mfc2(reg::MAC3);
    m.sh(record + 8, mx as u16);
    m.sh(record + 10, my as u16);
    m.sh(record + 12, mz as u16);
    let depth = m.gte.mfc2(reg::MAC3);
    let radius = lh(m, model + 2);
    if slt(depth, 0u32.wrapping_sub(radius)) {
        return;
    }
    let sxy = m.gte.mfc2(reg::SXY2);
    if !on_screen(depth, radius, c.lod, sxy, c.half_w, c.half_h) {
        return;
    }
    c.plain = record + 20;
    m.sw(c.frame + 0xac, record + 20);
    let flags = m.lw(entry + 12);
    if flags != 0 {
        push_screen_position(m, c.positions, flags, sra(sxy << 16, 16), sra(sxy, 16));
    }
}

// ---- mesh morphing (damage / animation blends) -------------------------------------

/// 80066A9C: blend two packed vertex lists (groups of three vertices: three
/// XY words, Z0|Z1, Z2) with INTPL at factor `blend` (IR0) into `out`
/// (count word first). Returns the end of the output.
fn blend_vertices(m: &mut Machine, blend: u32, from: u32, to: u32, out: u32) -> u32 {
    m.gte.mtc2(reg::IR0, blend);
    let mut src = from + 4;
    let mut dst_b = to + 4;
    let mut out_group = out.wrapping_sub(16);
    let mut b = to + 20;
    let mut count = m.lw(from);
    let mut a = from + 20;
    let mut o = out;
    m.sw(o, count);
    loop {
        let v0 = m.lw(src);
        let z0 = lh(m, a - 4);
        let z01 = m.lw(a - 4);
        m.gte.mtc2(reg::IR1, sra(v0 << 16, 16));
        m.gte.mtc2(reg::IR2, sra(v0, 16));
        m.gte.mtc2(reg::IR3, z0);
        let t0 = m.lw(dst_b);
        let t2 = lh(m, b - 4);
        let t1 = sra(t0, 16);
        let t0 = sra(t0 << 16, 16);
        let v0 = m.lw(b - 4);
        m.gte.ctc2(ctrl::RFC, t0);
        m.gte.ctc2(ctrl::GFC, t1);
        m.gte.ctc2(ctrl::BFC, t2);
        cop2(m, INTPL);
        o = o.wrapping_add(20);
        out_group = out_group.wrapping_add(20);
        let t0 = m.lw(b - 12);
        let t2 = sra(v0, 16);
        let t1 = sra(t0, 16);
        let t0 = sra(t0 << 16, 16);
        let x = m.gte.mfc2(reg::IR1);
        let y = m.gte.mfc2(reg::IR2);
        let z = m.gte.mfc2(reg::IR3);
        m.sh(out_group, x as u16);
        m.sh(o.wrapping_sub(14), y as u16);
        m.sh(o.wrapping_sub(4), z as u16);
        let v0 = m.lw(a - 12);
        let z1 = sra(z01, 16);
        m.gte.mtc2(reg::IR1, sra(v0 << 16, 16));
        m.gte.mtc2(reg::IR2, sra(v0, 16));
        m.gte.mtc2(reg::IR3, z1);
        m.gte.ctc2(ctrl::RFC, t0);
        m.gte.ctc2(ctrl::GFC, t1);
        m.gte.ctc2(ctrl::BFC, t2);
        cop2(m, INTPL);
        let t0 = m.lw(b - 8);
        let t2 = lh(m, b);
        let t1 = sra(t0, 16);
        let t0 = sra(t0 << 16, 16);
        let x = m.gte.mfc2(reg::IR1);
        let y = m.gte.mfc2(reg::IR2);
        let z = m.gte.mfc2(reg::IR3);
        m.sh(o.wrapping_sub(12), x as u16);
        m.sh(o.wrapping_sub(10), y as u16);
        m.sh(o.wrapping_sub(2), z as u16);
        let v0 = m.lw(a - 8);
        let z2 = lh(m, a);
        m.gte.mtc2(reg::IR1, sra(v0 << 16, 16));
        m.gte.mtc2(reg::IR2, sra(v0, 16));
        m.gte.mtc2(reg::IR3, z2);
        m.gte.ctc2(ctrl::RFC, t0);
        m.gte.ctc2(ctrl::GFC, t1);
        m.gte.ctc2(ctrl::BFC, t2);
        cop2(m, INTPL);
        a = a.wrapping_add(20);
        src = src.wrapping_add(20);
        b = b.wrapping_add(20);
        dst_b = dst_b.wrapping_add(20);
        count = count.wrapping_sub(3);
        let x = m.gte.mfc2(reg::IR1);
        let y = m.gte.mfc2(reg::IR2);
        let z = m.gte.mfc2(reg::IR3);
        m.sh(o.wrapping_sub(8), x as u16);
        m.sh(o.wrapping_sub(6), y as u16);
        m.sh(o, z as u16);
        if (count as i32) <= 0 {
            break;
        }
    }
    out_group.wrapping_add(20)
}

/// 80066C18: two-axis blend: `from_a`/`to_a` at `blend_a` into `temp`, then
/// `from_b`/`temp` at `blend_b` into `out`.
#[allow(clippy::too_many_arguments)]
fn blend_vertices_2d(
    m: &mut Machine,
    blend_a: u32,
    blend_b: u32,
    from_b: u32,
    from_a: u32,
    to_a: u32,
    out: u32,
    temp: u32,
) -> u32 {
    blend_vertices(m, blend_a, from_a, to_a, temp);
    blend_vertices(m, blend_b, from_b, temp, out)
}

/// 80066EF8: blend a mesh by two signed angles: the larger absolute angle
/// picks the blend ratio and the summed magnitude the strength; negative
/// angles select the opposite target meshes. `meshes` holds the base mesh
/// and the four targets (+x, -x, +y, -y).
fn blend_by_angles(m: &mut Machine, x: u32, y: u32, meshes: u32, out: u32, temp: u32) -> u32 {
    let base = m.lw(meshes);
    let (x, mesh_x) = if (x as i32) >= 0 {
        (x, m.lw(meshes + 4))
    } else {
        (0u32.wrapping_sub(x), m.lw(meshes + 8))
    };
    let (y, mesh_y) = if (y as i32) >= 0 {
        (y, m.lw(meshes + 12))
    } else {
        (0u32.wrapping_sub(y), m.lw(meshes + 16))
    };
    let ratio = if x == y {
        2048
    } else if slt(y, x) {
        div_lo(y << 11, x)
    } else {
        4096u32.wrapping_sub(div_lo(x << 11, y))
    };
    let sum = x.wrapping_add(y);
    let product = if slt(ratio, 2048) {
        mul_lo(sum, 4096u32.wrapping_sub(ratio))
    } else {
        mul_lo(sum, ratio)
    };
    let mut strength = sra(product, 12);
    if !slt(strength, 4097) {
        strength = 4096;
    }
    blend_vertices_2d(m, ratio, strength, base, mesh_x, mesh_y, out, temp)
}

/// 8006701C: blend `meshes[0]` toward `meshes[1]` by `blend`.
fn blend_pair(m: &mut Machine, blend: u32, meshes: u32, out: u32) -> u32 {
    let from = m.lw(meshes);
    let to = m.lw(meshes + 4);
    blend_vertices(m, blend, from, to, out)
}

/// 8006704C: blend `meshes[0]` toward `meshes[1]` (positive) or
/// `meshes[2]` (negative) by `|blend|`.
fn blend_signed(m: &mut Machine, blend: u32, meshes: u32, out: u32) -> u32 {
    let (blend, to) = if (blend as i32) >= 0 {
        (blend, m.lw(meshes + 4))
    } else {
        (0u32.wrapping_sub(blend), m.lw(meshes + 8))
    };
    let from = m.lw(meshes);
    blend_vertices(m, blend, from, to, out)
}

// ---- display list preparation ------------------------------------------------------

/// 80066C80: plain records whose side byte has bit 2 get their model
/// replaced: either a fixed damage variant (`id >> 12`, 0..4) or, when the
/// low 12 bits of the id are a blend factor, a copy of the variant model
/// with its vertices morphed toward the next variant.
fn prepare_plain_list(m: &mut Machine, list: u32, work: u32) {
    if m.lw(list + 16) == 0 {
        return;
    }
    let mut record = list;
    loop {
        if !lbu(m, record) & 4 == 0 {
            let id = lhu(m, record + 2);
            let mut level = sra(id << 16, 28);
            let blend = id & 0xfff;
            if (level as i32) < 0 {
                level = 0;
            }
            if !slt(level, 5) {
                level = 4;
            }
            if blend == 0 {
                let slot = m.lw(record + 4);
                let model = m.lw(slot + (level << 2));
                m.sw(record + 16, model);
            } else {
                let next = if slt(level, 4) { level + 1 } else { 4 };
                let copy = m.lw(work + 4);
                let slot = m.lw(record + 4);
                let model = m.lw(slot + (level << 2));
                let target = m.lw(slot + (next << 2));
                m.sw(work + 4, copy + 36);
                shared::copy_words(m, copy, model, 9);
                let cursor = m.lw(work + 4);
                m.sw(copy + 4, cursor);
                let from = m.lw(model + 4);
                let to = m.lw(target + 4);
                let cursor = m.lw(work + 4);
                let end = blend_vertices(m, blend, from, to, cursor);
                m.sw(work + 4, end);
                m.sw(record + 16, copy);
            }
        }
        record += 20;
        if m.lw(record + 16) == 0 {
            break;
        }
    }
}

/// 80066DCC: textured records whose side byte has bit 2 take the damage
/// variant `id >> 12` and, with a nonzero blend factor in the low 12 bits,
/// a morphed copy of the level-of-detail mesh in the primitive area.
fn prepare_textured_list(m: &mut Machine, list: u32, work: u32) {
    if (m.lw(list + 8) as i32) < 0 {
        return;
    }
    let mut record = list;
    loop {
        if !lbu(m, record) & 4 == 0 {
            let id = lhu(m, record + 2);
            let mut level = id >> 12;
            let blend = id & 0xfff;
            if !slt(level, 5) {
                level = 4;
            }
            let variants = m.lw(record + 4);
            let model = m.lw(variants + (level << 4));
            m.sw(record + 16, model);
            if blend != 0 {
                let next = if slt(level, 4) { level + 1 } else { 4 };
                let variants = m.lw(record + 4);
                let detail = m.lw(record + 8);
                let target = m.lw(variants + (next << 4));
                let offset = match detail {
                    0 => 4,
                    1 => 8,
                    _ => 12,
                };
                let from = m.lw(model + offset);
                let to = m.lw(target + offset);
                let cursor = m.lw(work + 4);
                m.sw(record + 12, cursor);
                let cursor = m.lw(work + 4);
                let end = blend_vertices(m, blend, from, to, cursor);
                m.sw(work + 4, end);
            }
        }
        record += 56;
        if (m.lw(record + 8) as i32) < 0 {
            break;
        }
    }
}

/// 80067088: lit target records morph their body, part 5 and part 7 meshes
/// by the model parameters' angles into the primitive area (`work`+16 is
/// the scratch mesh for the two-axis blend).
fn prepare_lit_list(m: &mut Machine, list: u32, work: u32) {
    if (m.lw(list) as i32) < 0 {
        return;
    }
    let mut record = list;
    loop {
        let cursor = m.lw(work + 4);
        let parameters = m.lw(record + 152);
        m.sw(record + 12, cursor);
        let x = lh(m, parameters);
        let y = lh(m, parameters + 2);
        let cursor = m.lw(work + 4);
        let end = blend_by_angles(m, x, y, record + 44, cursor, work + 16);
        m.sw(work + 4, end);
        m.sw(record + 16, end);
        let blend = lh(m, parameters + 8);
        let cursor = m.lw(work + 4);
        let end = blend_pair(m, blend, record + 64, cursor);
        m.sw(work + 4, end);
        m.sw(record + 20, end);
        let blend = lh(m, parameters + 4);
        let cursor = m.lw(work + 4);
        let end = blend_signed(m, blend, record + 72, cursor);
        record += 160;
        m.sw(work + 4, end);
        if (m.lw(record) as i32) < 0 {
            break;
        }
    }
}

// ---- display list drawing --------------------------------------------------------------

/// Work area layout in the scratchpad (`work` = 0x1F800000 for every caller):
/// +0 ordering table, +4 primitive cursor, +8 current face list, +12 depth
/// shift, +16 screen XY per vertex, +676 screen Z per vertex.
const WORK_SXY: u32 = 16;
const WORK_SZ: u32 = 676;

/// 800626E0: draw the plain records: project each model's vertices with
/// depth cueing (far colour 2096), then its depth-cued triangles (80061C3C)
/// and quads (800619E0).
fn draw_plain_list(m: &mut Machine, list: u32, work: u32) {
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0x830);
    }
    let mut model = m.lw(list + 16);
    if model == 0 {
        return;
    }
    let mut cursor = list + 16;
    loop {
        let x = lh(m, cursor - 8);
        let y = lh(m, cursor - 6);
        let z = lh(m, cursor - 4);
        m.gte.ctc2(ctrl::TRX, x);
        m.gte.ctc2(ctrl::TRY, y);
        m.gte.ctc2(ctrl::TRZ, z);
        let vertices = m.lw(model + 4);
        let mut out = work.wrapping_sub(16);
        let mut a0 = vertices + 4;
        let mut v1 = vertices + 20;
        let mut count = m.lw(vertices);
        let mut t1 = m.lw(vertices + 4);
        let mut t0 = m.lw(a0 + 4);
        let mut a3 = m.lw(a0 + 8);
        let mut v0 = m.lw(a0 + 12);
        let mut a2 = m.lw(a0 + 16);
        let mut a1 = v0 >> 16;
        loop {
            m.gte.mtc2(reg::VXY0, t1);
            m.gte.mtc2(reg::VXY1, t0);
            m.gte.mtc2(reg::VXY2, a3);
            m.gte.mtc2(reg::VZ0, v0);
            m.gte.mtc2(reg::VZ1, a1);
            m.gte.mtc2(reg::VZ2, a2);
            v1 = v1.wrapping_add(20);
            a0 = a0.wrapping_add(20);
            cop2(m, RTPT);
            count = count.wrapping_sub(3);
            out = out.wrapping_add(48);
            t1 = m.lw(a0);
            t0 = m.lw(v1.wrapping_sub(12));
            a3 = m.lw(v1.wrapping_sub(8));
            v0 = m.lw(v1.wrapping_sub(4));
            a2 = m.lw(v1);
            a1 = v0 >> 16;
            swc2(m, reg::SXY0, out);
            swc2(m, reg::SZ1, out + 4);
            swc2(m, reg::IR0, out + 8);
            swc2(m, reg::SXY1, out + 16);
            swc2(m, reg::SZ2, out + 20);
            swc2(m, reg::IR0, out + 24);
            swc2(m, reg::SXY2, out + 32);
            swc2(m, reg::SZ3, out + 36);
            swc2(m, reg::IR0, out + 40);
            if (count as i32) <= 0 {
                break;
            }
        }
        let vertices = m.lw(model + 4);
        m.sw(work + 560, vertices + 4);
        let triangles = m.lw(model + 16);
        cursor += 20;
        m.sw(work + 8, triangles);
        draw_gouraud_faces(m, work);
        let quads = m.lw(model + 12);
        m.sw(work + 8, quads);
        draw_flat_faces(m, work);
        model = m.lw(cursor);
        if model == 0 {
            break;
        }
    }
}

/// 80061C3C: the model's Gouraud face list (+16), drawn by the mesh
/// renderer shared with the terrain (translated there; called by address).
fn draw_gouraud_faces(m: &mut Machine, work: u32) {
    dispatch::call(m, 0x8006_1c3c, [work, 0, 0, 0]);
}

/// 800619E0: the model's flat face list (+12), drawn by the mesh renderer
/// shared with the terrain (translated there; called by address).
fn draw_flat_faces(m: &mut Machine, work: u32) {
    dispatch::call(m, 0x8006_19e0, [work, 0, 0, 0]);
}

/// Project a packed vertex list (count word, then groups of three vertices:
/// three XY words, Z0|Z1, Z2) into the work area's screen XY and Z arrays
/// (80062BD4's inline loop: the next group is loaded ahead, Z1 by `srl`).
fn project_vertices(m: &mut Machine, vertices: u32, work: u32) {
    let mut sxy = work + 4;
    let mut sz = work + 670;
    let mut a0 = vertices + 4;
    let mut v1 = vertices + 20;
    let mut count = m.lw(vertices);
    let mut t3 = m.lw(vertices + 4);
    let mut t2 = m.lw(a0 + 4);
    let mut t1 = m.lw(a0 + 8);
    let mut a1 = m.lw(a0 + 12);
    let mut t0 = m.lw(a0 + 16);
    let mut a3 = a1 >> 16;
    loop {
        m.gte.mtc2(reg::VXY0, t3);
        m.gte.mtc2(reg::VXY1, t2);
        m.gte.mtc2(reg::VXY2, t1);
        m.gte.mtc2(reg::VZ0, a1);
        m.gte.mtc2(reg::VZ1, a3);
        m.gte.mtc2(reg::VZ2, t0);
        v1 = v1.wrapping_add(20);
        a0 = a0.wrapping_add(20);
        cop2(m, RTPT);
        count = count.wrapping_sub(3);
        sxy = sxy.wrapping_add(12);
        sz = sz.wrapping_add(6);
        t3 = m.lw(a0);
        t2 = m.lw(v1.wrapping_sub(12));
        t1 = m.lw(v1.wrapping_sub(8));
        a1 = m.lw(v1.wrapping_sub(4));
        t0 = m.lw(v1);
        a3 = a1 >> 16;
        store_projection(m, sxy, sz);
        if (count as i32) <= 0 {
            break;
        }
    }
}

/// The same projection as 800637B0 inlines it: each group is loaded in the
/// iteration (`lwc2`), Z1 by `sra`.
fn project_vertices_lit(m: &mut Machine, vertices: u32, work: u32) {
    let mut sxy = work + 4;
    let mut sz = work + 670;
    let mut group = vertices + 4;
    let mut count = m.lw(vertices);
    loop {
        lwc2(m, reg::VXY0, group);
        lwc2(m, reg::VXY1, group + 4);
        lwc2(m, reg::VXY2, group + 8);
        let z01 = m.lw(group + 12);
        lwc2(m, reg::VZ2, group + 16);
        m.gte.mtc2(reg::VZ0, z01);
        m.gte.mtc2(reg::VZ1, sra(z01, 16));
        cop2(m, RTPT);
        group = group.wrapping_add(20);
        count = count.wrapping_sub(3);
        sxy = sxy.wrapping_add(12);
        sz = sz.wrapping_add(6);
        store_projection(m, sxy, sz);
        if (count as i32) <= 0 {
            break;
        }
    }
}

/// `swc2 SXY0..SXY2` to `sxy`, then SZ1..SZ3 (`mfc2` + `sh`) to `sz`.
fn store_projection(m: &mut Machine, sxy: u32, sz: u32) {
    swc2(m, reg::SXY0, sxy);
    swc2(m, reg::SXY1, sxy + 4);
    swc2(m, reg::SXY2, sxy + 8);
    for (i, register) in [reg::SZ1, reg::SZ2, reg::SZ3].into_iter().enumerate() {
        let z = m.gte.mfc2(register);
        m.sh(sz + i as u32 * 2, z as u16);
    }
}

/// 80062BD4: draw the textured records (56 bytes each, terminated by a
/// negative depth shift at +8): load the record's view matrix, run its
/// per-type callback, project the mesh, then draw its triangles (80062D68)
/// and quads (80062EC8).
fn draw_textured_list(m: &mut Machine, list: u32, work: u32) {
    let mut record = list;
    let mut shift = m.lw(record + 8);
    if (shift as i32) < 0 {
        return;
    }
    loop {
        m.sw(work + 12, shift);
        load_rotation(m, record + 20);
        load_translation(m, record + 40);
        let callback = m.lw(record + 52);
        if callback != 0 {
            // The callbacks (table 800B2C48) read only a0.
            dispatch::call(m, callback, [work, 0, 0, 0]);
        }
        let mesh = m.lw(record + 12);
        project_vertices(m, mesh, work);
        let model = m.lw(record + 16);
        let triangles = m.lw(model + 20);
        m.sw(work + 8, triangles);
        draw_textured_triangles(m, work);
        let model = m.lw(record + 16);
        let quads = m.lw(model + 24);
        record += 56;
        m.sw(work + 8, quads);
        draw_textured_quads(m, work);
        shift = m.lw(record + 8);
        if (shift as i32) < 0 {
            break;
        }
    }
}

/// 80062D68: back-face cull and depth-sort textured triangles (POLY_GT3,
/// 9 words, 40 bytes) from the face list at work+8.
fn draw_textured_triangles(m: &mut Machine, work: u32) {
    let sxy = work + WORK_SXY;
    let szs = work + WORK_SZ;
    let faces = m.lw(work + 8);
    let mut packet = m.lw(work + 4);
    let count = m.lw(faces);
    let ot = m.lw(work);
    if (count as i32) > 0 {
        let mut face = faces + 32;
        for _ in 0..count {
            let indices = m.lw(face - 24);
            let i0 = indices & 0xff;
            let i1 = (indices >> 8) & 0xff;
            let i2 = indices >> 16;
            lwc2(m, reg::SXY0, sxy + (i0 << 2));
            lwc2(m, reg::SXY1, sxy + (i1 << 2));
            lwc2(m, reg::SXY2, sxy + (i2 << 2));
            cop2(m, NCLIP);
            let area = m.gte.mfc2(reg::MAC0);
            if (area as i32) > 0 {
                let z0 = lh(m, szs + (i0 << 1));
                let z1 = lh(m, szs + (i1 << 1));
                let z2 = lh(m, szs + (i2 << 1));
                m.gte.mtc2(reg::SZ1, z0);
                m.gte.mtc2(reg::SZ2, z1);
                m.gte.mtc2(reg::SZ3, z2);
                cop2(m, AVSZ3);
                let average = m.gte.mfc2(reg::MAC0);
                let shift = m.lw(work + 12).wrapping_add(10);
                let depth = sra(average, shift);
                if depth.wrapping_sub(31) < 30681 {
                    let slot = ot.wrapping_add(sra(depth, 3) << 2);
                    add_prim(m, slot, packet, 0x0900_0000);
                    let a1 = m.lw(face - 20);
                    let a0 = m.lw(face - 8);
                    let v1 = m.lw(face - 16);
                    m.sw(packet + 4, a1);
                    swc2(m, reg::SXY0, packet + 8);
                    m.sw(packet + 12, a0);
                    m.sw(packet + 16, v1);
                    let a1 = m.lw(face - 4);
                    let a0 = m.lw(face - 12);
                    let v1 = m.lw(face);
                    swc2(m, reg::SXY1, packet + 20);
                    m.sw(packet + 24, a1);
                    m.sw(packet + 28, a0);
                    swc2(m, reg::SXY2, packet + 32);
                    m.sw(packet + 36, v1);
                    packet += 40;
                }
            }
            face += 32;
        }
    }
    m.sw(work + 4, packet);
}

/// 80062EC8: textured quads (POLY_GT4, 12 words, 52 bytes): visible when
/// either half passes NCLIP; depth is the average of the four Z values.
fn draw_textured_quads(m: &mut Machine, work: u32) {
    let sxy = work + WORK_SXY;
    let szs = work + WORK_SZ;
    let faces = m.lw(work + 8);
    let mut packet = m.lw(work + 4);
    let count = m.lw(faces);
    let ot = m.lw(work);
    if (count as i32) > 0 {
        let mut face = faces + 24;
        for _ in 0..count {
            let indices = m.lw(face - 16);
            let i0 = indices & 0xff;
            let i1 = (indices >> 8) & 0xff;
            let i2 = (indices >> 16) & 0xff;
            let i3 = indices >> 24;
            let xy0 = m.lw(sxy + (i0 << 2));
            lwc2(m, reg::SXY1, sxy + (i1 << 2));
            lwc2(m, reg::SXY2, sxy + (i2 << 2));
            m.gte.mtc2(reg::SXY0, xy0);
            let xy3 = m.lw(sxy + (i3 << 2));
            cop2(m, NCLIP);
            let area = m.gte.mfc2(reg::MAC0);
            m.gte.mtc2(reg::SXY0, xy3);
            let visible = (area as i32) > 0 || {
                cop2(m, NCLIP);
                (m.gte.mfc2(reg::MAC0) as i32) < 0
            };
            if visible {
                let sum = lh(m, szs + (i0 << 1))
                    .wrapping_add(lh(m, szs + (i1 << 1)))
                    .wrapping_add(lh(m, szs + (i2 << 1)))
                    .wrapping_add(lh(m, szs + (i3 << 1)));
                let shift = m.lw(work + 12);
                let depth = sra(sra(sum, 2), shift);
                if depth.wrapping_sub(31) < 30681 {
                    let slot = ot.wrapping_add(sra(depth, 3) << 2);
                    add_prim(m, slot, packet, 0x0c00_0000);
                    let v0 = m.lw(face - 12);
                    let a0 = m.lw(face + 4);
                    m.sw(packet + 8, xy0);
                    m.sw(packet + 4, v0);
                    m.sw(packet + 12, a0);
                    let a2 = m.lw(face - 8);
                    let v0 = m.lw(face + 8);
                    let a0 = m.lw(face - 4);
                    m.sw(packet + 16, a2);
                    swc2(m, reg::SXY1, packet + 20);
                    m.sw(packet + 24, v0);
                    m.sw(packet + 28, a0);
                    swc2(m, reg::SXY2, packet + 32);
                    let a2 = m.lw(face + 12);
                    let v0 = m.lw(face);
                    m.sw(packet + 36, a2);
                    m.sw(packet + 40, v0);
                    swc2(m, reg::SXY0, packet + 44);
                    m.sw(packet + 48, a2 >> 16);
                    packet += 52;
                }
            }
            face += 36;
        }
    }
    m.sw(work + 4, packet);
}

/// 8008D4E8: draw callback for model type 1: a POLY_GT3 sprite centred on
/// the projected point (0, 0, -30) of the current matrix, sized by the
/// projection distance over depth (at least 3).
fn draw_glow_sprite(m: &mut Machine, work: u32) {
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, (-30i32) as u32);
    let center = work + WORK_SXY;
    let packet = m.lw(work + 4);
    cop2(m, RTPS);
    m.sw(packet + 20, 0x0037_203f);
    m.sh(packet + 28, 0x3f20);
    m.sw(packet + 12, 0x4656_2020);
    m.sh(packet + 36, 0x3f3f);
    m.sw(packet + 4, 0x2e83_8383);
    swc2(m, reg::SXY2, center);
    let sz = m.gte.mfc2(reg::SZ3);
    let h = m.gte.cfc2(ctrl::H);
    let shift = m.lw(work + 12);
    let depth = sra(sz, shift);
    if depth.wrapping_sub(31) >= 30681 {
        return;
    }
    let mut size = div_lo(h << 3, depth);
    if slt(size, 3) {
        size = 3;
    }
    let offset = sra(depth, 3) << 2;
    let ot = m.lw(work);
    let tag = m.lw(offset.wrapping_add(ot)) | 0x0900_0000;
    m.sw(packet, tag);
    let ot = m.lw(work);
    m.sw(offset.wrapping_add(ot), link(packet));
    m.sw(work + 4, packet + 40);
    // Widescreen: the width squeezed like the projection (H-sized).
    let width = widescreen::squeeze_x(m, size);
    let half = sra(size, 1);
    let x = lhu(m, work + 16).wrapping_sub(sra(width, 1));
    m.sh(work + 16, x as u16);
    let y = lhu(m, center + 2).wrapping_sub(half);
    m.sh(center + 2, y as u16);
    let x = lhu(m, work + 16);
    m.sh(packet + 8, x as u16);
    let y = lhu(m, center + 2);
    m.sh(packet + 10, y as u16);
    let x = lhu(m, work + 16);
    m.sh(packet + 16, x.wrapping_add(width) as u16);
    let y = lhu(m, center + 2);
    m.sh(packet + 18, y as u16);
    let x = lhu(m, work + 16);
    m.sh(packet + 24, x as u16);
    let y = lhu(m, center + 2);
    m.sh(packet + 26, y.wrapping_add(size) as u16);
    let x = lhu(m, work + 16);
    m.sh(packet + 32, x.wrapping_add(width) as u16);
    let y = lhu(m, center + 2);
    m.sh(packet + 34, y.wrapping_add(size) as u16);
}

/// 800637B0: draw the lit records (160 bytes each, terminated by a
/// negative level word): each record gets its own 39-entry ordering table
/// (linked into the main table at its depth), its view and light matrices,
/// an optional per-shading callback, then five parts: the body lit against
/// the back colour (8006305C/800631F8) and four parts with their own colour
/// (800633D8/8006358C).
fn draw_lit_list(m: &mut Machine, list: u32, work: u32) {
    let mut record = list;
    if (m.lw(record) as i32) < 0 {
        return;
    }
    loop {
        let table = m.lw(work + 4);
        m.sw(work + 4, table + 156);
        sdk::clear_otag_r(m, table, 39);
        let depth = m.lw(record + 84);
        m.sh(work + 1014, depth as u16);
        if depth.wrapping_sub(31) < 30681 {
            let slot = m.lw(work).wrapping_add(sra(depth, 3) << 2);
            let head = m.lw(table);
            let link_word = m.lw(slot);
            m.sw(table, (head & 0xff00_0000) | (link_word & 0x00ff_ffff));
            let link_word = m.lw(slot);
            m.sw(
                slot,
                (link_word & 0xff00_0000) | ((table + 152) & 0x00ff_ffff),
            );
        }
        let level = m.lw(record);
        m.sw(work + 1016, table + 76);
        m.sw(work + 12, level);
        load_rotation(m, record + 88);
        load_translation(m, record + 108);
        load_light(m, record + 120);
        let parameters = m.lw(record + 152);
        m.sw(work + 1020, parameters);
        if parameters != 0 {
            let callback = m.lw(record + 156);
            if callback != 0 {
                // The callbacks (table 800B2C60) read only a0.
                dispatch::call(m, callback, [work, 0, 0, 0]);
            }
        }
        for part in 0..5u32 {
            let mesh = m.lw(record + 4 + part * 4);
            let model = m.lw(record + 24 + part * 4);
            project_vertices_lit(m, mesh, work);
            if part != 0 {
                let colour = m.lw(model + 16);
                m.sw(work + 1008, colour);
                let triangles = m.lw(model + 20);
                m.sw(work + 8, triangles);
                draw_coloured_triangles(m, work);
                let quads = m.lw(model + 24);
                m.sw(work + 8, quads);
                draw_coloured_quads(m, work);
            } else {
                let r = m.gte.cfc2(ctrl::RBK);
                let g = m.gte.cfc2(ctrl::RBK + 1);
                let b = m.gte.cfc2(ctrl::RBK + 2);
                m.gte.ctc2(ctrl::RFC, sra(r, 2));
                m.gte.ctc2(ctrl::GFC, sra(g, 2));
                m.gte.ctc2(ctrl::BFC, sra(b, 2));
                let parameters = m.lw(work + 1020);
                let mut fog = 3584u32;
                if parameters != 0 {
                    let raw = lhu(m, parameters + 6) << 16;
                    let angle = sra(raw, 16);
                    let quarter = sra(raw, 18);
                    fog = if slt(angle, 2048) {
                        4096u32.wrapping_sub(quarter)
                    } else {
                        let over = angle.wrapping_sub(2048);
                        3584u32.wrapping_sub((over << 1).wrapping_sub(sra(over, 2)))
                    };
                    if m.lhu(work + 1012) & 1 != 0 {
                        fog = fog.wrapping_add(8).wrapping_add(sra(fog, 5));
                    }
                }
                m.gte.mtc2(reg::IR0, fog);
                let triangles = m.lw(model + 20);
                m.sw(work + 8, triangles);
                draw_body_triangles(m, work);
                let quads = m.lw(model + 24);
                m.sw(work + 8, quads);
                draw_body_quads(m, work);
            }
        }
        record += 160;
        if (m.lw(record) as i32) < 0 {
            break;
        }
    }
}

// ==== 800333D8: tracer and spark pass ==================================================

/// `[gp+0x50C]`: effect block; +13264 holds four 32-bit masks of the live
/// segments and +5584 the 60-byte segment records (flags, timer, two
/// world positions).
const EFFECTS: u32 = 0x50c;
/// 800B09B4: identity view translation used by the effect pass.
const EFFECT_TRANSLATION: u32 = 0x800b_09b4;
/// DPCS sf=1.
const DPCS: u32 = 0x078_0010;
/// DPCT sf=1.
const DPCT: u32 = 0x0f8_002a;
/// GPL sf=1.
const GPL: u32 = 0x1a8_003e;

/// 800333D8: draw the effect segments with the view rotation and the fixed
/// translation 800B09B4; the new primitive cursor goes to gameplay +0x14.
pub fn draw_effects(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let gameplay = m.gw(GAMEPLAY);
    sdk::set_rot_matrix(m, gameplay + 0x4c);
    sdk::set_trans_matrix(m, EFFECT_TRANSLATION);
    let gameplay = m.gw(GAMEPLAY);
    let ot = m.lw(gameplay + 0x10) + 4;
    let primitives = m.lw(gameplay + 0x14);
    let next = effect_pass(m, ot, primitives);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
    m.pop_frame(0x18);
    next
}

/// 8006C5EC: sort the live effect segments into six screen lists in its
/// 0x3098-byte frame (8006C20C), then draw five of them; returns the new
/// primitive cursor. The sixth list (flag 0x100) is built but not drawn.
pub fn effect_pass(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    let frame = m.push_frame(0x3098);
    let lines = frame + 0x18;
    let thin = frame + 0x828;
    let medium = frame + 0x1038;
    let sparks = frame + 0x1848;
    let unused = frame + 0x2058;
    let wide = frame + 0x2868;
    m.sw(frame + 16, unused);
    m.sw(frame + 20, wide);
    sort_effect_segments(m, lines, thin, medium, sparks);
    let next = draw_effect_lines(m, lines, ot, primitives);
    let next = draw_thin_trails(m, thin, ot, next);
    let next = draw_medium_trails(m, medium, ot, next);
    let next = draw_sparks(m, sparks, ot, next);
    let next = draw_wide_trails(m, wide, ot, next);
    m.pop_frame(0x3098);
    next
}

/// 8006C20C: project both ends of every live effect segment within 0x7FFF
/// (in 4096-unit steps) of the camera and append a 16-byte record (end
/// SXY words, both SZ halfwords shifted down by the detail level, the
/// segment timer or 0 when either end is out of the depth range) to the
/// list selected by its flags (0x10, 0x20, 0x40, 0x80, 0x100, else the
/// sixth). The fifth and sixth lists are the caller's stack words +0x10 and
/// +0x14. Each list ends with a -1 timer.
fn sort_effect_segments(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) {
    let mut lists = [a0, a1, a2, a3, m.lw(m.sp + 16), m.lw(m.sp + 20)];
    let limit = 0x7fffu32;
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    let mut masks = m.gw(EFFECTS) + 13264;
    let eye_x = m.lw(camera + 4);
    let eye_y = m.lw(camera + 8);
    let eye_z = m.lw(camera + 12);
    let lod = lh(m, camera + 36);
    let range = (lod << 2).wrapping_add(lod << 3);
    let relative = |m: &mut Machine, point: u32| {
        let x = sra(m.lw(point).wrapping_sub(eye_x), 12);
        let y = sra(m.lw(point + 4).wrapping_sub(eye_y), 12);
        let z = sra(m.lw(point + 8).wrapping_sub(eye_z), 12);
        (x, y, z)
    };
    for base in (0..128u32).step_by(32) {
        let live = m.lw(masks);
        if live != 0 {
            for bit in 0..32u32 {
                if live & (1 << bit) == 0 {
                    continue;
                }
                let index = base + bit;
                let segment = m
                    .gw(EFFECTS)
                    .wrapping_add(((index << 4).wrapping_sub(index) << 2) + 5584);
                let (x0, y0, z0) = relative(m, segment + 8);
                let ax = abs(x0);
                if slt(limit, ax) {
                    continue;
                }
                let ay = abs(y0);
                if slt(limit, ay) {
                    continue;
                }
                let az = abs(z0);
                if slt(limit, az) {
                    continue;
                }
                let distance = ax.wrapping_add(ay).wrapping_add(az);
                let level = if slt(range, distance) {
                    0
                } else if slt(sra(range, 1), distance) {
                    1
                } else {
                    2
                };
                let (x0, y0, z0) = (sllv(x0, level), sllv(y0, level), sllv(z0, level));
                m.gte.mtc2(reg::VXY0, (x0 & 0xffff) | (y0 << 16));
                m.gte.mtc2(reg::VZ0, z0);
                let (x1, y1, z1) = relative(m, segment + 24);
                if slt(limit, abs(x1)) || slt(limit, abs(y1)) || slt(limit, abs(z1)) {
                    continue;
                }
                let (x1, y1, z1) = (sllv(x1, level), sllv(y1, level), sllv(z1, level));
                m.gte.mtc2(reg::VXY1, (x1 & 0xffff) | (y1 << 16));
                m.gte.mtc2(reg::VZ1, z1);
                cop2(m, RTPT);
                let flags = m.lw(segment);
                let list = if flags & 0x10 != 0 {
                    0
                } else if flags & 0x20 != 0 {
                    1
                } else if flags & 0x40 != 0 {
                    2
                } else if flags & 0x80 != 0 {
                    3
                } else if flags & 0x100 != 0 {
                    4
                } else {
                    5
                };
                let record = lists[list];
                lists[list] = record + 16;
                let mut timer = m.lw(segment + 4);
                let sz = m.gte.mfc2(reg::SZ1);
                if sz.wrapping_sub(1) >= 30711 {
                    timer = 0;
                }
                let sz = sra(sz, level);
                let sxy = m.gte.mfc2(reg::SXY0);
                m.sw(record, sxy);
                m.sh(record + 8, sz as u16);
                m.gte.mfc2(reg::SXY0);
                let sz = m.gte.mfc2(reg::SZ2);
                if sz.wrapping_sub(1) >= 30711 {
                    timer = 0;
                }
                let sz = sra(sz, level);
                let sxy = m.gte.mfc2(reg::SXY1);
                m.sw(record + 4, sxy);
                m.sh(record + 10, sz as u16);
                m.sw(record + 12, timer);
            }
        }
        masks += 4;
    }
    for list in lists {
        m.sw(list + 12, u32::MAX);
    }
}

/// Fade factor of a segment timer: `(limit - timer) << shift` while the
/// timer is below `limit`, else 0.
fn fade(timer: u32, limit: u32, shift: u32) -> u32 {
    if slt(timer, limit) {
        limit.wrapping_sub(timer) << shift
    } else {
        0
    }
}

/// Sign-extended low and high halves of a screen XY word.
fn screen_x(sxy: u32) -> u32 {
    sra(sxy << 16, 16)
}
fn screen_y(sxy: u32) -> u32 {
    sra(sxy, 16)
}

// JP-REVIEW(R0347): near function: PAL 8008F204 -> JP 80090A74, imm:4, 4 instructions differ (whole routine)
/// 80090A74: depth-cued gouraud lines (LINE_G2, code 0x52) for segments
/// beyond a quarter of the projection distance, culled when both ends are
/// off the same screen edge; returns the new primitive cursor.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_effect_lines` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_effect_lines(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_render::draw_effect_lines(m, list, ot, primitives)
}

/// Parameters of the three trail renderers 80090BF8, 80090ED0 and
/// 80091370, which differ only in these constants.
struct TrailStyle {
    /// Projected half-width numerator (a multiple of the projection
    /// distance).
    width: fn(u32) -> u32,
    fade_limit: u32,
    fade_shift: u32,
    /// Packet words +12 and +20 (texture page/CLUT halves and UVs) and
    /// halfwords +28 and +36.
    word12: u32,
    word20: u32,
    half28: u16,
    half36: u16,
}

// JP-REVIEW(R0349): near function: PAL 8008F388 -> JP 80090BF8, imm:4, 4 instructions differ (whole routine)
/// 80090BF8: thin trails (half-width 2x the projection distance over depth).
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_thin_trails` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_thin_trails(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_render::draw_thin_trails(m, list, ot, primitives)
}

// JP-REVIEW(R0350): near function: PAL 8008F660 -> JP 80090ED0, imm:4, 4 instructions differ (whole routine)
/// 80090ED0: medium trails (3x).
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_medium_trails` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_medium_trails(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_render::draw_medium_trails(m, list, ot, primitives)
}

// JP-REVIEW(R0351): near function: PAL 8008FB00 -> JP 80091370, imm:4, 4 instructions differ (whole routine)
/// 80091370: wide trails (6x), fading over 14 frames.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_wide_trails` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_wide_trails(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_render::draw_wide_trails(m, list, ot, primitives)
}

// JP-REVIEW(R0352): near function: PAL 8008F940 -> JP 800911B0, imm:1, 1 instructions differ (whole routine)
/// 800911B0: sparks: a depth-cued square sprite (POLY_GT3 corner layout) at
/// the far end of each segment within the screen, sized by 20x the
/// projection distance over depth (at least 2).
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_sparks` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_sparks(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_render::draw_sparks(m, list, ot, primitives)
}

/// 8008DE18: finish a trail quad: the screen direction between the two
/// ends (work +8 and +12) is normalised (VectorNormalSS) and each end is
/// offset by its half-width (work +16, +20) along the perpendicular with
/// GPL, giving the four corners at packet +8/+16/+24/+32; a zero width
/// collapses that end to its point.
fn build_trail_quad(m: &mut Machine, packet: u32, work: u32) {
    // Widescreen: the direction as on the 4:3 screen (the corners offsets
    // are squeezed again in trail_corner).
    let dx = lhu(m, work + 12).wrapping_sub(lhu(m, work + 8));
    let dx = widescreen::unsqueeze_x(m, sext16(dx));
    let dy = lhu(m, work + 14).wrapping_sub(lhu(m, work + 10));
    m.sh(work, dx as u16);
    m.sh(work + 2, dy as u16);
    let direction = m.lw(work);
    m.sh(work + 4, 0);
    if direction != 0 {
        sdk::vector_normal_ss(m, work, work);
    } else {
        m.sh(work, 4096);
    }
    let width = m.lw(work + 16);
    if width != 0 {
        m.gte.mtc2(reg::IR0, width);
        // Corners at the near end: (-nx - ny, nx - ny) and (-nx + ny, -nx - ny).
        let corners: [(Corner, u32); 2] = [
            (
                |nx, ny| (0u32.wrapping_sub(nx).wrapping_sub(ny), nx.wrapping_sub(ny)),
                8,
            ),
            (
                |nx, ny| {
                    let negative = 0u32.wrapping_sub(nx);
                    (negative.wrapping_add(ny), negative.wrapping_sub(ny))
                },
                16,
            ),
        ];
        for (offset, out) in corners {
            trail_corner(m, packet + out, work + 8, work, offset);
        }
    } else {
        let point = m.lw(work + 8);
        m.sw(packet + 16, point);
        m.sw(packet + 8, point);
    }
    let width = m.lw(work + 20);
    if width != 0 {
        m.gte.mtc2(reg::IR0, width);
        // Corners at the far end: (nx - ny, nx + ny) and (nx + ny, ny - nx).
        let corners: [(Corner, u32); 2] = [
            (|nx, ny| (nx.wrapping_sub(ny), nx.wrapping_add(ny)), 24),
            (|nx, ny| (nx.wrapping_add(ny), ny.wrapping_sub(nx)), 32),
        ];
        for (offset, out) in corners {
            trail_corner(m, packet + out, work + 12, work, offset);
        }
    } else {
        let point = m.lw(work + 12);
        m.sw(packet + 32, point);
        m.sw(packet + 24, point);
    }
}

/// Perpendicular offset of a trail corner from the unit direction (nx, ny).
type Corner = fn(u32, u32) -> (u32, u32);

/// One GPL corner of 8008DE18: MAC1/MAC2 = the end point (`lhu`), IR1/IR2 =
/// the perpendicular offset of the unit direction at work +0/+2, scaled by
/// IR0; the result halves go to `out`.
fn trail_corner(m: &mut Machine, out: u32, point: u32, work: u32, offset: Corner) {
    let x = lhu(m, point);
    m.gte.mtc2(reg::MAC1, x);
    let y = lhu(m, point + 2);
    m.gte.mtc2(reg::MAC2, y);
    let nx = lh(m, work);
    let ny = lh(m, work + 2);
    let (ir1, ir2) = offset(nx, ny);
    m.gte.mtc2(reg::IR1, ir1);
    m.gte.mtc2(reg::IR2, ir2);
    cop2(m, GPL);
    let corner = m.gte.mfc2(reg::MAC1);
    // Widescreen: the offset from the end squeezed like the projection.
    let corner = widescreen::squeeze_about(m, x, corner.wrapping_sub(x));
    m.sh(out, corner as u16);
    let y = m.gte.mfc2(reg::MAC2);
    m.sh(out + 2, y as u16);
}

// ==== 80033324: lit object systems ========================================================

/// `[gp+0x53C]`: trail block. +0 is the active mask of 32 smoke trails
/// (2312 bytes each from +4), +0x12124 the mask of 32 ribbon trails (1288
/// bytes each from +0x12128).
const TRAILS: u32 = 0x53c;

/// 80033324: load the view matrix, back colour and colour matrix, then draw
/// the trails (8007DB00) and the object systems (80085204), storing each
/// new primitive cursor in gameplay +0x14; returns the last one.
// JP-PATCH: replaced by `crate::jp::res_p1_render::draw_lit_systems` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_lit_systems(m: &mut Machine) -> u32 {
    crate::jp::res_p1_render::draw_lit_systems(m)
}

/// 8007DB00: smoke trails (8007C00C), then ribbon trails (8007CFA4).
pub fn draw_trail_systems(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    m.push_frame(0x18);
    let next = draw_smoke_trails(m, ot, primitives);
    let next = draw_ribbon_trails(m, ot, next);
    m.pop_frame(0x18);
    next
}

/// Screen work slot of 8007C00C (28 bytes from 0x1F800000): +0 point
/// record, +4 colour, +8 centre SXY, +12/+16/+20 the three ring SXY, +24
/// depth, +26 detail shift.
const SMOKE_SLOT: u32 = 28;

/// 8007C00C: draw the 32 smoke trails. Each live trail is a ring buffer of
/// 32 points (64 bytes: centre and three ring positions); points within
/// 0xB000 of the camera are projected and depth-cued (fog colour from the
/// mission colour block), then consecutive pairs become gouraud lines
/// (distant) or three POLY_G4 tube faces (near). Returns the new primitive
/// cursor.
fn draw_smoke_trails(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    let frame = m.push_frame(0x58);
    m.sw(frame + 88, ot);
    let mut packet = primitives;
    let block = m.gw(TRAILS);
    let active = m.lw(block);
    let mut trail = block + 4;
    if active != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay + 40);
        let half_w = sra(lhu(m, camera + 32) << 16, 17);
        let half_h = sra(lhu(m, camera + 34) << 16, 17);
        let view = SmokeView {
            limit_x: half_w.wrapping_add(56),
            limit_y: half_h.wrapping_add(56),
            eye: [m.lw(camera + 4), m.lw(camera + 8), m.lw(camera + 12)],
            far: 0,
            near: 0,
            ot,
        };
        let h = m.gte.cfc2(ctrl::H) << 2;
        let view = SmokeView {
            far: h.wrapping_add(h << 1),
            near: h << 2,
            ..view
        };
        m.gte.mtc2(reg::RGBC, 0);
        for index in 0..32u32 {
            let gameplay = m.gw(GAMEPLAY);
            let colours = m.lw(gameplay + 616) + 4;
            let r = lbu(m, colours + 28) << 4;
            let g = lbu(m, colours + 29) << 4;
            let b = lbu(m, colours + 30) << 4;
            m.gte.ctc2(ctrl::RFC, r);
            m.gte.ctc2(ctrl::GFC, g);
            m.gte.ctc2(ctrl::BFC, b);
            if active & (1 << index) != 0 {
                packet = draw_smoke_trail(m, &view, trail, packet);
            }
            trail += 2312;
        }
    }
    m.pop_frame(0x58);
    packet
}

/// Camera-dependent constants of 8007C00C.
#[derive(Clone, Copy)]
struct SmokeView {
    limit_x: u32,
    limit_y: u32,
    eye: [u32; 3],
    /// 12 x H: beyond it points are projected at a quarter of their size.
    far: u32,
    /// 16 x H: nearer points are drawn as tube faces.
    near: u32,
    ot: u32,
}

/// One trail of 8007C00C.
fn draw_smoke_trail(m: &mut Machine, view: &SmokeView, trail: u32, primitives: u32) -> u32 {
    let mut packet = primitives;
    let mut count = 0u32;
    let mut overflow = 0u32;
    let mut near = 0u32;
    let mut last = 0u32;
    let mut visible = 0u32;
    let [eye_x, eye_y, eye_z] = view.eye;
    // Pass 1: project the centre of each point.
    let mut point_index = m.lw(trail + 4);
    let mut slot = SCRATCH;
    if slt(point_index, m.lw(trail)) {
        loop {
            let ring = point_index & 0x1f;
            let point = trail + (ring << 6) + 264;
            let x = sra(m.lw(point).wrapping_sub(eye_x), 10);
            let y = sra(m.lw(point + 4).wrapping_sub(eye_y), 10);
            let z = sra(m.lw(point + 8).wrapping_sub(eye_z), 10);
            let distance = sra(abs(x).wrapping_add(abs(y)).wrapping_add(abs(z)), 2);
            if !slt(0xb000, distance) {
                let shift = if slt(view.far, distance) {
                    2
                } else {
                    u32::from(slt(sra(view.far, 1), distance))
                };
                let (x, y, z) = (sra(x, shift), sra(y, shift), sra(z, shift));
                m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
                m.gte.mtc2(reg::VZ0, z);
                cop2(m, RTPS);
                m.sh(slot + 26, shift as u16);
                m.sw(slot, point);
                let mut bit = 1u32 << (count & 31);
                if slt(distance, view.near) {
                    near |= bit;
                }
                if point_index == m.lw(trail).wrapping_sub(1) {
                    last = bit;
                }
                let age = m.lw(trail + (ring << 2) + 8);
                let life = m.lw(trail + (ring << 2) + 136);
                let fog = if slt(age << 4, life) {
                    (age << 2) << 5
                } else {
                    life << 5
                };
                let depth = m.gte.mfc2(reg::IR3);
                m.gte.mtc2(reg::IR0, fog);
                let flags = m.gte.cfc2(ctrl::FLAG);
                cop2(m, DPCS);
                if flags & 0x2_0000 != 0 {
                    overflow |= bit;
                    bit = 0;
                }
                let sxy = m.gte.mfc2(reg::SXY2);
                let sx = screen_x(sxy);
                let depth = sra(depth, 2u32.wrapping_sub(shift));
                if slt(view.limit_x, abs(sx)) || slt(view.limit_y, abs(screen_y(sxy))) {
                    bit = 0;
                }
                visible |= bit;
                swc2(m, reg::SXY2, slot + 8);
                m.sh(slot + 24, depth as u16);
                let colour = m.gte.mfc2(reg::RGB2);
                count += 1;
                m.sw(slot + 4, colour);
                slot += SMOKE_SLOT;
            }
            point_index = point_index.wrapping_add(1);
            if !slt(point_index, m.lw(trail)) {
                break;
            }
        }
    }
    // Neighbours of near/visible points take part as well.
    let hidden = !visible;
    near |= (sra(near, 1) | (near << 1)) & hidden;
    visible |= (sra(visible, 1) | (visible << 1)) & hidden;
    // Pass 2: project the three ring positions of each visible point.
    if (count as i32) > 0 {
        let mut slot = SCRATCH;
        for index in 0..count {
            let bit = 1u32 << (index & 31);
            let depth = lh(m, slot + 24);
            if !visible & bit == 0 {
                if !near & bit != 0 {
                    if (depth as i32) < 0 {
                        m.sh(slot + 24, 0);
                    }
                } else {
                    project_smoke_ring(m, view, slot, index, count, overflow & bit != 0, depth);
                    if (depth as i32) < 0 {
                        m.sh(slot + 24, 0);
                    }
                    if last & (1 << (index & 31)) != 0 {
                        let centre = m.lw(slot + 8);
                        m.sw(slot + 12, centre);
                        m.sw(slot + 16, centre);
                        m.sw(slot + 20, centre);
                        m.gte.mfc2(reg::MAC3);
                    } else {
                        swc2(m, reg::SXY0, slot + 12);
                        swc2(m, reg::SXY1, slot + 16);
                        swc2(m, reg::SXY2, slot + 20);
                    }
                }
            }
            slot += SMOKE_SLOT;
        }
    }
    // Pass 3: emit primitives for consecutive visible pairs.
    let mut previous = SmokePoint::load(m, SCRATCH);
    let mut previous_depth = lh(m, SCRATCH + 24);
    if slt(1, count) {
        let hidden = !visible;
        let mut slot = SCRATCH + SMOKE_SLOT;
        for index in 1..count {
            let pair = 3u32 << ((index - 1) & 31);
            let current = SmokePoint::load(m, slot);
            let depth = lh(m, slot + 24);
            if hidden & pair == 0 && slt(previous_depth, 22528) {
                let ot_slot = view.ot.wrapping_add(sra(previous_depth, 3) << 2);
                if !near & pair != 0 {
                    let tag = m.lw(ot_slot) | 0x0400_0000;
                    m.sw(packet + 4, previous.colour | 0x5200_0000);
                    m.sw(packet + 8, previous.centre);
                    m.sw(packet + 12, current.colour);
                    m.sw(packet + 16, current.centre);
                    m.sw(packet, tag);
                    m.sw(ot_slot, link(packet));
                    packet += 20;
                } else {
                    let code = previous.colour | 0x3a00_0000;
                    let faces = [
                        (previous.ring[0], current.ring[0]),
                        (previous.ring[1], current.ring[1]),
                    ];
                    for (face, (from, to)) in faces.into_iter().enumerate() {
                        let base = packet + face as u32 * 36;
                        m.sw(base + 4, code);
                        m.sw(base + 8, previous.centre);
                        m.sw(base + 12, 0);
                        m.sw(base + 16, from);
                        m.sw(base + 20, current.colour);
                        m.sw(base + 24, current.centre);
                        m.sw(base + 28, 0);
                        m.sw(base + 32, to);
                        let tag = m.lw(ot_slot) | 0x0800_0000;
                        m.sw(base, tag);
                        m.sw(ot_slot, link(base));
                    }
                    let base = packet + 72;
                    m.sw(base + 4, code);
                    m.sw(base + 8, previous.centre);
                    m.sw(base + 12, 0);
                    m.sw(base + 16, previous.ring[2]);
                    m.sw(base + 20, current.colour);
                    m.sw(base + 24, current.centre);
                    m.sw(base + 28, 0);
                    m.sw(base + 32, current.ring[2]);
                    let tag = m.lw(ot_slot) | 0x0800_0000;
                    packet += 108;
                    m.sw(base, tag);
                    m.sw(ot_slot, link(base));
                }
            }
            previous = current;
            previous_depth = depth;
            slot += SMOKE_SLOT;
        }
    }
    packet
}

/// The values pass 3 of 8007C00C keeps per point.
#[derive(Clone, Copy)]
struct SmokePoint {
    colour: u32,
    centre: u32,
    ring: [u32; 3],
}
impl SmokePoint {
    fn load(m: &mut Machine, slot: u32) -> Self {
        Self {
            centre: m.lw(slot + 8),
            ring: [m.lw(slot + 12), m.lw(slot + 16), m.lw(slot + 20)],
            colour: m.lw(slot + 4),
        }
    }
}

/// Relative position of a point record word triple, shifted by `shift`.
fn relative_shifted(m: &mut Machine, point: u32, eye: [u32; 3], shift: u32) -> (u32, u32, u32) {
    let x = sra(m.lw(point).wrapping_sub(eye[0]), shift);
    let y = sra(m.lw(point + 4).wrapping_sub(eye[1]), shift);
    let z = sra(m.lw(point + 8).wrapping_sub(eye[2]), shift);
    (x, y, z)
}

/// Pass 2 of 8007C00C for one near point: project its three ring positions
/// (VXY0..2). When the centre's projection overflowed (behind the near
/// plane), every position is first pulled toward the neighbouring point by
/// INTPL at the near-plane crossing ratio, the centre is reprojected, and
/// the point's colour is cleared.
fn project_smoke_ring(
    m: &mut Machine,
    view: &SmokeView,
    slot: u32,
    index: u32,
    count: u32,
    overflowed: bool,
    depth: u32,
) {
    let shift = lh(m, slot + 26).wrapping_add(10);
    let point = m.lw(slot);
    let eye = view.eye;
    let pack = |x: u32, y: u32| (x & 0xffff) | (y << 16);
    let last = if overflowed {
        let neighbour_slot = if slt(index, count.wrapping_sub(1)) {
            slot + SMOKE_SLOT
        } else {
            slot - SMOKE_SLOT
        };
        let h = m.gte.cfc2(ctrl::H);
        let numerator = depth.wrapping_sub(sra(h, 3)) << 12;
        let neighbour_depth = lh(m, neighbour_slot + 24);
        let ratio = div_lo(numerator, depth.wrapping_sub(neighbour_depth));
        m.gte.mtc2(reg::IR0, ratio);
        let neighbour = m.lw(neighbour_slot);
        let mut results = [(0u32, 0u32, 0u32); 4];
        for (i, offset) in [0u32, 16, 32, 48].into_iter().enumerate() {
            let (x, y, z) = relative_shifted(m, point + offset, eye, shift);
            let (fx, fy, fz) = relative_shifted(m, neighbour + offset, eye, shift);
            if i == 1 {
                m.gte.mtc2(reg::IR0, ratio);
            }
            m.gte.mtc2(reg::IR1, x);
            m.gte.mtc2(reg::IR2, y);
            m.gte.mtc2(reg::IR3, z);
            m.gte.ctc2(ctrl::RFC, fx);
            m.gte.ctc2(ctrl::GFC, fy);
            m.gte.ctc2(ctrl::BFC, fz);
            cop2(m, INTPL);
            let x = m.gte.mfc2(reg::IR1);
            let y = m.gte.mfc2(reg::IR2);
            let z = m.gte.mfc2(reg::IR3);
            results[i] = (x, y, z);
            match i {
                0 => {
                    m.gte.mtc2(reg::VXY0, pack(x, y));
                    m.gte.mtc2(reg::VZ0, z);
                    cop2(m, RTPS);
                    swc2(m, reg::SXY2, slot + 8);
                }
                1 => {
                    m.gte.mtc2(reg::VXY0, pack(x, y));
                    m.gte.mtc2(reg::VZ0, z);
                }
                2 => {
                    m.gte.mtc2(reg::VXY1, pack(x, y));
                    m.gte.mtc2(reg::VZ1, z);
                }
                _ => {}
            }
        }
        m.sw(slot + 4, 0);
        results[3]
    } else {
        let (x, y, z) = relative_shifted(m, point + 16, eye, shift);
        m.gte.mtc2(reg::VXY0, pack(x, y));
        m.gte.mtc2(reg::VZ0, z);
        let (x, y, z) = relative_shifted(m, point + 32, eye, shift);
        m.gte.mtc2(reg::VXY1, pack(x, y));
        m.gte.mtc2(reg::VZ1, z);
        relative_shifted(m, point + 48, eye, shift)
    };
    let (x, y, z) = last;
    m.gte.mtc2(reg::VXY2, pack(x, y));
    m.gte.mtc2(reg::VZ2, z);
    cop2(m, RTPT);
}

// JP-REVIEW(R0353): near function: PAL 8007B928 -> JP 8007CFA4, imm:8, 8 instructions differ (whole routine)
/// 8007CFA4: draw the 32 ribbon trails (two edge positions per point,
/// 32-byte records). The midpoint decides the detail level and the grey
/// level (halved with distance); consecutive visible pairs become two
/// gouraud lines (distant) or four POLY_G4 strips with feathered edges
/// (INTPL extrapolation by -96/96/4000/4192) (near). Returns the new
/// primitive cursor.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_ribbon_trails` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_ribbon_trails(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    crate::jp::res_p2_render::draw_ribbon_trails(m, ot, primitives)
}

/// Camera-dependent constants of 8007CFA4.
#[derive(Clone, Copy)]
struct RibbonView {
    eye: [u32; 3],
    half_w: u32,
    half_h: u32,
    /// 12 x the projection distance: detail levels.
    range: u32,
    /// 8 x the projection distance: nearer pairs are strips.
    near: u32,
    ot: u32,
}

/// The values pass 3 of 8007CFA4 keeps per point (edge coordinates
/// sign-extended from their halfwords).
#[derive(Clone, Copy)]
struct RibbonPoint {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    depth: u32,
    colour: u32,
}
impl RibbonPoint {
    fn load(m: &mut Machine, slot: u32) -> Self {
        Self {
            depth: m.lw(slot + 12),
            colour: m.lw(slot + 16),
            x0: lh(m, slot + 4),
            y0: lh(m, slot + 6),
            x1: lh(m, slot + 8),
            y1: lh(m, slot + 10),
        }
    }
}

// ---- object systems (80085204) ------------------------------------------------------------

/// `[gp+0x41C]`: object system block (+1480 sun/flare state, +1536 the
/// three 896-byte object lists, +4248 ten projectile lists, +13248 the
/// debris list, +14664 the lock-on marker, +14692 the explosion list); the
/// block itself starts with the 16 ambient particle positions.
const OBJECT_SYSTEMS: u32 = 0x41c;
/// `[gp+0x33C]`: sun state (+4 negated sun height).
const SUN_STATE: u32 = 0x33c;
/// Model renderers for the ambient layer type byte (1..5).
const AMBIENT_RENDERERS: u32 = 0x800b_4388;
/// Debris renderers by debris type (8008B048).
const DEBRIS_RENDERERS: u32 = 0x800b_4400;
/// Scratch work area of the object systems (0x1F80001C).
const WORK: u32 = SCRATCH + 0x1c;

/// MIPS `lwl` (little-endian): merge the bytes up to `address` into the
/// high end of `rt`.
fn lwl(m: &mut Machine, rt: u32, address: u32) -> u32 {
    let word = m.lw(address & !3);
    match address & 3 {
        0 => (rt & 0x00ff_ffff) | (word << 24),
        1 => (rt & 0x0000_ffff) | (word << 16),
        2 => (rt & 0x0000_00ff) | (word << 8),
        _ => word,
    }
}

/// MIPS `swl` (little-endian): store the high end of `rt` into the bytes up
/// to `address`.
fn swl(m: &mut Machine, rt: u32, address: u32) {
    let aligned = address & !3;
    let word = m.lw(aligned);
    let merged = match address & 3 {
        0 => (word & 0xffff_ff00) | (rt >> 24),
        1 => (word & 0xffff_0000) | (rt >> 16),
        2 => (word & 0xff00_0000) | (rt >> 8),
        _ => rt,
    };
    m.sw(aligned, merged);
}

/// The inline catPrim of these renderers: link the packet chain ending at
/// `last` (whose tag already links the earlier packets) into the ordering
/// table word `slot`, then make the slot point at `first`. Written with
/// `lwl`/`swl` on the 24-bit link fields.
fn insert_chain(m: &mut Machine, slot: u32, first: u32, last: u32) {
    let link_word = lwl(m, 0, slot + 2);
    swl(m, link_word, last + 2);
    swl(m, first << 8, slot + 2);
}

/// Save the GTE rotation matrix to `out` (`cfc2` R11R12..R33).
fn save_rotation(m: &mut Machine, out: u32) {
    let t4 = m.gte.cfc2(ctrl::R11R12);
    let t5 = m.gte.cfc2(ctrl::R13R21);
    m.sw(out, t4);
    m.sw(out + 4, t5);
    let t4 = m.gte.cfc2(ctrl::R22R23);
    let t5 = m.gte.cfc2(ctrl::R31R32);
    let t6 = m.gte.cfc2(ctrl::R33);
    m.sw(out + 8, t4);
    m.sw(out + 12, t5);
    m.sw(out + 16, t6);
}

// JP-REVIEW(R0359): changed function: PAL 80083B78 -> JP 80085204, call+:1 call~:1 del:11 imm:5 ins:68 op:24 ref:5 reg:12, 126 instructions differ (whole routine)
/// 80085204: draw the object systems: scratch work area (OT, primitive
/// cursor, eye position, camera angles, projection distance), the sun and
/// moon, the cloud layers, the ambient particle renderer of the mission,
/// objects, projectiles, debris, the lock-on marker and explosions, and
/// the 8008FFAC overlay when the camera flags ask for it. Returns the new
/// primitive cursor.
pub fn draw_object_systems(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    m.push_frame(0x28);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    m.sw(SCRATCH, ot);
    m.sw(SCRATCH + 4, primitives);
    let x = m.lw(camera + 4);
    let y = m.lw(camera + 8);
    let z = m.lw(camera + 12);
    m.sw(SCRATCH + 8, x);
    m.sw(SCRATCH + 12, y);
    m.sw(SCRATCH + 16, z);
    let pitch = m.lhu(camera + 24);
    let angles = m.lw(camera + 20);
    let lod = m.lhu(camera + 36);
    m.sh(SCRATCH + 24, pitch);
    m.sh(SCRATCH + 26, lod);
    let mode = m.lw(camera + 80);
    m.sw(SCRATCH + 20, angles);
    let context = m.gw(crate::common::CONTEXT);
    let systems = m.gw(OBJECT_SYSTEMS);
    if mode & 0x870 != 0 || m.lw(context) & 0x2000_0000 != 0 {
        let flags = m.lw(systems + 1480);
        m.sw(systems + 1480, flags | 1);
    }
    let sky = m.lw(gameplay + 580);
    let sun = lh(m, sky + 108);
    if sun != 0 {
        let state = m.gw(SUN_STATE);
        let height = m.lw(state + 4);
        draw_sun(m, systems + 1480, sun, 0u32.wrapping_sub(height));
        let moon = m.lw(sky + 112);
        if moon != 0 {
            if m.lh(context + 914) == 25 && m.lw(camera + 80) == 2 {
                let flags = m.lw(systems + 1480);
                m.sw(systems + 1480, flags | 4);
            } else {
                draw_moon(m, systems + 1480, moon);
            }
        }
    }
    if m.lbu(sky + 110) != 0 {
        draw_cloud_layers(m);
    }
    let ambient = lbu(m, sky + 111);
    if (ambient as i32) > 0 && slt(ambient, 6) {
        let renderer = m.lw(AMBIENT_RENDERERS + ((ambient - 1) << 2));
        // The renderers read only globals; a1 is the type byte.
        dispatch::call(m, renderer, [0, ambient, 0, 0]);
    }
    draw_objects(m, systems + 1536);
    draw_projectiles(m, systems + 4248);
    draw_debris(m, systems + 13248);
    draw_lock_marker(m, systems + 14664);
    draw_explosions(m, systems + 14692);
    if m.lw(camera + 80) & 0x200 != 0 {
        draw_overlays(m, SCRATCH);
    }
    let next = m.lw(SCRATCH + 4);
    m.pop_frame(0x28);
    next
}

// JP-REVIEW(R0360): near function: PAL 8008C7C8 -> JP 8008E038, imm:1, 1 instructions differ (whole routine)
/// 8008E038: ambient particles (renderer type 1): the 16 positions of the
/// object system block wrap around the camera in a 0x4000 cube; each is a
/// streak stretched by the camera velocity (8008DE18), fading in with
/// speed. The projection distance is narrowed with speed while drawing.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_ambient_particles` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_ambient_particles(m: &mut Machine) {
    crate::jp::res_p2_render::draw_ambient_particles(m)
}

/// PAL 8008C1C8: two cloud layers: each layer's height (relative to the eye,
/// halved per layer), count and drift step come from the mission colour
/// block (gameplay +616); clouds are POLY_FT4 sprites around the camera,
/// depth-cued by height, fading toward the near plane (AVSZ4 OTZ 300..556).
#[allow(unused_variables)]
fn draw_cloud_layers(m: &mut Machine) {
    crate::jp_review::pal_only("draw_cloud_layers: PAL 8008C1C8")
}

// JP-REVIEW(R0362): near function: PAL 80083D48 -> JP 800854B8, imm:1, 1 instructions differ (whole routine)
/// 800854B8: the three object lists (bit masks of 28-byte objects, 32 per
/// 896-byte list) are projected; visible ones (within 0x8000 grid units in
/// each axis, nearer than the near plane in projected size) get a screen
/// record at work +100 (SXY, OT depth, size) and a bit in work +40; then
/// each visible object is drawn by its class renderer (table 800B439C)
/// with the view rotation saved at work +56.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_objects` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_objects(m: &mut Machine, lists: u32) {
    crate::jp::res_p2_render::draw_objects(m, lists)
}

// JP-REVIEW(R0363): near function: PAL 80084BB0 -> JP 80086320, imm:1, 1 instructions differ (whole routine)
/// 80086320 and 80086A0C run on the scratchpad stack set up by 8008700C and
/// spill the caller's s0..s5 and `ra` at `sp - 0x20..sp - 0x8`. The
/// scratchpad is compared, so write the same words: here s0 = projectile,
/// s1 = remaining mask, s2 = WORK, s3 = mask word address, s4 = list
/// offset, s5 = group index, `ra` = the return address of the call.
fn spill_registers(m: &mut Machine, saved: [u32; 6], ra: u32) {
    let frame = m.sp.wrapping_sub(0x30);
    for (slot, value) in (0u32..).zip(saved) {
        m.sw(frame + 0x10 + slot * 4, value);
    }
    m.sw(frame + 0x28, ra);
}

/// 8008700C: ten projectile lists (bit masks of 28-byte projectiles, 32 per
/// 896-byte list): type 0 by 80086320, others by 80086A0C, run on a
/// scratchpad stack (the original moves `$sp` to 0x1F8003F8 and keeps the
/// old one at 0x1F8003FC). The view rotation is saved at work +0 and
/// restored with a zero translation afterwards.
fn draw_projectiles(m: &mut Machine, lists: u32) {
    m.push_frame(0x38);
    save_rotation(m, WORK);
    let saved = m.sp;
    m.sw(SCRATCH + 0x3fc, saved);
    m.sp = SCRATCH + 0x3f8;
    let mut base = 40u32;
    let mut mask_word = lists;
    for group in 0..10u32 {
        let mut mask = m.lw(mask_word);
        let mut projectile = lists + base;
        while mask != 0 {
            if mask & 1 != 0 {
                let saved = [projectile, mask, WORK, mask_word, base, group];
                if m.lh(projectile + 24) != 0 {
                    spill_registers(m, saved, 0x8008_70dc);
                    draw_exhaust(m, projectile, SCRATCH, WORK);
                } else {
                    spill_registers(m, saved, 0x8008_70c8);
                    draw_tracer(m, projectile, SCRATCH, WORK);
                }
            }
            projectile += 28;
            mask >>= 1;
        }
        base += 896;
        mask_word += 4;
    }
    m.sp = m.lw(m.sp + 4);
    load_rotation(m, WORK);
    m.gte.ctc2(ctrl::TRX, 0);
    m.gte.ctc2(ctrl::TRY, 0);
    m.gte.ctc2(ctrl::TRZ, 0);
    m.pop_frame(0x38);
}

/// 8008B048: debris: copy 32 halfwords of the 800B7104 table (stride 512)
/// to work +36 (the first nine also to work +100), choose the semi-
/// transparency mode at work +120 from the flare state, save the view
/// rotation at work +136, then draw each live 24-byte debris piece with its
/// renderer (table 800B4400, type < 3).
fn draw_debris(m: &mut Machine, list: u32) {
    m.push_frame(0x28);
    let mut mask = m.lw(list);
    if mask != 0 {
        let mut table = 0x800b_7104u32;
        for i in 0..32u32 {
            let value = m.lhu(table);
            m.sh(WORK + 36 + i * 2, value);
            if slt(i, 9) {
                m.sh(WORK + 100 + i * 2, value);
            }
            table += 512;
        }
        let systems = m.gw(OBJECT_SYSTEMS);
        let mode = if m.lw(systems + 1480) & 4 != 0 {
            0xe600_0001
        } else {
            0xe600_0000
        };
        m.sw(WORK + 120, mode);
        save_rotation(m, WORK + 136);
        let mut piece = list + 4;
        while mask != 0 {
            if mask & 1 != 0 {
                let kind = lh(m, piece + 20);
                if kind < 3 {
                    let renderer = m.lw(DEBRIS_RENDERERS + (kind << 2));
                    dispatch::call(m, renderer, [piece, WORK, 0, 0]);
                }
            }
            mask >>= 1;
            piece += 24;
        }
    }
    m.pop_frame(0x28);
}

/// 80092850: explosions: save the view rotation at 0x1F800118, then draw
/// each live 28-byte explosion with 8009236C.
fn draw_explosions(m: &mut Machine, list: u32) {
    m.push_frame(0x28);
    let mut mask = lhu(m, list);
    if mask != 0 {
        save_rotation(m, SCRATCH + 0x118);
        let mut explosion = list + 4;
        loop {
            if mask & 1 != 0 {
                draw_explosion(m, SCRATCH, WORK, explosion);
            }
            mask >>= 1;
            explosion += 28;
            if mask == 0 {
                break;
            }
        }
        let _ = explosion;
    }
    m.pop_frame(0x28);
}

/// `mult` + `mfhi`.
fn mul_hi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}

/// SDK ScaleMatrix (800A6F08), by original address (not in `sdk.rs` yet).
fn scale_matrix(m: &mut Machine, matrix: u32, scale: u32) {
    dispatch::call(m, 0x800a_6f08, [matrix, scale, 0, 0]);
}

// JP-REVIEW(R0366): changed function: PAL 8008A310 -> JP 8008BA8C, del:42 imm:5 ins:86 op:26 reg:35, 194 instructions differ (whole routine)
/// 8008BA8C: the sun: a POLY_FT4 disc at the projected sun direction
/// (state +8), and when its high byte is set the four-sided corona (state
/// +16..+44) and a rotating glow (two RotMatrix frames, 80086948 between
/// faces) chained at OT slot 4094; then the screen flash (fill packet or
/// blend value) and the lens flare (8008B7AC) from its brightness, reduced
/// when the sun is below the eye. `state` is object systems +1480.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_sun` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_sun(m: &mut Machine, state: u32, sun: u32, height: u32) {
    crate::jp::res_p2_render::draw_sun(m, state, sun, height)
}

// JP-REVIEW(R0367): near function: PAL 80089BA0 -> JP 8008B310, imm:5 ins:3 op:2 reg:1, 11 instructions differ (whole routine)
/// 8008B310: the moon (or second sun): projected from state +8 (pulled in
/// when far below), a 16-segment disc of POLY_G4 quads (colour `colour`
/// depth-cued by height) rotated by 80030778 and scaled (ScaleMatrix),
/// wobbling with the frame counter; chained with draw-mode packets at OT
/// slot 30. `state` is object systems +1480.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_moon` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_moon(m: &mut Machine, state: u32, colour: u32) {
    crate::jp::res_p2_render::draw_moon(m, state, colour)
}

/// 8009044C: the lock-on marker of the cockpit view (camera mode 1): in
/// states 0..4 a trail quad toward the target (8008DE18, styles by state,
/// at OT slot 2), then either a pulsing ring sprite (states 0..2) or, for
/// states 3 and 4, a four-quad burst around the target's screen position;
/// in other camera modes 80090210 handles the marker.
fn draw_lock_marker(m: &mut Machine, marker: u32) {
    m.push_frame(0x30);
    'body: {
        let state = lh(m, marker + 4);
        let target = m.lw(marker).wrapping_sub(2);
        m.sw(WORK, state);
        m.sw(WORK + 4, target);
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay + 40);
        if m.lw(camera + 80) != 1 {
            tint_lock_light(m, target, state, WORK);
        }
        if !slt(state, 5) || (target as i32) < 0 {
            break 'body;
        }
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay + 40);
        let mode = m.lw(camera + 80);
        if mode != 1 || m.lw(camera) & 0x200 != 0 {
            break 'body;
        }
        let mut packet = m.lw(SCRATCH + 4);
        let context = m.gw(crate::common::CONTEXT);
        let frame = m.lw(context + 40);
        let systems = m.gw(OBJECT_SYSTEMS);
        let noise = m.lw(((frame & 0xff) << 2).wrapping_add(systems));
        let target = m.lw(WORK + 4);
        m.sw(WORK + 8, frame);
        m.sw(WORK + 12, noise);
        if target == 0 {
            let state = m.lw(WORK);
            if state != 3 {
                if !slt(state, 5) {
                    m.sw(SCRATCH + 4, packet);
                    break 'body;
                }
                lock_trail(m, marker, packet, state);
                packet += 40;
            }
        }
        let state = m.lw(WORK);
        if slt(state, 5) {
            for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
                m.gte.ctc2(register, 0);
            }
            if slt(state, 3) {
                lock_ring(m, marker, packet, state);
                packet += 40;
            } else {
                packet = lock_burst(m, marker, packet, state);
            }
        }
        m.sw(SCRATCH + 4, packet);
    }
    m.pop_frame(0x30);
}

/// Lock trail style: widths (work +32/+36), packet words +12/+20 and
/// halves +28/+36.
struct LockStyle(u32, u32, u32, u32, u16, u16);

/// 80090550..8009070C: the trail quad from the marker toward the target
/// (marker +8..+15 are the two screen ends), linked at OT slot 2.
fn lock_trail(m: &mut Machine, marker: u32, packet: u32, state: u32) {
    let ot = m.lw(SCRATCH);
    let tag = m.lw(ot + 8) | 0x0900_0000;
    m.sw(packet, tag);
    let ot = m.lw(SCRATCH);
    m.sw(ot + 8, link(packet));
    for offset in [8u32, 10, 12, 14] {
        let value = m.lhu(marker + offset);
        m.sh(WORK + 16 + offset, value);
    }
    // Width and colour style per state: widths at work +32/+36, packet
    // words +4/+12/+20 and halves +28/+36.
    let style = match state {
        0 => Some(LockStyle(1, 3, 0x4456_60a0, 0x0038_60af, 0x9fa0, 0x9faf)),
        1 => Some(LockStyle(2, 4, 0x4496_60b0, 0x0038_60bf, 0x9fb0, 0x9fbf)),
        2 => Some(LockStyle(3, 4, 0x44d6_60c0, 0x0038_60cf, 0x9fc0, 0x9fcf)),
        4 => Some(LockStyle(6, 7, 0x4556_60d0, 0x0038_60df, 0x9fd0, 0x9fdf)),
        _ => None,
    };
    if let Some(LockStyle(near, far, word12, word20, half28, half36)) = style {
        match state {
            0 => {
                m.sw(WORK + 32, near);
                m.sw(WORK + 36, far);
                m.sw(packet + 20, word20);
                m.sh(packet + 28, half28);
                m.sw(packet + 4, 0x2e83_8383);
                m.sw(packet + 12, word12);
            }
            1 => {
                m.sw(WORK + 32, near);
                m.sw(WORK + 36, far);
                m.sh(packet + 28, half28);
                m.sw(packet + 4, 0x2e83_8383);
                m.sw(packet + 12, word12);
                m.sw(packet + 20, word20);
            }
            2 => {
                m.sw(WORK + 36, far);
                m.sh(packet + 28, half28);
                m.sw(WORK + 32, near);
                m.sw(packet + 4, 0x2e83_8383);
                m.sw(packet + 12, word12);
                m.sw(packet + 20, word20);
            }
            _ => {
                m.sw(WORK + 32, near);
                m.sw(WORK + 36, far);
                m.sh(packet + 28, half28);
                m.sw(packet + 4, 0x2e83_8383);
                m.sw(packet + 12, word12);
                m.sw(packet + 20, word20);
            }
        }
        m.sh(packet + 36, half36);
        if state < 2 {
            // States 0 and 1 jitter the far end by the noise bits.
            let jitter_x = lhu(m, WORK + 12) & 1;
            let x = lhu(m, WORK + 24).wrapping_add(jitter_x);
            let noise = m.lw(WORK + 12);
            m.sh(WORK + 24, x as u16);
            let jitter_y = sra(noise, 1) & 1;
            let y = lhu(m, WORK + 26).wrapping_add(jitter_y);
            m.sh(WORK + 26, y as u16);
        }
    }
    build_trail_quad(m, packet, WORK + 16);
}

/// 8009073C..800908AC: the ring sprite: colour by state, depth-cued by the
/// noise word (pulse), UVs cycling with the frame counter mod 5; corners
/// from 8008DE18 on the scratch packet at work +40, mirrored on odd
/// frames. Linked at OT slot 2.
fn lock_ring(m: &mut Machine, marker: u32, packet: u32, state: u32) {
    let pulse = m.lw(WORK + 12) & 0x7ff;
    m.gte.mtc2(reg::IR0, pulse);
    let (colour, near, far) = match state {
        0 => (0x2e83_8383, 14, 16),
        1 => (0x2e73_8393, 19, 21),
        _ => (0x2e73_7383, 15, 18),
    };
    m.gte.mtc2(reg::RGBC, colour);
    m.sw(WORK + 32, near);
    m.sw(WORK + 36, far);
    cop2(m, DPCS);
    let ot = m.lw(SCRATCH);
    let tag = m.lw(ot + 8) | 0x0900_0000;
    m.sw(packet, tag);
    let ot = m.lw(SCRATCH);
    m.sw(ot + 8, link(packet));
    let frame = m.lw(WORK + 8);
    let fifth = sra(mul_hi(frame, 0x6666_6667), 1).wrapping_sub(sra(frame, 31));
    let phase = frame.wrapping_sub((fifth << 2).wrapping_add(fifth));
    let u = phase << 5;
    let u_end = u + 31;
    m.sw(packet + 12, u | 0x4056_6000);
    m.sw(packet + 20, u_end | 0x0038_6000);
    m.sh(packet + 28, (u | 0x9f00) as u16);
    m.sh(packet + 36, (u_end | 0x9f00) as u16);
    let near_end = m.lw(marker + 16);
    let far_end = m.lw(marker + 20);
    m.sw(WORK + 24, near_end);
    m.sw(WORK + 28, far_end);
    swc2(m, reg::RGB2, packet + 4);
    build_trail_quad(m, WORK + 40, WORK + 16);
    let (a, b, c, d) = if m.lw(WORK + 8) & 1 != 0 {
        (
            m.lw(WORK + 56),
            m.lw(WORK + 48),
            m.lw(WORK + 72),
            m.lw(WORK + 64),
        )
    } else {
        (
            m.lw(WORK + 48),
            m.lw(WORK + 56),
            m.lw(WORK + 64),
            m.lw(WORK + 72),
        )
    };
    m.sw(packet + 8, a);
    m.sw(packet + 16, b);
    m.sw(packet + 24, c);
    m.sw(packet + 32, d);
}

/// 800908B0..80090A44: the burst (states 3 and 4): four POLY_FT4 quads
/// around the target's screen point (marker +24), sized by the noise word,
/// chained at OT slot 2. Returns the new primitive cursor.
fn lock_burst(m: &mut Machine, marker: u32, packet: u32, state: u32) -> u32 {
    let target = m.lw(WORK + 4) << 10;
    let noise = m.lw(WORK + 12) & 0x3ff;
    m.gte.mtc2(reg::IR0, target.wrapping_add(noise));
    let colour = if state == 3 { 0x2e23_1333 } else { 0x2e33_2313 };
    m.gte.mtc2(reg::RGBC, colour);
    let mut packet = packet;
    let centre = m.lw(marker + 24);
    cop2(m, DPCS);
    let shrink = sra(m.lw(WORK + 12), 7) & 7;
    // Widescreen: the burst width squeezed like the projection.
    let half_w = widescreen::squeeze_x(m, 100u32.wrapping_sub(shrink));
    let half_h = 90u32.wrapping_sub(shrink);
    m.sw(WORK + 48, centre);
    let x = lhu(m, WORK + 48);
    m.sh(WORK + 56, x.wrapping_sub(half_w) as u16);
    let y = lhu(m, WORK + 50);
    m.sh(WORK + 64, x.wrapping_add(half_w) as u16);
    m.sh(WORK + 58, y.wrapping_sub(half_h) as u16);
    m.sh(WORK + 66, y.wrapping_add(half_h) as u16);
    let lit = m.gte.mfc2(reg::RGB2);
    let mut link_word = (packet << 8).wrapping_add(10240);
    for corner in 0..4u32 {
        m.sw(packet, (link_word >> 8) | 0x0900_0000);
        m.sw(packet + 4, lit);
        m.sw(packet + 12, 0x4596_80e0);
        m.sw(packet + 20, 0x0038_80ff);
        m.sh(packet + 28, 0x9fe0);
        m.sh(packet + 36, 0x9fff);
        m.sw(packet + 8, centre);
        let x = m.lhu(WORK + 48);
        m.sh(packet + 24, x);
        let y = m.lhu(WORK + 50);
        m.sh(packet + 18, y);
        let x = if corner & 1 != 0 {
            m.lhu(WORK + 64)
        } else {
            m.lhu(WORK + 56)
        };
        m.sh(packet + 32, x);
        m.sh(packet + 16, x);
        let y = if corner & 2 != 0 {
            m.lhu(WORK + 66)
        } else {
            m.lhu(WORK + 58)
        };
        m.sh(packet + 34, y);
        m.sh(packet + 26, y);
        link_word = link_word.wrapping_add(10240);
        packet += 40;
    }
    let slot = m.lw(SCRATCH) + 8;
    insert_chain(m, slot, packet - 160, packet - 40);
    packet
}

// ==== 8007B438: camera update; 800332E4: view matrix ===========================================

/// Game context (`[gp+0x2EC]`).
fn game_context(m: &mut Machine) -> u32 {
    m.gw(crate::common::CONTEXT)
}

/// Word at `offset` of the game context.
fn context_word(m: &mut Machine, offset: u32) -> u32 {
    let context = game_context(m);
    m.lw(context + offset)
}

/// Entity pointer `index` of `[gp+0x2E8]`.
fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw(table.wrapping_add(index << 2))
}

// JP-REVIEW(R0370): near function: PAL 80079DA4 -> JP 8007B438, del:6 imm:18, 24 instructions differ (whole routine)
/// 8007B438: per-frame camera update. Runs the handler of the camera mode
/// (camera +80; only mode 1, the cockpit/chase view 80077A30, is
/// translated so far), follows the attached object (camera flag 0x100:
/// position and angles of the object at +52 plus the offsets at +56/+72),
/// applies the ground clamp (flag 0x800), and records the frame's velocity
/// and angular velocity at +156/+172 (zeroed once after a cut, flag 0x400).
// JP-PATCH: replaced by `crate::jp::res_p2_render::update_camera` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_camera(m: &mut Machine, camera: u32) {
    crate::jp::res_p2_render::update_camera(m, camera)
}

/// 8007786C: mark the camera for a cut (flag 0x400: no velocity this
/// frame); owned by shared.rs.
fn mark_camera_cut(m: &mut Machine, camera: u32) {
    crate::shared::mark_camera(m, camera);
}

/// 80077A30: cockpit/chase camera (modes 1 and 1024): the look direction
/// toward the locked target (800779C8) plus the look-around and debug view
/// offsets of the controls, eased toward at a quarter of the difference
/// per frame (snapped when close; set directly on a view cut), then the
/// view angles relative to the followed object (800313A8).
fn cockpit_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x20);
    let look = frame + 16;
    let context = game_context(m);
    let target = if m.lw(context + 296) & 0x2000 != 0 {
        m.lw(camera + 188)
    } else {
        u32::MAX
    };
    let origin = if m.lw(camera) & 0x100 != 0 {
        m.lw(camera + 52)
    } else {
        camera + 4
    };
    look_toward(m, origin, target, look);
    let mut pitch = lh(m, look);
    let context = game_context(m);
    let mut yaw = lh(m, look + 2);
    let buttons = m.lw(context + 312);
    let roll = lh(m, look + 4);
    if buttons & 0x10 != 0 {
        pitch = pitch.wrapping_sub(m.lw(context + 332));
    }
    if buttons & 0x20 != 0 {
        yaw = yaw.wrapping_add(m.lw(context + 336));
    }
    let views = m.lw(context + 296);
    if views & 0x10_0000 != 0 {
        yaw = yaw.wrapping_add(8192);
    }
    if views & 0x1_0000 != 0 {
        pitch = pitch.wrapping_add(4096);
    }
    if views & 0x2_0000 != 0 {
        pitch = pitch.wrapping_sub(4096);
    }
    if views & 0x4_0000 != 0 {
        yaw = yaw.wrapping_sub(4096);
    }
    if views & 0x8_0000 != 0 {
        yaw = yaw.wrapping_add(4096);
    }
    let current = camera + 182;
    if slt(lh(m, current).wrapping_sub(yaw), (-8192i32) as u32) {
        let value = lhu(m, current).wrapping_add(16384);
        m.sh(current, value as u16);
    }
    if !slt(lh(m, current).wrapping_sub(yaw), 8193) {
        let value = lhu(m, current).wrapping_sub(16384);
        m.sh(current, value as u16);
    }
    let context = game_context(m);
    if m.lw(context + 300) & 0x10_0000 != 0 || m.lw(context + 308) & 0x10_0000 != 0 {
        m.sh(camera + 180, pitch as u16);
        m.sh(camera + 182, yaw as u16);
        m.sh(camera + 184, roll as u16);
        mark_camera_cut(m, camera);
    } else {
        let targets = [(180u32, pitch), (182, yaw), (184, roll)];
        let mut steps = [0u32; 3];
        for (i, &(offset, goal)) in targets.iter().enumerate() {
            let value = lh(m, camera + offset);
            let difference = value.wrapping_sub(goal);
            let magnitude = if (difference as i32) >= 0 {
                sra(difference, 2)
            } else {
                sra(goal.wrapping_sub(value), 2)
            };
            steps[i] = magnitude.wrapping_add(2);
        }
        for (i, &(offset, goal)) in targets.iter().enumerate() {
            let address = camera + offset;
            if slt(lh(m, address), goal) {
                let value = lhu(m, address).wrapping_add(steps[i]);
                m.sh(address, value as u16);
            }
            if slt(goal, lh(m, address)) {
                let value = lhu(m, address).wrapping_sub(steps[i]);
                m.sh(address, value as u16);
            }
        }
        for (i, &(offset, goal)) in targets.iter().enumerate() {
            let address = camera + offset;
            let value = lh(m, address);
            let difference = value.wrapping_sub(goal);
            let close = if (difference as i32) >= 0 {
                slt(difference, steps[i])
            } else {
                slt(goal.wrapping_sub(value), steps[i])
            };
            if close {
                m.sh(address, goal as u16);
            }
        }
    }
    let (base, angles) = if m.lw(camera) & 0x100 != 0 {
        let object = m.lw(camera + 52);
        view_angles(m, object + 16, camera + 180, look);
        (m.lw(camera + 52) + 16, look)
    } else {
        view_angles(m, camera + 20, camera + 180, look);
        (camera + 20, look)
    };
    let relative: [u32; 3] = std::array::from_fn(|i| {
        lh(m, angles + i as u32 * 2).wrapping_sub(lh(m, base + i as u32 * 2))
    });
    m.sh(camera + 72, relative[0] as u16);
    m.sh(camera + 74, relative[1] as u16);
    m.sh(camera + 76, relative[2] as u16);
    m.pop_frame(0x20);
}

/// 800779C8: look angles from `origin` toward entity `target` when it is
/// live (flags 0x4000) via 800317F0 with roll 0, else all zero.
fn look_toward(m: &mut Machine, origin: u32, target: u32, out: u32) {
    m.push_frame(0x18);
    let mut aimed = false;
    if (target as i32) > 0 {
        let entity = entity(m, target);
        if m.lw(entity) & 0x4000 != 0 {
            aim_angles(m, origin, entity + 8, out);
            m.sh(out + 4, 0);
            aimed = true;
        }
    }
    if !aimed {
        m.sh(out, 0);
        m.sh(out + 2, 0);
        m.sh(out + 4, 0);
    }
    m.pop_frame(0x18);
}

/// 80083F40: vector length of (x, y, z) by SQR and the square-root table
/// at 800B6DD4.
fn vector_length(m: &mut Machine, x: u32, y: u32, z: u32) -> u32 {
    // One implementation: player.rs owns 80083F40.
    crate::player::vector_length_800828f8(m, x, y, z)
}

/// 800317F0: angles (pitch, yaw, roll) of the direction from `from` to
/// `to` (word positions), rotated into the frame of `from`'s angles at
/// +16 (800308F0); returns the distance (0x100000 when beyond 0xAFFF
/// units), negated when the pitch passes the vertical.
fn aim_angles(m: &mut Machine, from: u32, to: u32, out: u32) -> u32 {
    let frame = m.push_frame(0x60);
    shared::f_800306ac(m, [from + 16, frame + 16, out, 0]);
    load_rotation(m, frame + 16);
    let dx = sra(m.lw(from).wrapping_sub(m.lw(to)), 12);
    let dy = sra(m.lw(from + 4).wrapping_sub(m.lw(to + 4)), 12);
    let dz = sra(m.lw(from + 8).wrapping_sub(m.lw(to + 8)), 12);
    let mut distance = 0x10_0000u32;
    if !slt(0xafff, abs(dx).wrapping_add(abs(dy)).wrapping_add(abs(dz))) {
        distance = vector_length(m, dx, dy, dz);
    }
    let vector = frame + 48;
    m.sw(vector, dx);
    m.sw(vector + 4, dy);
    m.sw(vector + 8, dz);
    sdk::apply_rot_matrix_lv(m, vector, vector);
    let x = m.lw(vector);
    let y = m.lw(vector + 4);
    let z = m.lw(vector + 8);
    let horizontal = abs(x).wrapping_add(abs(z));
    let mut planar = 0x10_0000u32;
    if !slt(0x5_7fff, horizontal) {
        let shift = if slt(0xafff, horizontal) { 3 } else { 0 };
        let length = vector_length(m, sra(x, shift), 0, sra(z, shift));
        planar = sllv(length, shift);
    }
    let pitch = shared::f_80030b10(m, [y, planar, 0, 0]);
    m.sh(out, pitch as u16);
    let yaw = 0u32.wrapping_sub(shared::f_80030b10(m, [x, 0u32.wrapping_sub(z), 0, 0]));
    m.sh(out + 2, yaw as u16);
    let roll = 0u32.wrapping_sub(shared::f_80030b10(m, [x, y, 0, 0]));
    m.sh(out + 4, roll as u16);
    if !slt(abs(lh(m, out + 2)), 4097) {
        distance = 0u32.wrapping_sub(distance);
    }
    m.pop_frame(0x60);
    distance
}

/// 800313A8: compose the base angles `base` (pitch, yaw, roll halfwords)
/// with the relative angles `offset` and write the resulting angles to
/// `out`: rotate the unit axes (0x800234D8/D4: +Z 0x4000 and -Y 0x4000)
/// by both matrices (80030A68, ApplyRotMatrixLV), recover pitch and yaw
/// from the forward axis (80030EB8, 80030D54) and roll from the up axis in
/// the frame of those (80030BDC, 80030D54), with angle wrapping. Small
/// offsets are doubled when the base pitch is near vertical (gimbal
/// guard).
fn view_angles(m: &mut Machine, base: u32, offset: u32, out: u32) {
    let frame = m.push_frame(0x98);
    for (source, target) in [(0x8002_34d8u32, frame + 56), (0x8002_34e8, frame + 72)] {
        let a = m.lw(source);
        let b = m.lw(source + 4);
        let c = m.lw(source + 8);
        m.sw(target, a);
        m.sw(target + 4, b);
        m.sw(target + 8, c);
        let d = m.lw(source + 12);
        m.sw(target + 12, d);
    }
    let angles = frame + 48;
    let first = m.lhu(offset);
    m.sh(angles, first);
    let value = m.lhu(offset + 2);
    m.sh(angles + 2, value);
    let value = m.lhu(offset + 4);
    m.sh(angles + 4, value);
    if !slt(abs(lh(m, base)), 3985) && slt(abs(sra(u32::from(first) << 16, 16)), 8) {
        m.sh(angles, u32::from(first).wrapping_shl(1) as u16);
    }
    for (limit, field, small) in [
        (3977u32, 2u32, 8u32),
        (4041, 0, 12),
        (4033, 2, 12),
        (4065, 0, 16),
        (4059, 2, 16),
    ] {
        if !slt(abs(lh(m, base)), limit) {
            let value = lh(m, angles + field);
            let raw = lhu(m, angles + field);
            if slt(abs(value), small) {
                m.sh(angles + field, (raw << 1) as u16);
            }
        }
    }
    let matrix = frame + 16;
    let forward = frame + 88;
    let up = frame + 104;
    shared::f_80030824(m, [angles, matrix, out, 0]);
    load_rotation(m, matrix);
    sdk::apply_rot_matrix_lv(m, frame + 56, forward);
    sdk::apply_rot_matrix_lv(m, frame + 72, up);
    shared::f_80030824(m, [base, matrix, out, 0]);
    load_rotation(m, matrix);
    sdk::apply_rot_matrix_lv(m, forward, forward);
    sdk::apply_rot_matrix_lv(m, up, up);
    let fy = sra(0u32.wrapping_sub(m.lw(forward + 4)), 4);
    let ux = sra(m.lw(up), 4);
    let uz = sra(m.lw(up + 8), 4);
    m.sw(up, ux);
    m.sw(up + 8, uz);
    let uy = sra(m.lw(up + 4), 4);
    m.sw(up + 4, uy);
    let mut pitch = shared::f_80030c74(m, [fy, 0, 0, 0]);
    let fx = m.lw(forward);
    let fz = m.lw(forward + 8);
    let yaw_raw = shared::f_80030b10(m, [0u32.wrapping_sub(fx), fz, 0, 0]);
    let mut yaw = 0u32.wrapping_sub(yaw_raw);
    m.sh(angles, 0u32.wrapping_sub(pitch) as u16);
    m.sh(angles + 2, yaw_raw as u16);
    m.sh(angles + 4, 0);
    shared::f_80030998(m, [angles, matrix, 0, 0]);
    load_rotation(m, matrix);
    sdk::apply_rot_matrix_lv(m, up, up);
    let ux = sra(m.lw(up), 2);
    let uy = sra(m.lw(up + 4), 2);
    let uz = sra(m.lw(up + 8), 2);
    m.sw(up, ux);
    m.sw(up + 4, uy);
    m.sw(up + 8, uz);
    let mut roll = shared::f_80030b10(m, [ux, 0u32.wrapping_sub(uy), 0, 0]);
    if !slt(pitch, 4097) {
        pitch = 8192u32.wrapping_sub(pitch);
        yaw = yaw.wrapping_add(8192);
        roll = roll.wrapping_add(8192);
    }
    if slt(pitch, (-4096i32) as u32) {
        pitch = (-8192i32 as u32).wrapping_sub(pitch);
        yaw = yaw.wrapping_add(8192);
        roll = roll.wrapping_add(8192);
    }
    if !slt(yaw, 8192) {
        yaw = yaw.wrapping_sub(16384);
    }
    if slt(yaw, (-8192i32) as u32) {
        yaw = yaw.wrapping_add(16384);
    }
    if !slt(roll, 8192) {
        roll = roll.wrapping_sub(16384);
    }
    if slt(roll, (-8192i32) as u32) {
        roll = roll.wrapping_add(16384);
    }
    m.sh(out, pitch as u16);
    m.sh(out + 2, yaw as u16);
    m.sh(out + 4, roll as u16);
    m.pop_frame(0x98);
}

/// 800332E4: the view matrix: camera angles (+20) to the matrix at
/// `matrix` (80030778), then its transpose at `view`; returns `view`.
pub fn view_matrix(m: &mut Machine, camera: u32, matrix: u32, view: u32) -> u32 {
    m.push_frame(0x20);
    shared::f_80030534(m, [camera + 20, matrix, view, 0]);
    let result = sdk::transpose_matrix(m, matrix, view);
    m.pop_frame(0x20);
    result
}

// ---- lit record faces (800637B0's parts) ------------------------------------------------------

/// Depth slot of a lit-record face in the record's 39-entry table
/// (`work` +1016 points at its middle): the face depth relative to the
/// record depth (`work` +1014), within -75..76.
fn lit_slot(m: &mut Machine, work: u32, depth: u32) -> Option<u32> {
    let centre = lh(m, work + 1014);
    let relative = depth.wrapping_sub(centre);
    if relative.wrapping_add(75) >= 152 {
        return None;
    }
    let table = m.lw(work + 1016);
    Some(table.wrapping_add(sra(relative, 2) << 2))
}

/// 8006305C: lit body triangles (32-byte faces: indices, three colours,
/// three UVs): back-face culled, gouraud colours depth-cued toward the
/// fog colour (DPCT with IR0 set by the caller), POLY_GT3 into the record
/// table.
fn draw_body_triangles(m: &mut Machine, work: u32) {
    let sxy = work + WORK_SXY;
    let szs = work + WORK_SZ;
    let faces = m.lw(work + 8);
    let mut packet = m.lw(work + 4);
    let count = m.lw(faces);
    if (count as i32) > 0 {
        let mut face = faces + 4;
        for _ in 0..count {
            let indices = m.lw(face + 4);
            let i0 = indices & 0xff;
            let i1 = (indices >> 8) & 0xff;
            let i2 = indices >> 16;
            lwc2(m, reg::SXY0, sxy + (i0 << 2));
            lwc2(m, reg::SXY1, sxy + (i1 << 2));
            lwc2(m, reg::SXY2, sxy + (i2 << 2));
            cop2(m, NCLIP);
            if (m.gte.mfc2(reg::MAC0) as i32) > 0 {
                let z0 = lh(m, szs + (i0 << 1));
                let z2 = lh(m, szs + (i2 << 1));
                let z1 = lh(m, szs + (i1 << 1));
                m.gte.mtc2(reg::SZ1, z0);
                m.gte.mtc2(reg::SZ2, z1);
                m.gte.mtc2(reg::SZ3, z2);
                cop2(m, AVSZ3);
                let average = m.gte.mfc2(reg::MAC0);
                let shift = m.lw(work + 12).wrapping_add(10);
                let depth = sra(average, shift).wrapping_add(4);
                if let Some(slot) = lit_slot(m, work, depth) {
                    add_prim(m, slot, packet, 0x0900_0000);
                    swc2(m, reg::SXY0, packet + 8);
                    swc2(m, reg::SXY1, packet + 20);
                    swc2(m, reg::SXY2, packet + 32);
                    let colour = m.lw(face + 8);
                    m.gte.mtc2(reg::RGBC, colour);
                    m.gte.mtc2(reg::RGB0, colour);
                    lwc2(m, reg::RGB1, face + 12);
                    lwc2(m, reg::RGB2, face + 16);
                    cop2(m, DPCT);
                    for (from, to) in [(20u32, 12u32), (24, 24), (28, 36)] {
                        let uv = m.lw(face + from);
                        m.sw(packet + to, uv);
                    }
                    swc2(m, reg::RGB0, packet + 4);
                    swc2(m, reg::RGB1, packet + 16);
                    swc2(m, reg::RGB2, packet + 28);
                    packet += 40;
                }
            }
            face += 32;
        }
    }
    m.sw(work + 4, packet);
}

/// 800631F8: lit body quads (36-byte faces: indices, four colours, UVs):
/// visible if either half passes NCLIP; the first colour depth-cued alone
/// (DPCS), the other three by DPCT; POLY_GT4 into the record table.
fn draw_body_quads(m: &mut Machine, work: u32) {
    m.push_frame(0x8);
    let sxy = work + WORK_SXY;
    let szs = work + WORK_SZ;
    let faces = m.lw(work + 8);
    let mut packet = m.lw(work + 4);
    let count = m.lw(faces);
    if (count as i32) > 0 {
        let mut face = faces + 4;
        for _ in 0..count {
            let indices = m.lw(face + 4);
            let i0 = indices & 0xff;
            let i1 = (indices >> 8) & 0xff;
            let i2 = (indices >> 16) & 0xff;
            let i3 = indices >> 24;
            let xy0 = m.lw(sxy + (i0 << 2));
            lwc2(m, reg::SXY1, sxy + (i1 << 2));
            lwc2(m, reg::SXY2, sxy + (i2 << 2));
            m.gte.mtc2(reg::SXY0, xy0);
            let xy3 = m.lw(sxy + (i3 << 2));
            cop2(m, NCLIP);
            let area = m.gte.mfc2(reg::MAC0);
            m.gte.mtc2(reg::SXY0, xy3);
            let visible = (area as i32) > 0 || {
                cop2(m, NCLIP);
                (m.gte.mfc2(reg::MAC0) as i32) < 0
            };
            if visible {
                let sum = lh(m, szs + (i0 << 1))
                    .wrapping_add(lh(m, szs + (i1 << 1)))
                    .wrapping_add(lh(m, szs + (i2 << 1)))
                    .wrapping_add(lh(m, szs + (i3 << 1)));
                let shift = m.lw(work + 12);
                let depth = sra(sra(sum, 2), shift).wrapping_add(4);
                if let Some(slot) = lit_slot(m, work, depth) {
                    add_prim(m, slot, packet, 0x0c00_0000);
                    m.sw(packet + 8, xy0);
                    swc2(m, reg::SXY1, packet + 20);
                    swc2(m, reg::SXY2, packet + 32);
                    swc2(m, reg::SXY0, packet + 44);
                    lwc2(m, reg::RGBC, face + 8);
                    cop2(m, DPCS);
                    let uv = m.lw(face + 24);
                    m.sw(packet + 12, uv);
                    let uv = m.lw(face + 28);
                    m.sw(packet + 24, uv);
                    swc2(m, reg::RGB2, packet + 4);
                    lwc2(m, reg::RGB0, face + 12);
                    lwc2(m, reg::RGB1, face + 16);
                    lwc2(m, reg::RGB2, face + 20);
                    cop2(m, DPCT);
                    let uvs = m.lw(face + 32);
                    m.sw(packet + 36, uvs);
                    m.sw(packet + 48, uvs >> 16);
                    swc2(m, reg::RGB0, packet + 16);
                    swc2(m, reg::RGB1, packet + 28);
                    swc2(m, reg::RGB2, packet + 40);
                    packet += 52;
                }
            }
            face += 36;
        }
    }
    m.sw(work + 4, packet);
    m.pop_frame(0x8);
}

/// NCT sf=1 lm=1.
const NCT: u32 = 0x0d8_0420;
/// NCS sf=1 lm=1.
const NCS: u32 = 0x0c8_041e;

/// 800633D8: coloured part triangles (24-byte faces: indices, three
/// normal indices into the table at `work` +1008, UVs): lit per vertex by
/// NCT with the base colour 0x34808080.
fn draw_coloured_triangles(m: &mut Machine, work: u32) {
    let sxy = work + WORK_SXY;
    let szs = work + WORK_SZ;
    let mut packet = m.lw(work + 4);
    let faces = m.lw(work + 8);
    let normals = m.lw(work + 1008);
    let count = m.lw(faces);
    m.gte.mtc2(reg::RGBC, 0x3480_8080);
    if (count as i32) > 0 {
        let mut face = faces + 4;
        for _ in 0..count {
            let indices = m.lw(face + 4);
            let i0 = indices & 0xff;
            let i1 = (indices >> 8) & 0xff;
            let i2 = indices >> 16;
            lwc2(m, reg::SXY0, sxy + (i0 << 2));
            lwc2(m, reg::SXY1, sxy + (i1 << 2));
            lwc2(m, reg::SXY2, sxy + (i2 << 2));
            cop2(m, NCLIP);
            if (m.gte.mfc2(reg::MAC0) as i32) > 0 {
                let z0 = lh(m, szs + (i0 << 1));
                let z1 = lh(m, szs + (i1 << 1));
                let z2 = lh(m, szs + (i2 << 1));
                m.gte.mtc2(reg::SZ1, z0);
                m.gte.mtc2(reg::SZ2, z1);
                m.gte.mtc2(reg::SZ3, z2);
                cop2(m, AVSZ3);
                let average = m.gte.mfc2(reg::MAC0);
                let shift = m.lw(work + 12).wrapping_add(10);
                let depth = sra(average, shift);
                if let Some(slot) = lit_slot(m, work, depth) {
                    let n0 = normals + (lbu(m, face + 8) << 3);
                    let n1 = normals + (lbu(m, face + 9) << 3);
                    let n2 = normals + (lbu(m, face + 10) << 3);
                    lwc2(m, reg::VXY0, n0);
                    lwc2(m, reg::VZ0, n0 + 4);
                    lwc2(m, reg::VXY1, n1);
                    lwc2(m, reg::VZ1, n1 + 4);
                    lwc2(m, reg::VXY2, n2);
                    lwc2(m, reg::VZ2, n2 + 4);
                    cop2(m, NCT);
                    add_prim(m, slot, packet, 0x0900_0000);
                    let uv0 = m.lw(face + 12);
                    let uv1 = m.lw(face + 16);
                    let uv2 = m.lw(face + 20);
                    m.sw(packet + 12, uv0);
                    m.sw(packet + 24, uv1);
                    m.sw(packet + 36, uv2);
                    swc2(m, reg::SXY0, packet + 8);
                    swc2(m, reg::SXY1, packet + 20);
                    swc2(m, reg::SXY2, packet + 32);
                    swc2(m, reg::RGB0, packet + 4);
                    swc2(m, reg::RGB1, packet + 16);
                    swc2(m, reg::RGB2, packet + 28);
                    packet += 40;
                }
            }
            face += 24;
        }
    }
    m.sw(work + 4, packet);
}

/// 8006358C: coloured part quads (24-byte faces: indices, four normal
/// indices, UVs): the first vertex lit by NCS, the other three by NCT,
/// base colour 0x3C808080; POLY_GT4 into the record table.
fn draw_coloured_quads(m: &mut Machine, work: u32) {
    m.push_frame(0x8);
    let sxy = work + WORK_SXY;
    let szs = work + WORK_SZ;
    let mut packet = m.lw(work + 4);
    let faces = m.lw(work + 8);
    let normals = m.lw(work + 1008);
    let count = m.lw(faces);
    m.gte.mtc2(reg::RGBC, 0x3c80_8080);
    if (count as i32) > 0 {
        let mut face = faces + 4;
        for _ in 0..count {
            let indices = m.lw(face + 4);
            let i0 = indices & 0xff;
            let i1 = (indices >> 8) & 0xff;
            let i2 = (indices >> 16) & 0xff;
            let i3 = indices >> 24;
            let xy0 = m.lw(sxy + (i0 << 2));
            lwc2(m, reg::SXY1, sxy + (i1 << 2));
            lwc2(m, reg::SXY2, sxy + (i2 << 2));
            m.gte.mtc2(reg::SXY0, xy0);
            let xy3 = m.lw(sxy + (i3 << 2));
            cop2(m, NCLIP);
            let area = m.gte.mfc2(reg::MAC0);
            m.gte.mtc2(reg::SXY0, xy3);
            let visible = (area as i32) > 0 || {
                cop2(m, NCLIP);
                (m.gte.mfc2(reg::MAC0) as i32) < 0
            };
            if visible {
                let sum = lh(m, szs + (i0 << 1))
                    .wrapping_add(lh(m, szs + (i1 << 1)))
                    .wrapping_add(lh(m, szs + (i2 << 1)))
                    .wrapping_add(lh(m, szs + (i3 << 1)));
                let shift = m.lw(work + 12);
                let depth = sra(sra(sum, 2), shift);
                if let Some(slot) = lit_slot(m, work, depth) {
                    let n0 = normals + (lbu(m, face + 8) << 3);
                    lwc2(m, reg::VXY0, n0);
                    lwc2(m, reg::VZ0, n0 + 4);
                    cop2(m, NCS);
                    let n1 = normals + (lbu(m, face + 9) << 3);
                    let n2 = normals + (lbu(m, face + 10) << 3);
                    let n3 = normals + (lbu(m, face + 11) << 3);
                    lwc2(m, reg::VXY2, n3);
                    lwc2(m, reg::VZ2, n3 + 4);
                    lwc2(m, reg::VXY1, n2);
                    lwc2(m, reg::VZ1, n2 + 4);
                    swc2(m, reg::RGB2, packet + 4);
                    lwc2(m, reg::VXY0, n1);
                    lwc2(m, reg::VZ0, n1 + 4);
                    cop2(m, NCT);
                    add_prim(m, slot, packet, 0x0c00_0000);
                    let uv0 = m.lw(face + 12);
                    m.sw(packet + 8, xy0);
                    m.sw(packet + 12, uv0);
                    let uvs = m.lw(face + 20);
                    let uv1 = m.lw(face + 16);
                    m.sw(packet + 36, uvs);
                    m.sw(packet + 24, uv1);
                    m.sw(packet + 48, uvs >> 16);
                    swc2(m, reg::SXY1, packet + 20);
                    swc2(m, reg::SXY2, packet + 32);
                    swc2(m, reg::SXY0, packet + 44);
                    swc2(m, reg::RGB0, packet + 16);
                    swc2(m, reg::RGB1, packet + 28);
                    swc2(m, reg::RGB2, packet + 40);
                    packet += 52;
                }
            }
            face += 24;
        }
    }
    m.sw(work + 4, packet);
    m.pop_frame(0x8);
}

// ---- object renderers (table 800B439C) and helpers ------------------------------------------

/// Sine/cosine table (cos, sin halfword pairs, 4096 steps).
const SINES: u32 = 0x800b_7104;
/// GPF sf=1.
const GPF: u32 = 0x198_003d;
/// MVMVA sf=1, rotation x V0, no translation.
const MVMVA_RT_V0: u32 = 0x048_6012;

/// The sprite UV/size bytes shared by 80085A94 and 80085C8C: `frame`
/// selects one of 16 64x64 cells (4 per row), halved for sprites of
/// projected size 17 and above.
fn sprite_cell(m: &mut Machine, packet: u32, frame: u32, size: u32) {
    let shift = u32::from(slt(size, 17));
    let u = (frame & 3) << 6;
    let u0 = sra(u, shift);
    let u1 = sra(u + 64, shift).wrapping_sub(1);
    let v = sra(frame, 2) << 6;
    let v0 = sra(v, shift);
    m.sb(packet + 21, v0 as u8);
    m.sb(packet + 13, v0 as u8);
    let v1 = sra(v + 64, shift).wrapping_sub(1);
    m.sb(packet + 28, u0 as u8);
    m.sb(packet + 12, u0 as u8);
    m.sb(packet + 36, u1 as u8);
    m.sb(packet + 20, u1 as u8);
    m.sb(packet + 37, v1 as u8);
    m.sb(packet + 29, v1 as u8);
}

/// Link a sprite packet at the record's OT depth (screen record +4): with
/// an appended DR_TPAGE (0xE1000220, 10 words) when `flags` bit 0 is
/// clear, else as a plain 9-word POLY_FT4. Returns the new cursor.
fn link_sprite(m: &mut Machine, work: u32, screen: u32, packet: u32, flags: u32) -> u32 {
    let plain = flags & 1 != 0;
    if !plain {
        m.sw(packet + 40, 0xe100_0220);
    }
    let code = if plain { 0x0900_0000 } else { 0x0a00_0000 };
    let depth = lh(m, screen + 4);
    let ot = m.lw(work);
    let tag = m.lw((depth << 2).wrapping_add(ot)) | code;
    m.sw(packet, tag);
    let depth = lh(m, screen + 4);
    let ot = m.lw(work);
    m.sw((depth << 2).wrapping_add(ot), link(packet));
    if plain { packet + 40 } else { packet + 44 }
}

/// 80085A94: object class 0 (flak burst): a POLY_FT4 sprite whose
/// animation frame advances with the object's age (word 0: age low,
/// lifetime high), grey 0x838383, rotated corners by 8008591C.
fn draw_burst(m: &mut Machine, work: u32, screen: u32, object: u32) {
    m.push_frame(0x28);
    let state = m.lw(object);
    let packet = m.lw(work + 4);
    let mut life = sra(state, 16);
    if life == 0 {
        life = 1;
    }
    let age = sra(state << 16, 16);
    let frame = div_lo((age << 2).wrapping_add(age) << 1, life);
    let centre = m.lw(screen);
    let size = lh(m, screen + 6);
    m.sw(work + 24, 0);
    m.sw(work + 28, centre);
    m.sh(work + 22, size as u16);
    let frame = frame.wrapping_add(6);
    m.sh(work + 20, size as u16);
    if slt(size, 8) {
        m.sw(work + 16, 0);
    }
    sprite_corners(m, work, packet);
    sprite_cell(m, packet, frame, size);
    let flags = m.lw(work + 8);
    m.sw(packet + 4, 0x2e83_8383);
    let (page, clut) = if flags == 0 {
        (0x4056, 0x17)
    } else {
        (0x4016, ((flags & 3) << 5) | 0x17)
    };
    m.sh(packet + 14, page);
    m.sh(packet + 22, clut as u16);
    let next = link_sprite(m, work, screen, packet, flags);
    m.sw(work + 4, next);
    m.pop_frame(0x28);
}

/// 80085C8C: object class 1 (smoke puff): grows from 6 to 16 cells and
/// from the projected size to double (GPL), spins with the object's age
/// (work +16), depth-cued grey 0x535353 toward 60, corners by 8008591C.
fn draw_puff(m: &mut Machine, work: u32, screen: u32, object: u32) {
    m.push_frame(0x18);
    let state = m.lw(object);
    let packet = m.lw(work + 4);
    let mut life = sra(state, 16);
    if life == 0 {
        life = 1;
    }
    let age = sra(state << 16, 16);
    let fraction = div_lo(age << 12, life);
    let size = lh(m, screen + 6);
    m.gte.mtc2(reg::MAC1, 6);
    m.gte.mtc2(reg::IR1, 10);
    m.gte.mtc2(reg::IR0, fraction);
    let spin = m.lw(work + 12);
    m.gte.mtc2(reg::MAC2, size);
    let growth = if spin != 0 { size << 1 } else { size };
    m.gte.mtc2(reg::IR2, growth);
    let angle = m
        .lw(work + 16)
        .wrapping_add(sllv(age, spin.wrapping_add(5)));
    m.sw(work + 16, angle);
    cop2(m, GPL);
    let flags = m.lw(work + 8);
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 60);
    }
    m.gte.mtc2(reg::RGBC, 0x2e53_5353);
    let size = m.gte.mfc2(reg::MAC2);
    let frame = m.gte.mfc2(reg::MAC1);
    m.sh(work + 22, size as u16);
    m.sh(work + 20, size as u16);
    cop2(m, DPCS);
    let clut = if flags & 1 == 0 {
        ((flags & 3) << 5) | 0x17
    } else {
        55
    };
    m.sh(packet + 22, clut as u16);
    let next = link_sprite(m, work, screen, packet, flags);
    m.sw(work + 4, next);
    if slt(size, 8) {
        m.sw(work + 16, 0);
    }
    sprite_cell(m, packet, frame, size);
    m.sh(packet + 14, 0x40d6);
    swc2(m, reg::RGB2, packet + 4);
    let centre = m.lw(screen);
    m.sw(work + 24, 0);
    m.sw(work + 28, centre);
    sprite_corners(m, work, packet);
    m.pop_frame(0x18);
}

/// 8008591C: the four corners of a sprite quad at packet +8/+16/+24/+32:
/// centre `sprite` +28 (plus the rotated offset +24), half sizes from
/// +20/+22, rotated by the 12-bit angle at +16 >> 2 (the rotation matrix
/// is rebuilt only when the angle differs from the cached +88).
fn sprite_corners(m: &mut Machine, sprite: u32, packet: u32) {
    let mut x = lh(m, sprite + 28);
    let angle = sra(m.lw(sprite + 16), 2) & 0xfff;
    let mut y = lh(m, sprite + 30);
    let (dx0, dy0, dx1, dy1);
    if angle != 0 {
        let cached = m.lw(sprite + 88);
        let offset = m.lw(sprite + 24);
        if angle != cached {
            let pair = m.lw(SINES + (angle << 2));
            m.sw(sprite + 88, angle);
            let cos = pair >> 16;
            m.gte
                .ctc2(ctrl::R11R12, (0u32.wrapping_sub(pair) << 16) | cos);
            m.gte.ctc2(ctrl::R22R23, cos);
            m.gte.ctc2(ctrl::R13R21, pair << 16);
            m.gte.ctc2(ctrl::R31R32, 0);
            m.gte.ctc2(ctrl::R33, 4096);
        }
        if offset != 0 {
            m.gte.mtc2(reg::VXY0, offset);
            cop2(m, MVMVA_RT_V0);
            let ox = m.gte.mfc2(reg::MAC1);
            let oy = m.gte.mfc2(reg::MAC2);
            x = x.wrapping_add(widescreen::squeeze_x(m, ox));
            y = y.wrapping_add(oy);
        }
        let height = lhu(m, sprite + 22) << 16;
        let half_height = sra(height, 16);
        let half_width = sra(lhu(m, sprite + 20), 1);
        m.gte.mtc2(reg::VXY0, half_width | (sra(height, 17) << 16));
        let lower = sra(half_height.wrapping_add(1), 1);
        cop2(m, MVMVA_RT_V0);
        let second = half_width | (0u32.wrapping_sub(lower) << 16);
        let mac = m.gte.mfc2(reg::MAC1);
        dx0 = widescreen::squeeze_x(m, mac);
        dy0 = m.gte.mfc2(reg::MAC2);
        m.gte.mtc2(reg::VXY0, second);
        cop2(m, MVMVA_RT_V0);
        let mac = m.gte.mfc2(reg::MAC1);
        dx1 = widescreen::squeeze_x(m, mac);
        dy1 = m.gte.mfc2(reg::MAC2);
    } else {
        let offset = lh(m, sprite + 24);
        x = x.wrapping_add(widescreen::squeeze_x(m, offset));
        y = y.wrapping_add(lh(m, sprite + 26));
        let half_width = sra(lhu(m, sprite + 20) << 16, 17);
        dx0 = widescreen::squeeze_x(m, half_width);
        dx1 = dx0;
        let height = lhu(m, sprite + 22) << 16;
        dy0 = sra(height, 17);
        dy1 = sra(0u32.wrapping_sub(sra(height, 16)).wrapping_add(1), 1);
    }
    let corners = [
        (8u32, x.wrapping_sub(dx0), y.wrapping_sub(dy0)),
        (16, x.wrapping_add(dx1), y.wrapping_add(dy1)),
        (24, x.wrapping_sub(dx1), y.wrapping_sub(dy1)),
        (32, x.wrapping_add(dx0), y.wrapping_add(dy0)),
    ];
    for (offset, cx, cy) in corners {
        m.sh(packet + offset, cx as u16);
        m.sh(packet + offset + 2, cy as u16);
    }
}

/// 800867E4: rotation matrix (halfwords, `out`) from the three 12-bit
/// angles at `angles`, scaled by `scale`, with the sine table and GPF.
fn angle_matrix(m: &mut Machine, angles: u32, scale: u32, out: u32) {
    let cos_of = |m: &mut Machine, angle: u32| lh(m, ((angle & 0xfff) << 2) + SINES + 2);
    let sin_of = |m: &mut Machine, angle: u32| lh(m, ((angle & 0xfff) << 2) + SINES);
    let ax = lhu(m, angles);
    let ay = lhu(m, angles + 2);
    let cos_x = cos_of(m, ax);
    let sin_x = sin_of(m, ax);
    let cos_y = cos_of(m, ay);
    m.gte.mtc2(reg::IR0, scale);
    m.gte.mtc2(reg::IR1, sin_x);
    m.gte.mtc2(reg::IR2, cos_x);
    m.gte.mtc2(reg::IR3, cos_y);
    let sin_y = sin_of(m, ay);
    cop2(m, GPF);
    let az = lhu(m, angles + 4);
    let cos_z = cos_of(m, az);
    let sin_z = sin_of(m, az);
    let s_sin_x = m.gte.mfc2(reg::MAC1);
    let s_cos_x = m.gte.mfc2(reg::MAC2);
    let s_cos_y = m.gte.mfc2(reg::MAC3);
    m.gte.mtc2(reg::IR0, s_cos_x);
    m.gte.mtc2(reg::IR1, sin_y);
    m.gte.mtc2(reg::IR2, sin_z);
    m.gte.mtc2(reg::IR3, cos_z);
    m.sh(out + 10, 0u32.wrapping_sub(s_sin_x) as u16);
    cop2(m, GPF);
    let scaled_sin_y = sra(mul_lo(scale, sin_y), 12);
    let a = m.gte.mfc2(reg::MAC1);
    let b = m.gte.mfc2(reg::MAC2);
    let c = m.gte.mfc2(reg::MAC3);
    m.gte.mtc2(reg::IR0, s_cos_y);
    m.gte.mtc2(reg::IR1, sin_z);
    m.gte.mtc2(reg::IR2, cos_z);
    m.gte.mtc2(reg::IR3, cos_x);
    m.sh(out + 4, a as u16);
    m.sh(out + 6, b as u16);
    cop2(m, GPF);
    m.sh(out + 8, c as u16);
    let d = m.gte.mfc2(reg::MAC1);
    let e = m.gte.mfc2(reg::MAC2);
    let f = m.gte.mfc2(reg::MAC3);
    m.gte.mtc2(reg::IR0, scaled_sin_y);
    m.gte.mtc2(reg::IR1, cos_z);
    m.gte.mtc2(reg::IR2, sin_z);
    m.sh(out + 16, f as u16);
    cop2(m, GPF);
    let g = m.gte.mfc2(reg::MAC1);
    let h = m.gte.mfc2(reg::MAC2);
    m.gte.mtc2(reg::IR0, sin_x);
    m.gte.mtc2(reg::IR1, g);
    m.gte.mtc2(reg::IR2, d);
    m.gte.mtc2(reg::IR3, e);
    cop2(m, GPF);
    let i = m.gte.mfc2(reg::MAC1);
    let j = m.gte.mfc2(reg::MAC2);
    let k = m.gte.mfc2(reg::MAC3);
    m.gte.mtc2(reg::IR0, sin_x);
    m.gte.mtc2(reg::IR1, h);
    m.sh(out + 2, i.wrapping_sub(d) as u16);
    cop2(m, GPF);
    m.sh(out + 12, j.wrapping_sub(g) as u16);
    m.sh(out + 14, h.wrapping_add(k) as u16);
    let l = m.gte.mfc2(reg::MAC1);
    m.sh(out, e.wrapping_add(l) as u16);
}

/// 80086948: rotation registers = rotation x the halfword matrix at
/// `matrix` (column by column with MVMVA).
fn multiply_rotation(m: &mut Machine, matrix: u32) {
    let column = |m: &mut Machine, offset: u32| {
        let a = lh(m, matrix + offset);
        let b = lh(m, matrix + offset + 6);
        let c = lh(m, matrix + offset + 12);
        (a, b, c)
    };
    let (a, b, c) = column(m, 0);
    m.gte.mtc2(reg::IR1, a);
    m.gte.mtc2(reg::IR2, b);
    m.gte.mtc2(reg::IR3, c);
    cop2(m, MVMVA_RT_IR);
    let x0 = m.gte.mfc2(reg::IR1);
    let y0 = m.gte.mfc2(reg::IR2);
    let z0 = m.gte.mfc2(reg::IR3);
    let (a, b, c) = column(m, 2);
    m.gte.mtc2(reg::IR1, a);
    m.gte.mtc2(reg::IR2, b);
    m.gte.mtc2(reg::IR3, c);
    cop2(m, MVMVA_RT_IR);
    let x1 = m.gte.mfc2(reg::IR1);
    let y1 = m.gte.mfc2(reg::IR2);
    let z1 = m.gte.mfc2(reg::IR3);
    let (a, b, c) = column(m, 4);
    m.gte.mtc2(reg::IR1, a);
    m.gte.mtc2(reg::IR2, b);
    m.gte.mtc2(reg::IR3, c);
    cop2(m, MVMVA_RT_IR);
    let x2 = m.gte.mfc2(reg::IR1);
    let y2 = m.gte.mfc2(reg::IR2);
    let z2 = m.gte.mfc2(reg::IR3);
    m.gte.ctc2(ctrl::R11R12, (x0 & 0xffff) | (x1 << 16));
    m.gte.ctc2(ctrl::R13R21, (y0 << 16) | (x2 & 0xffff));
    m.gte.ctc2(ctrl::R22R23, (y1 & 0xffff) | (y2 << 16));
    m.gte.ctc2(ctrl::R31R32, (z0 & 0xffff) | (z1 << 16));
    m.gte.ctc2(ctrl::R33, z2);
}

/// 80090210: in the external views, tint the third light colour column by
/// the lock state (red to white per state, scaled by the target
/// strength `level`) and store the player's light direction (entity 0's
/// matrix x (0, -100, 3896)) at gameplay +120; the view matrix is kept.
fn tint_lock_light(m: &mut Machine, level: u32, state: u32, work: u32) {
    m.push_frame(0x28);
    if (level as i32) >= 0 && state < 6 {
        let saved = work + 80;
        save_rotation(m, saved);
        for (i, register) in [ctrl::TRX, ctrl::TRY, ctrl::TRZ].into_iter().enumerate() {
            let value = m.gte.cfc2(register);
            m.sw(saved + 20 + i as u32 * 4, value);
        }
        let player_matrix = work + 112;
        let player = entity(m, 0);
        shared::f_80030534(m, [player + 24, player_matrix, 0, 0]);
        load_rotation(m, player_matrix);
        m.gte.mtc2(reg::VXY0, 0xff9c_0000);
        m.gte.mtc2(reg::VZ0, 3896);
        cop2(m, MVMVA_RT_V0);
        let x = m.gte.mfc2(reg::MAC1);
        let y = m.gte.mfc2(reg::MAC2);
        let z = m.gte.mfc2(reg::MAC3);
        let gameplay = m.gw(GAMEPLAY);
        m.sh(gameplay + 120, x as u16);
        m.sh(gameplay + 122, y as u16);
        m.sh(gameplay + 124, z as u16);
        load_rotation(m, saved);
        // Jump table 800248C8.
        let (r, g, b, shift) = match state {
            0 | 1 => (2560, 1536, 768, 11),
            2 => (3584, 2048, 512, 11),
            3 => (2048, 1536, 2560, 10),
            4 => (512, 1024, 3584, 10),
            _ => (512, 3072, 512, 8),
        };
        m.gte.mtc2(reg::IR1, r);
        m.gte.mtc2(reg::IR2, g);
        m.gte.mtc2(reg::IR3, b);
        m.gte.mtc2(reg::IR0, level << shift);
        for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
            m.gte.ctc2(register, 0);
        }
        let lr3_lg1 = m.gte.cfc2(ctrl::LR1LR2 + 1);
        let lg2_lg3 = m.gte.cfc2(ctrl::LR1LR2 + 2);
        let high = sra(lr3_lg1, 16) << 16;
        cop2(m, INTPL);
        let low = lg2_lg3 & 0xffff;
        let red = m.gte.mfc2(reg::MAC1);
        let green = m.gte.mfc2(reg::MAC2);
        let blue = m.gte.mfc2(reg::MAC3);
        m.gte.ctc2(ctrl::LR1LR2 + 1, high | (red & 0xffff));
        m.gte.ctc2(ctrl::LR1LR2 + 2, low | (green << 16));
        m.gte.ctc2(ctrl::LB3, blue);
    }
    m.pop_frame(0x28);
}

// JP-REVIEW(R0373): near function: PAL 8008E73C -> JP 8008FFAC, imm:3, 3 instructions differ (whole routine)
/// 8008FFAC: screen overlay toggles (context +946 bits, buttons at +96):
/// bit 0 draws the two-tile full-screen blend (8008F950); bits 1 and 2
/// raise the flare state and bit 1 also draws the noise overlay
/// (8008488C).
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_overlays` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_overlays(m: &mut Machine, work: u32) {
    crate::jp::res_p2_render::draw_overlays(m, work)
}

// JP-REVIEW(R0374): near function: PAL 8008E0E0 -> JP 8008F950, imm:1, 1 instructions differ (whole routine)
/// 8008F950: full-screen blend: a draw-mode packet, an optional offset
/// (from `rect`), two 256-wide semi-transparent sprites (colour 0x838383,
/// subtractive) over the display buffer, chained into `slot`. Returns the
/// new cursor.
// JP-PATCH: replaced by `crate::jp::res_p2_render::screen_blend` (src/jp/mod.rs REPLACES, PATCHES.md)
fn screen_blend(m: &mut Machine, slot: u32, packet: u32, rect: u32) -> u32 {
    crate::jp::res_p2_render::screen_blend(m, slot, packet, rect)
}

// ---- projectiles (8008700C's renderers) ---------------------------------------------------------
//
// Both run on 8008700C's scratchpad stack; they take no stack addresses, so
// their register-save frames are not modelled.

// JP-REVIEW(R0376): near function: PAL 80084BB0 -> JP 80086320, imm:1, 1 instructions differ (whole routine)
/// 80086320: projectile type 0 (tracer): projected; tiny ones become a
/// TILE_1 dot (bright or dim by the noise bits), larger ones a gouraud
/// triangle (POLY_F3, or POLY_FT3 above size 6) of three points spun by
/// the projectile's noise word, coloured by facing.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_tracer` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_tracer(m: &mut Machine, projectile: u32, work: u32, saved: u32) {
    crate::jp::res_p2_render::draw_tracer(m, projectile, work, saved)
}

/// One store of a decoded vertex: the halfword offset and its field decoder.
type FieldStore = (u32, fn(u32) -> u32);

// JP-REVIEW(R0382): near function: PAL 8008529C -> JP 80086A0C, imm:1, 1 instructions differ (whole routine)
/// 80086A0C: projectile types 1.. (missiles): an exhaust flame of up to 15
/// POLY_G4 segments built from packed random vertices (object system block
/// +1056 and noise table), depth-cued by distance from the type colour
/// table 800B43AC, scaled by the projectile's size; chained into the OT at
/// its depth.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_exhaust` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_exhaust(m: &mut Machine, projectile: u32, work: u32, saved: u32) {
    crate::jp::res_p2_render::draw_exhaust(m, projectile, work, saved)
}

/// 80078D70: the orbiting chase camera (mode 32): the camera swings out
/// behind the followed object (+52) over the first 32 frames (timer +208),
/// pitching down and yawing around it, damping the roll, at a distance
/// (+228) that grows to 0xAEFFFF; the offset from the object goes to +56
/// (position) and +72 (angles).
fn orbit_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x60);
    if context_word(m, 0) & 0x2000_0000 == 0 {
        let timer = m.lw(camera + 208);
        let mut ease = timer << 7;
        if !slt(ease, 4097) {
            ease = 4096;
        }
        if timer == 0 {
            let start = 0x0019_0000u32.wrapping_sub(lh(m, camera + 48) << 6);
            let yaw = m.lhu(camera + 22);
            let roll = m.lhu(camera + 24);
            m.sw(camera + 228, start);
            m.sh(camera + 214, yaw);
            m.sh(camera + 216, roll);
        }
        let distance = m.lw(camera + 228);
        if !slt(0x00ae_ffff, distance) {
            let mut step = ((ease << 2).wrapping_add(ease)) << 2;
            if (step as i32) < 0 {
                step = step.wrapping_add(4095);
            }
            m.sw(camera + 228, distance.wrapping_add(sra(step, 12) << 12));
        }
        let turn = |ease: u32| {
            let value = if (ease as i32) < 0 {
                ease.wrapping_add(127)
            } else {
                ease
            };
            sra(value, 7)
        };
        if !slt(lh(m, camera + 212), (-2047i32) as u32) {
            let pitch = lhu(m, camera + 212).wrapping_sub(turn(ease));
            m.sh(camera + 212, pitch as u16);
        }
        if !slt(lh(m, camera + 216), 17) {
            let roll = lhu(m, camera + 216).wrapping_sub(16);
            m.sh(camera + 216, roll as u16);
        }
        if slt(lh(m, camera + 216), (-16i32) as u32) {
            let roll = lhu(m, camera + 216).wrapping_add(16);
            m.sh(camera + 216, roll as u16);
        }
        if slt(abs(lh(m, camera + 216)), 16) {
            m.sh(camera + 216, 0);
        }
        let yaw = lhu(m, camera + 214).wrapping_add(turn(ease));
        m.sh(camera + 214, yaw as u16);
        if !slt(sra(yaw << 16, 16), 8193) {
            m.sh(camera + 214, yaw.wrapping_sub(16384) as u16);
        }
        let timer = m.lw(camera + 208);
        let object = m.lw(camera + 52);
        let pitch = lh(m, camera + 212);
        let yaw = lh(m, camera + 214);
        let roll = lh(m, camera + 216);
        m.sw(camera + 208, timer.wrapping_add(1));
        let object_pitch = lh(m, object + 16);
        let object_yaw = lh(m, object + 18);
        let object_roll = lh(m, object + 20);
        m.sh(frame + 48, 0);
        m.sh(frame + 50, 0);
        let back = sra(0u32.wrapping_sub(m.lw(camera + 228)), 9);
        m.sh(frame + 52, back as u16);
        shared::f_80030534(m, [camera + 212, frame + 16, 0, 0]);
        load_rotation(m, frame + 16);
        lwc2(m, reg::VXY0, frame + 48);
        lwc2(m, reg::VZ0, frame + 52);
        cop2(m, MVMVA_RT_V0);
        store_mac123(m, frame + 56);
        for (i, offset) in [56u32, 60].into_iter().enumerate() {
            let value = m.lw(frame + 56 + i as u32 * 4) << 9;
            m.sw(camera + offset, value);
        }
        let z = m.lw(frame + 64);
        m.sh(camera + 72, pitch.wrapping_sub(object_pitch) as u16);
        m.sh(camera + 74, yaw.wrapping_sub(object_yaw) as u16);
        m.sh(camera + 76, roll.wrapping_sub(object_roll) as u16);
        m.sw(camera + 64, z << 9);
    }
    m.pop_frame(0x60);
}

// ==== camera mode 512: the scripted replay camera (8007AFFC) ==================================

/// `div` at `pc` with the compiler's checks: `break 7` at `pc + 0xC` for a
/// zero divisor and `break 6` at `pc + 0x24` for `0x80000000 / -1`, both
/// raised as `Fault::Trap`. Returns `lo`.
fn div_checked(m: &mut Machine, numerator: u32, divisor: u32, pc: u32) -> u32 {
    if divisor == 0 {
        m.raise(Fault::Trap {
            address: pc + 0xc,
            code: 7,
        });
    } else if numerator == 0x8000_0000 && divisor == u32::MAX {
        m.raise(Fault::Trap {
            address: pc + 0x24,
            code: 6,
        });
    }
    div_lo(numerator, divisor)
}

/// Entry `index` of the 1.14 sine table 800104AC, sign-extended.
fn sine14(m: &mut Machine, index: u32) -> u32 {
    lh(m, shared::SINE_TABLE + (index << 1))
}

/// The eased step shared by 8007A3B4 and 8007AC50: the sine table entry
/// at `((2 * t - 4096) & 0x3FFF)`, as a halfword shifted right by three,
/// plus 2048 (0..4096 over t = 0..4096).
fn ease(m: &mut Machine, t: u32) -> u32 {
    let index = ((t << 1).wrapping_sub(4096)) & 0x3fff;
    let value = lhu(m, shared::SINE_TABLE + (index << 1));
    sra(value << 16, 19).wrapping_add(2048)
}

// JP-REVIEW(R0384): changed function: PAL 8007998C -> JP 8007AFFC, del:20 imm:1 ins:29 op:13 reg:16, 79 instructions differ (whole routine)
/// 8007AFFC: camera mode 512, the replay camera. The shot block at
/// `sp+0x30` names the subject (+0x10C or +0x134 objects, swapped when the
/// camera's side +0x15C allows it), the reference (+0x124 or +0x14C) and
/// the camera's object (+0x2C). When no shot runs (+0xCC = 0) a new one
/// starts: the camera is marked cut (8007786C), its pose saved at +0xF4,
/// and a script chosen from the tables 800B40AC/800B40DC/800B40EC/`gp+0x2B4`
/// by the held buttons or the random sequence 800B4104; `[gp+0x534]` keeps
/// it. Each frame the script (`jalr`, state at +0x160) poses +0x164, which
/// is used directly or eased into +0xF4 (8007AE40) and copied to the pose.
// JP-PATCH: replaced by `crate::jp::res_p2_render::replay_camera` (src/jp/mod.rs REPLACES, PATCHES.md)
fn replay_camera(m: &mut Machine, camera: u32) {
    crate::jp::res_p2_render::replay_camera(m, camera)
}

/// 8007AE40: ease the pose `current` (+0..+8 position, +0x10..+0x14
/// angles) toward `target`: each angle moves by its distance / 16 (/ 8 for
/// the second, which with the third is first wrapped to within half a turn
/// of the target); an angle closer than its step is set to the step (as
/// the original does). The position is copied.
fn ease_pose(m: &mut Machine, target: u32, current: u32) {
    let mut angles = [
        lh(m, current + 0x10),
        lh(m, current + 0x12),
        lh(m, current + 0x14),
    ];
    let goals = [
        lh(m, target + 0x10),
        lh(m, target + 0x12),
        lh(m, target + 0x14),
    ];
    for i in [1, 2] {
        if slt(angles[i].wrapping_sub(goals[i]), 0xffff_e000) {
            angles[i] = angles[i].wrapping_add(0x4000);
        }
        if !slt(angles[i].wrapping_sub(goals[i]), 0x2001) {
            angles[i] = angles[i].wrapping_sub(0x4000);
        }
    }
    let shifts = [4u32, 3, 4];
    let mut steps = [0u32; 3];
    for i in 0..3 {
        steps[i] = sra(
            abs(angles[i].wrapping_sub(goals[i])).wrapping_add(1),
            shifts[i],
        );
    }
    for i in 0..3 {
        if slt(angles[i], goals[i]) {
            angles[i] = angles[i].wrapping_add(steps[i]);
        }
        if slt(goals[i], angles[i]) {
            angles[i] = angles[i].wrapping_sub(steps[i]);
        }
    }
    for i in 0..3 {
        if slt(abs(angles[i].wrapping_sub(goals[i])), steps[i]) {
            angles[i] = steps[i];
        }
    }
    for (i, angle) in (0u32..).zip(angles) {
        m.sh(current + 0x10 + i * 2, angle as u16);
    }
    for offset in [0u32, 4, 8] {
        let value = m.lw(target + offset);
        m.sw(current + offset, value);
    }
}

/// 8007867C: manual look in the replay camera: the stick (context +0x138
/// bits 0x10/0x20: +0x14C/+0x150) and the buttons held at +0x128 give
/// target angles; +0xB4..+0xB8 approach them by a quarter of the distance
/// (snapping when closer), or jump there with a cut (8007786C) on the
/// buttons at +0x12C/+0x134 bit 0x100000. Then 800313A8 adds them to the
/// view angles at +0x14.
fn manual_look(m: &mut Machine, camera: u32) {
    m.push_frame(0x18);
    let context = game_context(m);
    let stick = m.lw(context + 0x138);
    let mut pitch = 0u32;
    let mut yaw = 0u32;
    let roll = 0u32;
    if stick & 0x10 != 0 {
        pitch = 0u32.wrapping_sub(m.lw(context + 0x14c));
    }
    if stick & 0x20 != 0 {
        yaw = m.lw(context + 0x150);
    }
    let held = m.lw(context + 0x128);
    if held & 0x10_0000 != 0 {
        yaw = yaw.wrapping_add(0x2000);
    }
    if held & 0x1_0000 != 0 {
        pitch = pitch.wrapping_add(0x1000);
    }
    if held & 0x2_0000 != 0 {
        pitch = pitch.wrapping_sub(0x1000);
    }
    if held & 0x4_0000 != 0 {
        yaw = yaw.wrapping_sub(0x1000);
    }
    if held & 0x8_0000 != 0 {
        yaw = yaw.wrapping_add(0x1000);
    }
    if slt(lh(m, camera + 0xb6).wrapping_sub(yaw), 0xffff_e000) {
        let value = lhu(m, camera + 0xb6).wrapping_add(0x4000);
        m.sh(camera + 0xb6, value as u16);
    }
    if !slt(lh(m, camera + 0xb6).wrapping_sub(yaw), 0x2001) {
        let value = lhu(m, camera + 0xb6).wrapping_sub(0x4000);
        m.sh(camera + 0xb6, value as u16);
    }
    let context = game_context(m);
    let jump = (m.lw(context + 0x12c) | m.lw(context + 0x134)) & 0x10_0000 != 0;
    let goals = [pitch, yaw, roll];
    if jump {
        for (i, goal) in (0u32..).zip(goals) {
            m.sh(camera + 0xb4 + i * 2, goal as u16);
        }
        mark_camera_cut(m, camera);
    } else {
        let mut steps = [0u32; 3];
        for (i, step) in (0u32..).zip(steps.iter_mut()) {
            let angle = lh(m, camera + 0xb4 + i * 2);
            *step = sra(
                abs(angle.wrapping_sub(goals[i as usize])).wrapping_add(2),
                2,
            );
        }
        for (i, goal) in (0u32..).zip(goals) {
            let slot = camera + 0xb4 + i * 2;
            let step = steps[i as usize];
            if slt(lh(m, slot), goal) {
                let value = lhu(m, slot).wrapping_add(step);
                m.sh(slot, value as u16);
            }
            if slt(goal, lh(m, slot)) {
                let value = lhu(m, slot).wrapping_sub(step);
                m.sh(slot, value as u16);
            }
        }
        for (i, goal) in (0u32..).zip(goals) {
            let slot = camera + 0xb4 + i * 2;
            if slt(abs(lh(m, slot).wrapping_sub(goal)), steps[i as usize]) {
                m.sh(slot, goal as u16);
            }
        }
    }
    view_angles(m, camera + 0x14, camera + 0xb4, camera + 0x14);
    m.pop_frame(0x18);
}

/// 80079B1C: terrain height under `position` (x +0, z +8): tile
/// `[gameplay+0x248]` at (x >> 23, z >> 23) + 128 (folded into 0..255);
/// flat tiles (low 9 bits zero) give 0 and the up normal (0, -4096, 0) at
/// `normal`. Otherwise a vertical ray from y = 0xFF448000 to 0 through
/// 8009A5D0 gives the hit height, the surface normal (`[800BD0C0]`) and
/// surface word (`[800BD0C4]`); no hit gives 0. `normal`/`surface` may be 0.
fn ground_height(m: &mut Machine, position: u32, normal: u32, surface: u32) -> u32 {
    let frame = m.push_frame(0x50);
    let x = m.lw(position);
    let mut column = sra(x, 23).wrapping_add(0x80);
    let mut row = sra(m.lw(position + 8), 23).wrapping_add(0x80);
    let gameplay = m.gw(GAMEPLAY);
    let tiles = m.lw(gameplay + 0x248);
    if slt(column, 0) {
        column &= 7;
    }
    if slt(row, 0) {
        row &= 7;
    }
    if !slt(column, 0x100) {
        column = (column & 7) + 0xf8;
    }
    if !slt(row, 0x100) {
        row = (row & 7) + 0xf8;
    }
    let tile = lhu(
        m,
        ((column.wrapping_add(row << 8)) << 1).wrapping_add(tiles),
    );
    let mut height = 0;
    let up = |m: &mut Machine| {
        if normal != 0 {
            m.sh(normal, 0);
            m.sh(normal + 2, 0xf000);
            m.sh(normal + 4, 0);
        }
    };
    if tile & 0x1ff == 0 {
        up(m);
    } else {
        m.sw(frame + 0x10, x);
        m.sw(frame + 0x14, 0xff44_8000);
        let z = m.lw(position + 8);
        m.sw(frame + 0x18, z);
        let x = m.lw(position);
        m.sw(frame + 0x24, 0);
        m.sw(frame + 0x20, x);
        let z = m.lw(position + 8);
        m.sw(frame + 0x28, z);
        let hit = dispatch::call(
            m,
            0x8009_a5d0,
            [frame + 0x10, frame + 0x20, frame + 0x30, SCRATCH],
        );
        if slt(0x7fff_fffe, hit) {
            up(m);
            if surface != 0 {
                m.sw(surface, 0);
            }
        } else {
            if normal != 0 {
                let source = m.lw(0x800b_d0c0);
                for offset in [0u32, 2, 4] {
                    let value = m.lhu(source + offset);
                    m.sh(normal + offset, value);
                }
            }
            if surface != 0 {
                let value = m.lw(0x800b_d0c4);
                m.sw(surface, value);
            }
            height = m.lw(frame + 0x34);
        }
    }
    m.pop_frame(0x50);
    height
}

/// 80079CB0: how far the position (x, y, z words) must move up (negative
/// y) to stay 0x84000 above the ground (80079B1C); 0 when it already does.
fn ground_clearance(m: &mut Machine, position: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let ground = ground_height(m, position, frame + 0x10, frame + 0x18);
    let lift = ground
        .wrapping_add(0xfff7_c000)
        .wrapping_sub(m.lw(position + 4));
    m.pop_frame(0x28);
    if slt(lift, 0) { lift } else { 0 }
}

/// 80079CFC: clamp the word vector `position` into the box `low..=high`.
fn clamp_to_box(m: &mut Machine, low: u32, high: u32, position: u32) {
    for offset in [0u32, 4, 8] {
        let bound = m.lw(low + offset);
        if slt(m.lw(position + offset), bound) {
            m.sw(position + offset, bound);
        }
    }
    for offset in [0u32, 4, 8] {
        let bound = m.lw(high + offset);
        if slt(bound, m.lw(position + offset)) {
            m.sw(position + offset, bound);
        }
    }
}

// ---- shot geometry helpers --------------------------------------------------------------------

/// 80031CD8: angles (pitch, yaw, 0) of the word vector `vector` (>> 12):
/// pitch from atan2(-y, horizontal length), yaw from atan2(x, z), folded so
/// |pitch| <= 0x1000 and the yaw stays within half a turn.
fn direction_angles(m: &mut Machine, vector: u32, out: u32) {
    m.push_frame(0x28);
    let wx = m.lw(vector);
    let wy = m.lw(vector + 4);
    let wz = m.lw(vector + 8);
    let x = sra(wx, 12);
    let y = sra(wy, 12);
    let z = sra(wz, 12);
    let horizontal = abs(x).wrapping_add(abs(z));
    let planar = if slt(0xa_ffff, horizontal) {
        0x10_0000
    } else {
        let (a, c, shift) = if slt(0xb000, horizontal) {
            (sra(wx, 16), sra(wz, 16), 4)
        } else {
            (x, z, 0)
        };
        sllv(vector_length(m, a, 0, c), shift)
    };
    let mut pitch = shared::f_80030b10(m, [0u32.wrapping_sub(y), planar, 0, 0]);
    let mut yaw = shared::f_80030b10(m, [x, z, 0, 0]);
    if !slt(pitch, 4097) {
        pitch = 8192u32.wrapping_sub(pitch);
        yaw = yaw.wrapping_add(8192);
    }
    if slt(pitch, 0xffff_f000) {
        pitch = 0xffff_e000u32.wrapping_sub(pitch);
        yaw = yaw.wrapping_add(8192);
    }
    if !slt(yaw, 8192) {
        yaw = yaw.wrapping_sub(16384);
    }
    if slt(yaw, 0xffff_e000) {
        yaw = yaw.wrapping_add(16384);
    }
    m.sh(out, pitch as u16);
    m.sh(out + 2, yaw as u16);
    m.sh(out + 4, 0);
    m.pop_frame(0x28);
}

/// 8007995C: `out` = the subject (shot +0) position plus the third
/// object's (shot +8) position >> 5 scaled by `factor` (GPF, no shift).
fn toward_reference(m: &mut Machine, shot: u32, factor: u32, out: u32) {
    let reference = m.lw(shot + 8);
    let x = sra(m.lw(reference), 5);
    let y = sra(m.lw(reference + 4), 5);
    let z = sra(m.lw(reference + 8), 5);
    let subject = m.lw(shot);
    m.gte.mtc2(reg::IR0, factor);
    m.gte.mtc2(reg::IR1, x);
    m.gte.mtc2(reg::IR2, y);
    m.gte.mtc2(reg::IR3, z);
    cop2(m, GPF_NO_SHIFT);
    let sy = m.lw(subject + 4);
    let sz = m.lw(subject + 8);
    let sx = m.lw(subject);
    let mac1 = m.gte.mfc2(reg::MAC1);
    let mac2 = m.gte.mfc2(reg::MAC2);
    let mac3 = m.gte.mfc2(reg::MAC3);
    m.sw(out + 4, mac2.wrapping_add(sy));
    m.sw(out + 8, mac3.wrapping_add(sz));
    m.sw(out, mac1.wrapping_add(sx));
}

/// GPF sf=0.
const GPF_NO_SHIFT: u32 = 0x190_003d;

/// 800799D4: `out` = the subject position plus (0, 0, `distance` >> 5)
/// rotated by the subject's angles (+16, 80030778) and scaled by `factor`
/// (GPF, no shift).
fn behind_subject(m: &mut Machine, shot: u32, distance: u32, factor: u32, out: u32) {
    let frame = m.push_frame(0x60);
    let subject = m.lw(shot);
    shared::f_80030534(m, [subject + 16, frame + 0x20, 0, 0]);
    load_rotation(m, frame + 0x20);
    let vector = frame + 0x40;
    m.sh(vector, 0);
    m.sh(vector + 2, 0);
    m.sh(vector + 4, sra(distance, 5) as u16);
    lwc2(m, reg::VXY0, vector);
    lwc2(m, reg::VZ0, vector + 4);
    cop2(m, MVMVA_RT_V0);
    store_mac123(m, frame + 0x10);
    let x = m.lw(frame + 0x10);
    let y = m.lw(frame + 0x14);
    let z = m.lw(frame + 0x18);
    m.gte.mtc2(reg::IR0, factor);
    m.gte.mtc2(reg::IR1, x);
    m.gte.mtc2(reg::IR2, y);
    m.gte.mtc2(reg::IR3, z);
    cop2(m, GPF_NO_SHIFT);
    let sx = m.lw(subject);
    let sy = m.lw(subject + 4);
    let sz = m.lw(subject + 8);
    let mac1 = m.gte.mfc2(reg::MAC1);
    let mac2 = m.gte.mfc2(reg::MAC2);
    let mac3 = m.gte.mfc2(reg::MAC3);
    m.sw(out, mac1.wrapping_add(sx));
    m.sw(out + 4, mac2.wrapping_add(sy));
    m.sw(out + 8, mac3.wrapping_add(sz));
    m.pop_frame(0x60);
}

/// 80079AE8: the length of the third object's position (80083F40) x 8.
fn reference_distance(m: &mut Machine, shot: u32) -> u32 {
    m.push_frame(0x18);
    let reference = m.lw(shot + 8);
    let x = m.lw(reference);
    let y = m.lw(reference + 4);
    let z = m.lw(reference + 8);
    let length = vector_length(m, x, y, z);
    m.pop_frame(0x18);
    length << 3
}

/// 80079DAC: `out` = the subject position plus the offset vector of the
/// shot's fourth pointer (+0xC, halfwords) plus `offsets` >> 6, rotated
/// into the subject-to-target direction (80031CD8 into `out` +0x10,
/// 80030778) and scaled by 64.
fn orbit_point(m: &mut Machine, shot: u32, offsets: u32, out: u32) {
    let frame = m.push_frame(0x60);
    let difference = frame + 0x38;
    for offset in [0u32, 4, 8] {
        let target = m.lw(shot + 4);
        let subject = m.lw(shot);
        let value = m.lw(target + offset).wrapping_sub(m.lw(subject + offset));
        m.sw(difference + offset, value);
    }
    direction_angles(m, difference, out + 0x10);
    shared::f_80030534(m, [out + 0x10, frame + 0x10, 0, 0]);
    load_rotation(m, frame + 0x10);
    let ox = sra(m.lw(offsets), 6);
    let oy = sra(m.lw(offsets + 4), 6);
    let oz = sra(m.lw(offsets + 8), 6);
    let base = m.lw(shot + 0xc);
    let vector = frame + 0x30;
    for (i, delta) in (0u32..).zip([ox, oy, oz]) {
        let value = lh(m, base + i * 2).wrapping_add(delta);
        m.sh(vector + i * 2, value as u16);
    }
    lwc2(m, reg::VXY0, vector);
    lwc2(m, reg::VZ0, vector + 4);
    cop2(m, MVMVA_RT_V0);
    store_mac123(m, difference);
    let subject = m.lw(shot);
    for offset in [0u32, 4, 8] {
        let value = m.lw(difference + offset) << 6;
        let base = m.lw(subject + offset);
        m.sw(out + offset, value.wrapping_add(base));
    }
    m.pop_frame(0x60);
}

/// 80079F2C: `out` = the subject position plus the vector (0, 0, z)
/// rotated by `angles` (80030778) and scaled by 4096, where z is the
/// offset z times `scale`, shifted right by 18 and clamped to 16 bits;
/// `out` +0x10 = the angles.
fn swing_point(m: &mut Machine, shot: u32, scale: u32, angles: u32, out: u32) {
    let frame = m.push_frame(0x58);
    let base = m.lw(shot + 0xc);
    let mut z = sra(mul_lo(lh(m, base + 4), scale), 18);
    if slt(0x7fff, z) {
        z = 0x7fff;
    }
    if slt(z, 0xffff_8000) {
        z = 0xffff_8000;
    }
    let vector = frame + 0x30;
    m.sh(vector, 0);
    m.sh(vector + 2, 0);
    m.sh(vector + 4, z as u16);
    shared::f_80030534(m, [angles, frame + 0x10, 0, 0]);
    load_rotation(m, frame + 0x10);
    lwc2(m, reg::VXY0, vector);
    lwc2(m, reg::VZ0, vector + 4);
    cop2(m, MVMVA_RT_V0);
    store_mac123(m, frame + 0x38);
    let subject = m.lw(shot);
    for offset in [0u32, 2, 4] {
        let value = m.lhu(angles + offset);
        m.sh(out + 0x10 + offset, value);
    }
    for offset in [0u32, 4, 8] {
        let value = m.lw(frame + 0x38 + offset) << 12;
        let base = m.lw(subject + offset);
        m.sw(out + offset, base.wrapping_add(value));
    }
    m.pop_frame(0x58);
}

/// 8007A060: aim from `from` at the subject: `out` +0x10 = the angles of
/// subject - from (80031CD8), `out` position = `from`.
fn aim_at_subject(m: &mut Machine, shot: u32, from: u32, out: u32) {
    let frame = m.push_frame(0x30);
    let subject = m.lw(shot);
    for offset in [0u32, 4, 8] {
        let value = m.lw(subject + offset).wrapping_sub(m.lw(from + offset));
        m.sw(frame + 0x10 + offset, value);
    }
    direction_angles(m, frame + 0x10, out + 0x10);
    for offset in [0u32, 4, 8] {
        let value = m.lw(from + offset);
        m.sw(out + offset, value);
    }
    m.pop_frame(0x30);
}

/// The roll of the scripts with flag bit 8 (state +0x24): the subject's
/// roll (+0x14), negated unless bit 4 is set.
fn follow_roll(m: &mut Machine, shot: u32, state: u32) {
    let flags = m.lw(state + 0x24);
    if flags & 8 != 0 {
        let subject = m.lw(shot);
        let roll = lhu(m, subject + 0x14);
        let roll = if flags & 4 != 0 {
            roll
        } else {
            0u32.wrapping_sub(roll)
        };
        m.sh(state + 0x18, roll as u16);
    }
}

/// The common script tail: advance the timer (+0x1C); at the duration
/// (+0x20) report completion when the shot has a `limit`, else restart.
fn advance_shot(m: &mut Machine, state: u32, limit: u32) -> u32 {
    let timer = m.lw(state + 0x1c).wrapping_add(1);
    m.sw(state + 0x1c, timer);
    if slt(timer, m.lw(state + 0x20)) {
        return 0;
    }
    if slt(0, limit) {
        1
    } else {
        m.sw(state, 0);
        0
    }
}

/// Duration (+0x20): `limit` when positive, else `default`.
fn shot_duration(m: &mut Machine, state: u32, limit: u32, default: u32) {
    m.sw(state + 0x20, if slt(0, limit) { limit } else { default });
}

/// The reference offset (shot +0xC halfwords << `shift`) added to the
/// state position (+4..+0xC), or subtracted when `subtract`.
fn offset_by_reference(m: &mut Machine, shot: u32, state: u32, shift: u32, subtract: bool) {
    for i in 0..3u32 {
        let base = m.lw(shot + 0xc);
        let delta = lh(m, base + i * 2) << shift;
        let value = m.lw(state + 4 + i * 4);
        let value = if subtract {
            value.wrapping_sub(delta)
        } else {
            value.wrapping_add(delta)
        };
        m.sw(state + 4 + i * 4, value);
    }
}

// ---- replay scripts (tables 800B40AC, 800B40DC, 800B40EC, gp+0x2B4) ---------------------------

/// 8007A0F0: a fixed shot near the subject: the subject's angles, the
/// position toward the reference (8007995C, factor from `limit` or the
/// frame counter) plus the offset vector << 6, held for the duration.
fn shot_fixed_near(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    m.push_frame(0x20);
    let result = match m.lw(state) {
        0 => {
            m.sw(state + 0x1c, 0);
            for offset in [0u32, 2, 4] {
                let subject = m.lw(shot);
                let value = m.lhu(subject + 0x10 + offset);
                m.sh(state + 0x14 + offset, value);
            }
            let (factor, duration) = if slt(0, limit) {
                let factor = if slt(limit, 13) { sra(limit, 1) } else { 12 };
                (factor, limit.wrapping_sub(factor))
            } else {
                let factor = context_word(m, 0x24) & 0xf;
                (factor, factor + 0x32)
            };
            m.sw(state + 0x20, duration);
            toward_reference(m, shot, factor << 5, state + 4);
            offset_by_reference(m, shot, state, 6, false);
            m.sw(state, 1);
            0
        }
        1 => {
            let timer = m.lw(state + 0x1c).wrapping_add(1);
            m.sw(state + 0x1c, timer);
            if slt(timer, m.lw(state + 0x20)) {
                0
            } else if slt(0, limit) {
                1
            } else {
                m.sw(state, 0);
                0
            }
        }
        _ => 0,
    };
    m.pop_frame(0x20);
    result
}

/// 8007A260: a tracking shot from near the subject: as 8007A0F0 (offset
/// << 5, longer default) but aimed at the subject every frame (8007A060).
fn shot_tracking_near(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    m.push_frame(0x28);
    let mut state_value = m.lw(state);
    let mut result = 0;
    if state_value == 0 {
        m.sw(state + 0x1c, 0);
        let (factor, duration) = if slt(0, limit) {
            let factor = if slt(limit, 31) { sra(limit, 1) } else { 30 };
            (factor, limit.wrapping_sub(factor))
        } else {
            let factor = (context_word(m, 0x24) & 0x1f) + 30;
            (factor, factor + 90)
        };
        m.sw(state + 0x20, duration);
        toward_reference(m, shot, factor << 5, state + 4);
        offset_by_reference(m, shot, state, 5, false);
        m.sw(state, 1);
        state_value = 1;
    }
    if state_value == 1 {
        aim_at_subject(m, shot, state + 4, state + 4);
        result = advance_shot(m, state, limit);
    }
    m.pop_frame(0x28);
    result
}

/// 8007A3B4: a fly-by from above: x/z from the subject, height eased (sine
/// table) from the terrain-relative altitude (gameplay +0x28 object +0x28,
/// subject y >> 12, clamped 0..0x7FFF) to 200, looking straight down.
fn shot_overhead(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    let mut state_value = m.lw(state);
    if state_value == 0 {
        m.sw(state + 0x1c, 0);
        shot_duration(m, state, limit, 150);
        m.sw(state, 1);
        state_value = 1;
    }
    if state_value != 1 {
        return 0;
    }
    let numerator = m.lw(state + 0x1c) << 14;
    let duration = m.lw(state + 0x20);
    let mut t = div_checked(m, numerator, duration, 0x8007_a404);
    if !slt(t, 4097) {
        t = 4096;
    }
    let eased = ease(m, t);
    let gameplay = m.gw(GAMEPLAY);
    let subject = m.lw(shot);
    let terrain = m.lw(gameplay + 0x28);
    let level = lh(m, terrain + 0x28);
    let mut height = sra(level.wrapping_sub(1000), 1).wrapping_add(sra(m.lw(subject + 4), 12));
    if slt(height, 0) {
        height = 0;
    }
    if slt(0x7fff, height) {
        height = 0x7fff;
    }
    let product = mul_lo(height, eased);
    let x = m.lw(subject);
    m.sw(state + 4, x);
    let subject = m.lw(shot);
    let lift = sra(product, 12).wrapping_add(200);
    let y = m.lw(subject + 4).wrapping_sub(lift << 12);
    m.sw(state + 8, y);
    let subject = m.lw(shot);
    let z = m.lw(subject + 8);
    let timer = m.lw(state + 0x1c).wrapping_add(1);
    m.sh(state + 0x14, 0xf000);
    m.sh(state + 0x16, 0);
    m.sh(state + 0x18, 0);
    m.sw(state + 0x1c, timer);
    m.sw(state + 0xc, z);
    u32::from(slt(0, limit) && !slt(timer, m.lw(state + 0x20)))
}

/// 8007A52C: a swing around behind the subject (800799D4 at the reference
/// distance, at least 0x2A000, with the factor sweeping through the
/// shot), minus the offset vector << 6, aimed at the subject.
fn shot_swing_behind(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    m.push_frame(0x28);
    let mut state_value = m.lw(state);
    let mut result = 0;
    if state_value == 0 {
        m.sw(state + 0x1c, 0);
        m.sw(state, 1);
        shot_duration(m, state, limit, 450);
        let distance = reference_distance(m, shot);
        m.sw(state + 0x28, distance);
        if !slt(0x2_9fff, distance) {
            m.sw(state + 0x28, 0x2_a000);
        }
        state_value = 1;
    }
    if state_value == 1 {
        let half = sra(m.lw(state + 0x20), 1);
        let progress = m.lw(state + 0x1c).wrapping_sub(half);
        let sweep = ((progress << 4).wrapping_sub(progress)) << 5;
        let factor = div_checked(m, sweep, half, 0x8007_a5dc);
        let distance = m.lw(state + 0x28);
        behind_subject(m, shot, distance, 0u32.wrapping_sub(factor), state + 4);
        offset_by_reference(m, shot, state, 6, true);
        aim_at_subject(m, shot, state + 4, state + 4);
        result = advance_shot(m, state, limit);
    }
    m.pop_frame(0x28);
    result
}

/// 8007A6CC: a swing past the subject: pitch/yaw swept by the shot
/// progress per the context flags (+0x24 byte bits 0x80/0x40, 0x20/0x10)
/// at a distance eased by the sine table (x (flags & 3) + 1), via 80079F2C.
fn shot_swing_past(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let mut state_value = m.lw(state);
    let mut result = 0;
    if state_value == 0 {
        m.sw(state + 0x1c, 0);
        m.sw(state, 1);
        shot_duration(m, state, limit, 320);
        let context = game_context(m);
        let flags = lbu(m, context + 0x24);
        m.sw(state + 0x24, flags);
        state_value = 1;
    }
    if state_value == 1 {
        let half = sra(m.lw(state + 0x20), 1);
        let progress = m.lw(state + 0x1c).wrapping_sub(half) << 12;
        let sweep = div_checked(m, progress, half, 0x8007_a75c);
        let mut flags = m.lw(state + 0x24);
        let wave = sine14(m, (abs(sweep).wrapping_add(4096)) & 0x3fff);
        let scale = mul_lo(16384u32.wrapping_sub(wave), (flags & 3) + 1).wrapping_add(4096);
        let mut pitch = 0u32;
        let mut yaw = 0u32;
        if flags & 0x80 != 0 {
            pitch = sweep;
            if flags & 0x40 == 0 {
                pitch = 0u32.wrapping_sub(pitch);
                flags = m.lw(state + 0x24);
            }
        }
        if flags & 0x20 != 0 {
            yaw = sweep;
            if flags & 0x10 == 0 {
                yaw = 0u32.wrapping_sub(yaw);
            }
        }
        let angles = frame + 0x10;
        m.sh(angles, pitch as u16);
        m.sh(angles + 2, yaw as u16);
        m.sh(angles + 4, 0);
        swing_point(m, shot, scale, angles, state + 4);
        follow_roll(m, shot, state);
        result = advance_shot(m, state, limit);
    }
    m.pop_frame(0x30);
    result
}

/// 8007A8C4: a slow drift behind the subject (800799D4 with factor
/// +-128 per flag bit 1, at the reference distance clamped to at least
/// 0x4000 or 0x31000), minus the offset vector << 6, aimed at the subject.
fn shot_drift_behind(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    m.push_frame(0x28);
    let mut state_value = m.lw(state);
    let mut result = 0;
    if state_value == 0 {
        m.sw(state + 0x1c, 0);
        m.sw(state, 1);
        shot_duration(m, state, limit, 240);
        let distance = reference_distance(m, shot);
        let context = game_context(m);
        m.sw(state + 0x28, distance);
        let flags = m.lw(context + 0x24);
        m.sw(state + 0x24, flags & 0xf);
        if flags & 1 != 0 {
            if slt(m.lw(state + 0x28), 16384) {
                m.sw(state + 0x28, 16384);
            }
        } else if !slt(0x3_0fff, m.lw(state + 0x28)) {
            m.sw(state + 0x28, 0x3_1000);
        }
        state_value = 1;
    }
    if state_value == 1 {
        let factor = if m.lw(state + 0x24) & 1 != 0 {
            128
        } else {
            0xffff_ff80
        };
        let distance = m.lw(state + 0x28);
        behind_subject(m, shot, distance, factor, state + 4);
        offset_by_reference(m, shot, state, 6, true);
        aim_at_subject(m, shot, state + 4, state + 4);
        follow_roll(m, shot, state);
        let timer = m.lw(state + 0x1c).wrapping_add(1);
        m.sw(state + 0x1c, timer);
        result = u32::from(slt(0, limit) && !slt(timer, m.lw(state + 0x20)));
    }
    m.pop_frame(0x28);
    result
}

/// 8007AAB4: a chase shot above and behind the subject (80079DAC with an
/// offset from the subject's pitch and roll sines), for exactly `limit`
/// frames.
fn shot_chase(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    let frame = m.push_frame(0x38);
    let mut state_value = m.lw(state);
    let mut result = 0;
    if state_value == 0 {
        let context = game_context(m);
        m.sw(state + 0x1c, 0);
        m.sw(state, 1);
        let flags = m.lw(context + 0x24);
        m.sw(state + 0x20, limit);
        m.sw(state + 0x28, 0);
        m.sw(state + 0x24, flags & 0xf);
        state_value = 1;
    }
    if state_value == 1 {
        let subject = m.lw(shot);
        let target = m.lw(shot + 4);
        shared::f_800827b4(m, [subject, target, 0, 0]);
        let countdown = m.lw(state + 0x28);
        if countdown != 0 {
            m.sw(state + 0x28, countdown.wrapping_sub(1));
        }
        let subject = m.lw(shot);
        let roll_index = lhu(m, subject + 0x14) & 0x3fff;
        let roll = sine14(m, roll_index);
        let pitch_index = lhu(m, subject + 0x10) & 0x3fff;
        let pitch = sine14(m, pitch_index);
        let offsets = frame + 0x10;
        m.sw(offsets + 8, 0xfffb_0000);
        m.sw(offsets, roll.wrapping_mul(10));
        let lift = sra(pitch.wrapping_mul(15), 1).wrapping_add(0xfffe_c000);
        m.sw(offsets + 4, lift);
        orbit_point(m, shot, offsets, state + 4);
        follow_roll(m, shot, state);
        let timer = m.lw(state + 0x1c).wrapping_add(1);
        m.sw(state + 0x1c, timer);
        result = u32::from(limit != 0 && !slt(timer, m.lw(state + 0x20)));
    }
    m.pop_frame(0x38);
    result
}

/// 8007AC50: a pull-away shot (80079DAC) whose side offset eases out
/// (sine table) in the direction of flag bits 2/1.
fn shot_pull_away(m: &mut Machine, shot: u32, state: u32, limit: u32) -> u32 {
    let frame = m.push_frame(0x38);
    let mut state_value = m.lw(state);
    let mut result = 0;
    if state_value == 0 {
        let context = game_context(m);
        m.sw(state + 0x1c, 0);
        m.sw(state, 1);
        let flags = m.lw(context + 0x24);
        m.sw(state + 0x24, flags & 0xf);
        shot_duration(m, state, limit, 60);
        state_value = 1;
    }
    if state_value == 1 {
        let numerator = m.lw(state + 0x1c) << 12;
        let duration = m.lw(state + 0x20);
        let t = div_checked(m, numerator, duration, 0x8007_acd8);
        let flags = m.lw(state + 0x24);
        let eased = ease(m, 4096u32.wrapping_sub(t));
        let mut side = 0u32;
        if flags & 2 != 0 {
            let swing = ((eased << 4).wrapping_sub(eased)) << 15;
            let swing = if flags & 1 != 0 {
                swing
            } else {
                0u32.wrapping_sub(swing)
            };
            side = sra(swing, 12);
        }
        let offsets = frame + 0x10;
        m.sw(offsets + 8, 0xfffb_0000);
        m.sw(offsets, side);
        m.sw(offsets + 4, sra(0xfffb_0000, 2));
        orbit_point(m, shot, offsets, state + 4);
        follow_roll(m, shot, state);
        let timer = m.lw(state + 0x1c);
        if slt(timer, m.lw(state + 0x20)) {
            m.sw(state + 0x1c, timer.wrapping_add(1));
        }
        if slt(0, limit) && !slt(m.lw(state + 0x1c), m.lw(state + 0x20)) {
            result = 1;
        }
    }
    m.pop_frame(0x38);
    result
}

// ==== debris renderer type 1 (table 800B4400): the shock ring ====================================

/// Clamp a projected depth into the debris ordering slots: `SZ3 >> 3 - 12`
/// into 0..=4094.
fn ring_depth(sz3: u32) -> u32 {
    let depth = sra(sz3, 3).wrapping_sub(12);
    if !slt(depth, 4095) {
        4094
    } else if slt(depth, 0) {
        0
    } else {
        depth
    }
}

/// Link the 44-byte ring packet at `packet` into the ordering-table slot
/// kept at work +0xC: the tag is the slot word ORed with length 10, the
/// slot then points at the packet.
fn link_ring_packet(m: &mut Machine, work: u32, packet: u32) {
    let slot = m.lw(work + 0xc);
    let tag = m.lw(slot) | 0x0a00_0000;
    m.sw(packet, tag);
    let slot = m.lw(work + 0xc);
    m.sw(slot, link(packet));
}

/// 8008A61C: debris type 1, an expanding shock ring: the piece's
/// camera-relative position (word +4..+0xC minus the scratchpad camera,
/// shifted right by 12, each within 16 bits) rotated by the view gives the
/// translation;
/// RotMatrixYXZ of the piece angles (+0x10, +0x12, age << 7), scaled by
/// rsin(age * 1024 / life) x size (+0x16) and multiplied into the view
/// matrix (work +136), places a ring of 16 points (the 32-entry table at
/// work +36, inner radius 3/4). Each projected segment becomes a 44-byte
/// semi-transparent POLY_G4 between the inner and outer edges (mask mode
/// packets 0xE6000001/0xE6000000 around it), in grey fading with age, and
/// the ring is closed back to its first edge. The view rotation is
/// restored with a zero translation; scratchpad +4 is the packet cursor.
fn draw_shock_ring(m: &mut Machine, piece: u32, work: u32) {
    m.push_frame(0x30);
    for (i, offset) in (0u32..).zip([4u32, 8, 12]) {
        let value = m
            .lw(piece + offset)
            .wrapping_sub(m.lw(SCRATCH + 4 + offset));
        let delta = sra(value, 12);
        m.sh(work + 0x10 + i * 2, delta as u16);
        if slt(0x7fff, abs(delta)) {
            m.pop_frame(0x30);
            return;
        }
    }
    lwc2(m, reg::VXY0, work + 0x10);
    lwc2(m, reg::VZ0, work + 0x14);
    cop2(m, MVMVA_RT_V0);
    let mut life = lh(m, piece + 2);
    let age = lh(m, piece);
    if !slt(0, life) {
        life = 1;
    }
    for (register, mac) in [
        (ctrl::TRX, reg::MAC1),
        (ctrl::TRY, reg::MAC2),
        (ctrl::TRZ, reg::MAC3),
    ] {
        let value = m.gte.mfc2(mac);
        m.gte.ctc2(register, value);
    }
    let angles = work + 200;
    let matrix = work + 168;
    let x = m.lhu(piece + 0x10);
    m.sh(angles, x);
    let y = m.lhu(piece + 0x12);
    m.sh(angles + 4, (age << 7) as u16);
    m.sh(angles + 2, y);
    sdk::rot_matrix_yxz(m, angles, matrix);
    let phase = div_checked(m, age << 10, life, 0x8008_a73c);
    let wave = dispatch::call(m, 0x800a_60f8, [phase, 0, 0, 0]);
    let mut size = mul_lo(wave, lh(m, piece + 0x16));
    if slt(size, 0) {
        size = size.wrapping_add(0xfff);
    }
    let size = sra(size, 12);
    let scale = work + 208;
    m.sw(scale + 8, size);
    m.sw(scale + 4, size);
    m.sw(scale, size);
    scale_matrix(m, matrix, scale);
    dispatch::call(m, 0x800a_6a08, [work + 136, matrix, 0, 0]);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0);
    m.gte.mtc2(reg::VXY1, 0x0c00_0000);
    m.gte.mtc2(reg::VZ1, 0);
    m.gte.mtc2(reg::VXY2, 0x1000_0000);
    m.gte.mtc2(reg::VZ2, 0);
    let mut packet = m.lw(SCRATCH + 4);
    cop2(m, RTPT);
    let shade = if slt(sra(life, 1), age) {
        let remaining = life.wrapping_sub(age);
        div_checked(
            m,
            ((remaining << 3).wrapping_add(remaining)) << 3,
            life,
            0x8008_a800,
        )
    } else {
        36
    };
    m.sw(work, shade);
    let flag = m.gte.cfc2(ctrl::FLAG);
    m.sw(work + 8, flag);
    if !slt(m.lw(work + 8), 0) {
        swc2(m, reg::SXY1, work + 0x1c);
        swc2(m, reg::SXY2, work + 0x20);
        let first = m.lw(work + 0x1c);
        let second = m.lw(work + 0x20);
        m.sw(work + 0x7c, first);
        m.sw(work + 0x80, second);
        let sz3 = m.gte.mfc2(reg::SZ3);
        let depth = ring_depth(sz3);
        let ot = m.lw(SCRATCH);
        m.sw(work + 0xc, ot.wrapping_add(depth << 2));
        if !slt(depth, 10001) {
            // Unreachable after the clamp above, kept as compiled.
            let shade = mul_lo(m.lw(work), 0xb3b0u32.wrapping_sub(sz3));
            let high = ((u64::from(shade) * 0xb65c_6d59) >> 32) as u32;
            m.sw(work, (high >> 15) & 0xff);
        }
    }
    let first_flag = m.lw(work + 8);
    let shade = m.lw(work);
    let mut edge = packet + 36;
    m.sw(work + 0x84, first_flag);
    m.sw(work, shade | (shade << 8) | (shade << 16));
    for point in 1..16u32 {
        let outer_x = lh(m, work + 4 * point + 36);
        let outer_y = lh(m, work + 4 * point + 52);
        let quarter = |value: u32| {
            let triple = (value << 1).wrapping_add(value);
            let triple = if slt(triple, 0) {
                triple.wrapping_add(3)
            } else {
                triple
            };
            sra(triple, 2)
        };
        let inner_x = quarter(outer_x);
        let inner_y = quarter(outer_y);
        m.gte.mtc2(reg::VXY1, (inner_x & 0xffff) | (inner_y << 16));
        m.gte.mtc2(reg::VXY2, (outer_x & 0xffff) | (outer_y << 16));
        cop2(m, RTPT);
        if slt(m.lw(work + 8), 0) {
            let flag = m.gte.cfc2(ctrl::FLAG);
            m.sw(work + 8, flag);
            if slt(m.lw(work + 8), 0) {
                continue;
            }
            swc2(m, reg::SXY1, work + 0x1c);
            swc2(m, reg::SXY2, work + 0x20);
        } else {
            m.sw(edge - 28, 0x3a00_0000);
            m.sw(edge - 12, 0);
            let previous_inner = m.lw(work + 0x1c);
            m.sw(edge - 24, previous_inner);
            let previous_outer = m.lw(work + 0x20);
            m.sw(edge - 32, 0xe600_0001);
            m.sw(edge + 4, 0xe600_0000);
            m.sw(edge - 16, previous_outer);
            let flag = m.gte.cfc2(ctrl::FLAG);
            m.sw(work + 8, flag);
            if slt(m.lw(work + 8), 0) {
                continue;
            }
            let colour = m.lw(work);
            m.sw(edge - 20, colour);
            let colour = m.lw(work);
            m.sw(edge - 4, colour);
            swc2(m, reg::SXY1, work + 0x1c);
            swc2(m, reg::SXY2, work + 0x20);
            let inner = m.lw(work + 0x1c);
            m.sw(edge - 8, inner);
            let outer = m.lw(work + 0x20);
            m.sw(edge, outer);
            link_ring_packet(m, work, packet);
            edge += 44;
            packet += 44;
        }
        let sz3 = m.gte.mfc2(reg::SZ3);
        let depth = ring_depth(sz3);
        let ot = m.lw(SCRATCH);
        m.sw(work + 0xc, ot.wrapping_add(depth << 2));
    }
    if !slt(m.lw(work + 8), 0) && !slt(m.lw(work + 0x84), 0) {
        m.sw(packet + 8, 0x3a00_0000);
        m.sw(packet + 24, 0);
        let colour = m.lw(work);
        m.sw(packet + 4, 0xe600_0001);
        m.sw(packet + 32, colour);
        m.sw(packet + 16, colour);
        for (target, source) in [
            (40u32, 0x78u32),
            (12, 0x1c),
            (20, 0x20),
            (28, 0x7c),
            (36, 0x80),
        ] {
            let value = m.lw(work + source);
            m.sw(packet + target, value);
        }
        link_ring_packet(m, work, packet);
        packet += 44;
    }
    load_rotation(m, work + 136);
    m.gte.ctc2(ctrl::TRX, 0);
    m.gte.ctc2(ctrl::TRY, 0);
    m.gte.ctc2(ctrl::TRZ, 0);
    m.sw(SCRATCH + 4, packet);
    m.pop_frame(0x30);
}

// ==== camera mode 2: the external look camera (80077DD0) ======================================

/// 80077DD0: camera mode 2, the external view around the followed object
/// (+52): look angles toward the locked target (800779C8) plus the stick
/// and the look buttons, approached at a quarter of the difference (plus
/// 2, snapped when closer) or set with a cut; the camera offset +44 is
/// rotated by the object's angles times the look angles (80030778 twice,
/// MulMatrix) into +56..+64 (x 64), and the view angles (800313A8)
/// relative to the object (or to +20 when not following) go to +72..+76.
fn look_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x80);
    let look = frame + 16;
    let context = game_context(m);
    let target = if m.lw(context + 296) & 0x2000 != 0 {
        m.lw(camera + 188)
    } else {
        u32::MAX
    };
    let origin = if m.lw(camera) & 0x100 != 0 {
        m.lw(camera + 52)
    } else {
        camera + 4
    };
    look_toward(m, origin, target, look);
    let mut pitch = lh(m, look);
    let context = game_context(m);
    let mut yaw = lh(m, look + 2);
    let stick = m.lw(context + 312);
    let roll = lh(m, look + 4);
    if stick & 0x10 != 0 {
        pitch = pitch.wrapping_sub(m.lw(context + 332));
    }
    if stick & 0x20 != 0 {
        yaw = yaw.wrapping_add(m.lw(context + 336));
    }
    let held = m.lw(context + 296);
    if held & 0x10_0000 != 0 {
        yaw = yaw.wrapping_add(8192);
    }
    if held & 0x1_0000 != 0 {
        pitch = pitch.wrapping_add(4096);
    }
    if held & 0x2_0000 != 0 {
        pitch = pitch.wrapping_sub(4096);
    }
    if held & 0x4_0000 != 0 {
        yaw = yaw.wrapping_sub(4096);
    }
    if held & 0x8_0000 != 0 {
        yaw = yaw.wrapping_add(4096);
    }
    if slt(lh(m, camera + 182).wrapping_sub(yaw), 0xffff_e000) {
        let value = lhu(m, camera + 182).wrapping_add(16384);
        m.sh(camera + 182, value as u16);
    }
    if !slt(lh(m, camera + 182).wrapping_sub(yaw), 8193) {
        let value = lhu(m, camera + 182).wrapping_sub(16384);
        m.sh(camera + 182, value as u16);
    }
    let context = game_context(m);
    let goals = [pitch, yaw, roll];
    if (m.lw(context + 300) | m.lw(context + 308)) & 0x10_0000 != 0 {
        for (i, goal) in (0u32..).zip(goals) {
            m.sh(camera + 180 + i * 2, goal as u16);
        }
        mark_camera_cut(m, camera);
    } else {
        let mut steps = [0u32; 3];
        for (i, step) in (0u32..).zip(steps.iter_mut()) {
            let angle = lh(m, camera + 180 + i * 2);
            *step = sra(
                abs(angle.wrapping_sub(goals[i as usize])).wrapping_add(2),
                2,
            );
        }
        for (i, goal) in (0u32..).zip(goals) {
            let slot = camera + 180 + i * 2;
            let step = steps[i as usize];
            if slt(lh(m, slot), goal) {
                let value = lhu(m, slot).wrapping_add(step);
                m.sh(slot, value as u16);
            }
            if slt(goal, lh(m, slot)) {
                let value = lhu(m, slot).wrapping_sub(step);
                m.sh(slot, value as u16);
            }
        }
        for (i, goal) in (0u32..).zip(goals) {
            let slot = camera + 180 + i * 2;
            if slt(abs(lh(m, slot).wrapping_sub(goal)), steps[i as usize]) {
                m.sh(slot, goal as u16);
            }
        }
    }
    let object_matrix = frame + 40;
    let look_matrix = frame + 72;
    let object = m.lw(camera + 52);
    shared::f_80030534(m, [object + 16, object_matrix, 0, 0]);
    shared::f_80030534(m, [camera + 180, look_matrix, 0, 0]);
    sdk::mul_matrix(m, object_matrix, look_matrix);
    load_rotation(m, object_matrix);
    lwc2(m, reg::VXY0, camera + 44);
    lwc2(m, reg::VZ0, camera + 48);
    cop2(m, MVMVA_RT_V0);
    store_mac123(m, frame + 24);
    for offset in [0u32, 4, 8] {
        let value = m.lw(frame + 24 + offset) << 6;
        m.sw(camera + 56 + offset, value);
    }
    let base = if m.lw(camera) & 0x100 != 0 {
        let object = m.lw(camera + 52);
        view_angles(m, object + 16, camera + 180, look);
        m.lw(camera + 52) + 16
    } else {
        view_angles(m, camera + 20, camera + 180, look);
        camera + 20
    };
    for i in 0..3u32 {
        let value = lh(m, look + i * 2).wrapping_sub(lh(m, base + i * 2));
        m.sh(camera + 72 + i * 2, value as u16);
    }
    m.pop_frame(0x80);
}

// ==== 8008488C: the noise overlay of the screen overlays (8008FFAC bit 1) ==========================

// JP-REVIEW(R0385): near function: PAL 80083244 -> JP 8008488C, imm:2, 2 instructions differ (whole routine)
/// 8008488C: a full-screen noise layer of `level` (0: nothing; negative:
/// subtractive blend mode 2, else additive mode 1): a DR_MODE packet
/// (texture page 0x218 with the blend mode, texture window 0xE20C4318),
/// the noise tiles of 80084318 (owned by the loader; colour 0x668300
/// faded by 4096 - |level| through RGBC/IR0, CLUT 18134, 320x256 from
/// (-160, -128)) and a closing DR_MODE, chained in at `ot`. Returns the
/// packet cursor after it.
// JP-PATCH: replaced by `crate::jp::res_p2_render::noise_overlay` (src/jp/mod.rs REPLACES, PATCHES.md)
fn noise_overlay(m: &mut Machine, ot: u32, cursor: u32, level: u32) -> u32 {
    crate::jp::res_p2_render::noise_overlay(m, ot, cursor, level)
}

// ==== phase-3 additions: debug text, loading screens, camera setup and modes ===============

/// 800104AC: the 1.14 sine table.
const SINE: u32 = 0x8001_04ac;

/// `value / 2^shift` rounded toward zero (`bgez` / `addiu 2^shift-1` / `sra`).
fn div_pow2(value: u32, shift: u32) -> u32 {
    let value = if slt(value, 0) {
        value.wrapping_add((1 << shift) - 1)
    } else {
        value
    };
    sra(value, shift)
}
/// Sign-extend a halfword held in a register.
fn sext16(value: u32) -> u32 {
    value as u16 as i16 as i32 as u32
}

/// 800277F0 (x, y, value, digits) -> x after the text, or -1 for 10 or
/// more digits: `value` in hexadecimal (digit table 80010124).
pub fn debug_hex(m: &mut Machine, x: u32, y: u32, value: u32, digits: u32) -> u32 {
    let frame = m.push_frame(0x38);
    for offset in [0u32, 4, 8, 0xc] {
        let word = m.lw(0x8001_0124 + offset);
        m.sw(frame + 0x10 + offset, word);
    }
    let result = if !slt(digits, 10) {
        u32::MAX
    } else {
        let mut value = value;
        let mut cursor = frame + 0x28;
        m.sb(frame + 0x28, 0);
        let mut index = 0u32;
        while slt(index, digits) {
            cursor = cursor.wrapping_sub(1);
            let digit = lbu(m, frame + 0x10 + (value & 0xf));
            index = index.wrapping_add(1);
            m.sb(cursor, digit as u8);
            value = sra(value, 4);
        }
        dispatch::call(m, 0x8002_7608, [x, y, cursor, 0])
    };
    m.pop_frame(0x38);
    result
}

/// 80077460 (camera): the debug camera readout (flags, mode, position,
/// angles) when the context's debug bit is set.
pub fn debug_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x20);
    let _ = frame;
    let x = dispatch::call(m, 0x8002_7608, [0xe6, 0xb4, 0x800b_ce24, 0]).wrapping_add(0xc);
    let word = m.lw(camera);
    debug_hex(m, x, 0xb4, word, 8);
    let word = m.lw(camera + 0x50);
    debug_hex(m, x, 0xba, word, 8);
    for (row, offset) in [(0xc0u32, 4u32), (0xc6, 8), (0xcc, 0xc)] {
        let value = sra(m.lw(camera + offset), 12);
        dispatch::call(m, 0x8002_76c8, [x, row, value, 0x207]);
    }
    for (row, offset) in [(0xd2u32, 0x14u32), (0xd8, 0x16), (0xde, 0x18)] {
        let value = lh(m, camera + offset);
        dispatch::call(m, 0x8002_76c8, [x, row, value, 0x207]);
    }
    m.pop_frame(0x20);
}

// JP-REVIEW(R0387): near function: PAL 800360FC -> JP 80036440, del:1 imm:1 ins:2 op:1, 5 instructions differ (whole routine)
/// 80036440 (ot, prim) -> next primitive: the loading screen copies the
/// displayed 320x256 half of the frame buffer to the other half (context
/// +4), and sets 4 in `[[gp+0x2D4]]`.
// JP-PATCH: replaced by `crate::jp::res_p2_render::copy_frame` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn copy_frame(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    crate::jp::res_p2_render::copy_frame(m, ot, prim)
}

// JP-REVIEW(R0388): near function: PAL 800362B4 -> JP 800365FC, del:5 imm:1 ins:8 op:1 reg:2, 17 instructions differ (whole routine)
/// 800365FC (ot, prim, level, color; second color as the caller's stack
/// word +0x10) -> next primitive: four gouraud quads closing a frame
/// border of width `level * 160 / 4096` from the screen edges.
// JP-PATCH: replaced by `crate::jp::res_p2_render::border_quads` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn border_quads(m: &mut Machine, ot: u32, prim: u32, level: u32, color: u32) -> u32 {
    crate::jp::res_p2_render::border_quads(m, ot, prim, level, color)
}

/// 8005E40C (block, ot, prim) -> next primitive: the loading-screen tiles
/// (8005E10C then 8005E178) in 0x424 bytes.
pub fn loading_tiles(m: &mut Machine, block: u32, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let _ = frame;
    dispatch::call(m, 0x8005_e10c, [prim, ot, prim, 0]);
    dispatch::call(m, 0x8005_e178, [block, ot, prim, 0]);
    m.pop_frame(0x20);
    prim + 0x424
}

// JP-REVIEW(R0389): near function: PAL 80036724 -> JP 80036A84, imm:3 ins:1 op:2 reg:4, 10 instructions differ (whole routine)
/// 80036A84 (ot, prim) -> next primitive: loading visual mode 0, the fade
/// between the briefing and flight: texts, the state's fade stages by
/// level (+2 for mode 1, full for mode 2), the tiles, the opening
/// aperture (800847A8) and the closing border.
// JP-PATCH: replaced by `crate::jp::res_p2_render::fade_screen` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn fade_screen(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    crate::jp::res_p2_render::fade_screen(m, ot, prim)
}

// JP-REVIEW(R0390): near function: PAL 80082998 -> JP 80083FE0, imm:2, 2 instructions differ (whole routine)
/// 80083FE0 (ot, prim) -> next primitive: the loading starfield: a dark
/// backdrop tile and 512 depth-cued one-pixel stars (fixed seed 0x2FD,
/// the RNG state restored afterwards) around the camera direction.
// JP-PATCH: replaced by `crate::jp::res_p2_render::starfield` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn starfield(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    crate::jp::res_p2_render::starfield(m, ot, prim)
}

// JP-REVIEW(R0391): near function: PAL 8003617C -> JP 800364C4, imm:1, 1 instructions differ (whole routine)
/// 800364C4 (ot, prim) -> next primitive: loading visual mode 2, the
/// starfield seen from a slowly rolling camera (context +0x20 timer).
// JP-PATCH: replaced by `crate::jp::res_p2_render::star_screen` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn star_screen(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    crate::jp::res_p2_render::star_screen(m, ot, prim)
}

/// 80073928 (flags): select the flight camera script by the setup flags
/// (`[gp+0x2A0]` kind, `[gp+0x2A4]` path 800B3B94 + 56n, or `[gp+0x2A8]`/
/// `[gp+0x2AC]` paths 800B3E6C + 20n and 800B3EBC by the player's type).
pub fn select_camera_script(m: &mut Machine, flags: u32) {
    let table = m.gw(ENTITIES);
    let player = m.lw(table);
    let kind = lh(m, player + 0x48);
    let index = flags & 0xff;
    let path = |index: u32| index.wrapping_mul(56).wrapping_add(0x800b_3b94);
    if flags & 0x100 != 0 {
        m.set_gw(0x2a0, 0);
        m.set_gw(0x2a4, path(index));
        return;
    }
    let kind_code = if flags & 0x200 != 0 {
        1
    } else if flags & 0x400 != 0 || flags & 0x800 != 0 {
        let code = if flags & 0x400 != 0 { 2 } else { 3 };
        m.set_gw(0x2a0, code);
        m.set_gw(0x2a8, index.wrapping_mul(20).wrapping_add(0x800b_3e6c));
        m.set_gw(0x2ac, (kind << 4).wrapping_add(0x800b_3ebc));
        return;
    } else if flags & 0x1000 != 0 {
        m.set_gw(0x2a0, 4);
        return;
    } else if flags & 0x2000 != 0 {
        5
    } else if flags & 0x4000 != 0 {
        6
    } else {
        return;
    };
    m.set_gw(0x2a0, kind_code);
    m.set_gw(0x2a4, path(index));
}

/// 80076DBC: reset the camera control blocks `[800BD094]` (+0 0x400, +4
/// 0x2000, +8 and +0x10 cleared) and `[800BD098]` +0x24.
pub fn reset_camera_control(m: &mut Machine) {
    let control = m.lw(0x800b_d094);
    let other = m.lw(0x800b_d098);
    m.sw(control + 0x10, 0);
    m.sw(other + 0x24, 0);
    m.sw(control, 0x400);
    m.sw(control + 4, 0x2000);
    m.sw(control + 8, 0);
}

// JP-REVIEW(R0392): near function: PAL 80077FAC -> JP 8007961C, imm:1, 1 instructions differ (whole routine)
/// 8007961C (flags): the starting camera mode (0x400 for the scripted
/// setups 0x6300, else 2 with context +0x3A3 or 1) on the player, then
/// 80035740(1).
// JP-PATCH: replaced by `crate::jp::res_p2_render::start_camera` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn start_camera(m: &mut Machine, flags: u32) {
    crate::jp::res_p2_render::start_camera(m, flags)
}

/// 80077004 (position, out, step, buttons): the debug free camera (mode
/// 0x40000000). The pad bits turn the angle deltas (frame +0x10..+0x14,
/// +-0x40 and +-0x3C, << 2 with bit 1) and move the step vector (+0x18..
/// +0x20, +-0x40000 and 0x3F000); `out` receives the step (+0) and the
/// deltas (+0x10). The angles at `position + 0x10` turn by 80030F64, the
/// step is rotated into `step` by 80030EF8 and added to `position`.
pub fn free_camera(m: &mut Machine, position: u32, out: u32, step: u32, buttons: u32) {
    let frame = m.push_frame(0x40);
    sdk::memset(m, frame + 0x10, 0, 8);
    sdk::memset(m, frame + 0x18, 0, 0x10);
    let shift = (buttons << 1) & 4;
    let turns: [(u32, u32, u32); 12] = [
        (0x10, 0x10, 0xffc0),
        (0x40, 0x10, 0x40),
        (0x20, 0x14, 0x40),
        (0x80, 0x14, 0xffc0),
        (0x200, 0x12, 0x40),
        (0x100, 0x12, 0xffc0),
        (0x14, 0x10, 0x3c),
        (0x44, 0x10, 0xffc4),
        (0x24, 0x14, 0xffc4),
        (0x84, 0x14, 0x3c),
        (0x204, 0x12, 0xffc4),
        (0x104, 0x12, 0x3c),
    ];
    for (mask, offset, delta) in turns {
        if buttons & mask == mask {
            let value = lhu(m, frame + offset).wrapping_add(delta);
            m.sh(frame + offset, value as u16);
        }
    }
    for offset in [0x10u32, 0x14, 0x12] {
        let value = lh(m, frame + offset) << shift;
        m.sh(frame + offset, value as u16);
    }
    let moves: [(u32, u32, u32); 12] = [
        (0x1000, 0x20, 0x0004_0000),
        (0x4000, 0x20, 0xfffc_0000),
        (0x8000, 0x18, 0xfffc_0000),
        (0x2000, 0x18, 0x0004_0000),
        (0x400, 0x1c, 0xfffc_0000),
        (0x800, 0x1c, 0x0004_0000),
        (0x1004, 0x20, 0xfffc_1000),
        (0x4004, 0x20, 0x0003_f000),
        (0x8004, 0x18, 0x0003_f000),
        (0x2004, 0x18, 0xfffc_1000),
        (0x404, 0x1c, 0x0003_f000),
        (0x804, 0x1c, 0xfffc_1000),
    ];
    for (mask, offset, delta) in moves {
        if buttons & mask == mask {
            let value = m.lw(frame + offset).wrapping_add(delta);
            m.sw(frame + offset, value);
        }
    }
    for offset in [0x18u32, 0x20, 0x1c] {
        let value = m.lw(frame + offset) << shift;
        m.sw(frame + offset, value);
    }
    let words = m.read_bytes(frame + 0x18, 16);
    m.write_bytes(out, &words);
    let deltas = m.read_bytes(frame + 0x10, 8);
    m.write_bytes(out + 0x10, &deltas);
    let angles = position + 0x10;
    dispatch::call(m, 0x8003_0f64, [angles, out + 0x10, 0, 0]);
    dispatch::call(m, 0x8003_0ef8, [out, angles, step, 0]);
    if buttons & 0xc00 == 0xc00 {
        m.sh(position + 0x10, 0xf000);
        m.sh(position + 0x12, 0);
        m.sh(position + 0x14, 0);
    }
    if buttons & 6 == 6 {
        m.sh(position + 0x10, 0);
        m.sh(position + 0x12, 0);
        m.sh(position + 0x14, 0);
    }
    for offset in [0u32, 4, 8] {
        let value = m.lw(position + offset).wrapping_add(m.lw(step + offset));
        m.sw(position + offset, value);
    }
    m.pop_frame(0x40);
}

/// The view tail of the orbit camera modes: the eye offset (0, 0,
/// -[camera +0xE4] >> `shift`) rotated by the orbit angles +0xD4 into
/// +0x38..+0x40 (<< `shift`), and the orbit angles relative to the target
/// (+0x34) into +0x48..+0x4C. `count` advances the mode timer +0xD0 first.
fn orbit_view(m: &mut Machine, camera: u32, frame: u32, shift: u32, count: bool) {
    let target = m.lw(camera + 0x34);
    let angles = [
        lh(m, camera + 0xd4),
        lh(m, camera + 0xd6),
        lh(m, camera + 0xd8),
    ];
    if count {
        let timer = m.lw(camera + 0xd0).wrapping_add(1);
        m.sw(camera + 0xd0, timer);
    }
    let base = [
        lh(m, target + 0x10),
        lh(m, target + 0x12),
        lh(m, target + 0x14),
    ];
    m.sh(frame + 0x30, 0);
    m.sh(frame + 0x32, 0);
    let distance = sra(m.lw(camera + 0xe4).wrapping_neg(), shift);
    m.sh(frame + 0x34, distance as u16);
    shared::rotation_yxz(m, camera + 0xd4, frame + 0x10);
    load_rotation(m, frame + 0x10);
    lwc2(m, reg::VXY0, frame + 0x30);
    lwc2(m, reg::VZ0, frame + 0x34);
    cop2(m, MVMVA_RT_V0);
    store_mac123(m, frame + 0x38);
    let x = m.lw(frame + 0x38) << shift;
    m.sw(camera + 0x38, x);
    let y = m.lw(frame + 0x3c) << shift;
    m.sw(camera + 0x3c, y);
    let z = m.lw(frame + 0x40);
    for i in 0..3u32 {
        let value = angles[i as usize].wrapping_sub(base[i as usize]);
        m.sh(camera + 0x48 + i * 2, value as u16);
    }
    m.sw(camera + 0x40, z << shift);
}

/// The chase tail of camera modes 4 and 2048: the rotation at frame +0x28
/// (the target's angles times the camera's +0xB4) applied to the eye
/// offset at `offset` (into +0x38..+0x40, << 6), and the look angles
/// (800313A8 from the attached object, flag 0x100, or the camera +0x14)
/// relative to that base into +0x48..+0x4C.
fn chase_view(m: &mut Machine, camera: u32, frame: u32, offset: u32) {
    load_rotation(m, frame + 0x28);
    lwc2(m, reg::VXY0, offset);
    lwc2(m, reg::VZ0, offset + 4);
    cop2(m, MVMVA_RT_V0);
    store_mac123(m, frame + 0x18);
    let x = m.lw(frame + 0x18) << 6;
    m.sw(camera + 0x38, x);
    let flags = m.lw(camera);
    let y = m.lw(frame + 0x1c) << 6;
    m.sw(camera + 0x3c, y);
    let z = m.lw(frame + 0x20) << 6;
    m.sw(camera + 0x40, z);
    let base = if flags & 0x100 != 0 {
        let object = m.lw(camera + 0x34);
        view_angles(m, object + 0x10, camera + 0xb4, frame + 0x10);
        m.lw(camera + 0x34) + 0x10
    } else {
        view_angles(m, camera + 0x14, camera + 0xb4, frame + 0x10);
        camera + 0x14
    };
    let relative = [
        lh(m, frame + 0x10).wrapping_sub(lh(m, base)),
        lh(m, frame + 0x12).wrapping_sub(lh(m, base + 2)),
        lh(m, frame + 0x14).wrapping_sub(lh(m, base + 4)),
    ];
    for (i, value) in relative.into_iter().enumerate() {
        m.sh(camera + 0x48 + i as u32 * 2, value as u16);
    }
}

/// The chase rotation of camera modes 4 and 2048: frame +0x28 = the
/// target's rotation (80030778) times the camera angles +0xB4.
fn chase_rotation(m: &mut Machine, camera: u32, frame: u32) {
    let target = m.lw(camera + 0x34);
    shared::rotation_yxz(m, target + 0x10, frame + 0x28);
    shared::rotation_yxz(m, camera + 0xb4, frame + 0x48);
    sdk::mul_matrix(m, frame + 0x28, frame + 0x48);
}

/// 8007754C (camera, buttons): the debug orbit camera (mode 0x20000000
/// with context +0x5E bit 15): the pad turns the orbit angles +0xD4
/// (+-0x80, pitch clamped to +-0x1000, yaw wrapped) and the distance
/// +0xE4 (+-0xA000 up to 0x31FFFF), then the orbit view (shift 7).
pub fn debug_orbit_camera(m: &mut Machine, camera: u32, buttons: u32) {
    let frame = m.push_frame(0x60);
    if buttons & 0x40 != 0 && slt(lh(m, camera + 0xd4), 0x1000) {
        let value = lhu(m, camera + 0xd4).wrapping_add(0x80);
        m.sh(camera + 0xd4, value as u16);
    }
    if buttons & 0x10 != 0 && !slt(lh(m, camera + 0xd4), (-0xfff_i32) as u32) {
        let value = lhu(m, camera + 0xd4).wrapping_sub(0x80);
        m.sh(camera + 0xd4, value as u16);
    }
    if buttons & 0x80 != 0 {
        let old = lhu(m, camera + 0xd6);
        let value = old.wrapping_sub(0x80);
        m.sh(camera + 0xd6, value as u16);
        if slt(sext16(value), (-0x2000_i32) as u32) {
            m.sh(camera + 0xd6, old.wrapping_add(0x3f80) as u16);
        }
    }
    if buttons & 0x20 != 0 {
        let old = lhu(m, camera + 0xd6);
        let value = old.wrapping_add(0x80);
        m.sh(camera + 0xd6, value as u16);
        if !slt(sext16(value), 0x2001) {
            m.sh(camera + 0xd6, old.wrapping_sub(0x3f80) as u16);
        }
    }
    if buttons & 0x400 != 0 {
        let distance = m.lw(camera + 0xe4);
        if !slt(0x31_ffff, distance) {
            m.sw(camera + 0xe4, distance.wrapping_add(0xa000));
        }
    }
    if buttons & 0x800 != 0 {
        let distance = m.lw(camera + 0xe4);
        if slt(0, distance) {
            m.sw(camera + 0xe4, distance.wrapping_add(0xffff_6000));
        }
    }
    orbit_view(m, camera, frame, 7, false);
    m.pop_frame(0x60);
}

/// 80078234 (camera, target): camera mode 2048, the chase view of
/// `target`: the look angles toward it (800779C8 from the attached
/// object or the camera), offset by the context's pad look (+0x138 bits
/// 4/5 with +0x14C/+0x150, +0x128 bits 16..20) and the yaw unwrapped
/// toward +0xB6; snapped (with 8007786C) on a cut (context +0x12C or
/// +0x134 bit 20), otherwise eased a quarter of the way into +0xB4..+0xB8.
pub fn chase_camera(m: &mut Machine, camera: u32, target: u32) {
    let frame = m.push_frame(0x80);
    let origin = if m.lw(camera) & 0x100 != 0 {
        m.lw(camera + 0x34)
    } else {
        camera + 4
    };
    look_toward(m, origin, target, frame + 0x10);
    let mut pitch = lh(m, frame + 0x10);
    let context = game_context(m);
    let mut yaw = lh(m, frame + 0x12);
    let look = m.lw(context + 0x138);
    let roll = lh(m, frame + 0x14);
    if look & 0x10 != 0 {
        pitch = pitch.wrapping_sub(m.lw(context + 0x14c));
    }
    if look & 0x20 != 0 {
        yaw = yaw.wrapping_add(m.lw(context + 0x150));
    }
    let held = m.lw(context + 0x128);
    if held & 0x10_0000 != 0 {
        yaw = yaw.wrapping_add(0x2000);
    }
    if held & 0x1_0000 != 0 {
        pitch = pitch.wrapping_add(0x1000);
    }
    if held & 0x2_0000 != 0 {
        pitch = pitch.wrapping_sub(0x1000);
    }
    if held & 0x4_0000 != 0 {
        yaw = yaw.wrapping_sub(0x1000);
    }
    if held & 0x8_0000 != 0 {
        yaw = yaw.wrapping_add(0x1000);
    }
    if slt(lh(m, camera + 0xb6).wrapping_sub(yaw), (-0x2000_i32) as u32) {
        let value = lhu(m, camera + 0xb6).wrapping_add(0x4000);
        m.sh(camera + 0xb6, value as u16);
    }
    if !slt(lh(m, camera + 0xb6).wrapping_sub(yaw), 0x2001) {
        let value = lhu(m, camera + 0xb6).wrapping_sub(0x4000);
        m.sh(camera + 0xb6, value as u16);
    }
    let context = game_context(m);
    let cut = m.lw(context + 0x12c) & 0x10_0000 != 0 || m.lw(context + 0x134) & 0x10_0000 != 0;
    if cut {
        m.sh(camera + 0xb4, pitch as u16);
        m.sh(camera + 0xb6, yaw as u16);
        m.sh(camera + 0xb8, roll as u16);
        dispatch::call(m, 0x8007_786c, [camera, 0, 0, 0]);
    } else {
        let goals = [pitch, yaw, roll];
        let mut steps = [0u32; 3];
        for (i, goal) in goals.into_iter().enumerate() {
            let current = lh(m, camera + 0xb4 + i as u32 * 2);
            let difference = current.wrapping_sub(goal);
            let distance = if slt(difference, 0) {
                goal.wrapping_sub(current)
            } else {
                difference
            };
            steps[i] = sra(distance.wrapping_add(2), 2);
        }
        for (i, goal) in goals.into_iter().enumerate() {
            let at = camera + 0xb4 + i as u32 * 2;
            if slt(lh(m, at), goal) {
                let value = lhu(m, at).wrapping_add(steps[i]);
                m.sh(at, value as u16);
            }
            if slt(goal, lh(m, at)) {
                let value = lhu(m, at).wrapping_sub(steps[i]);
                m.sh(at, value as u16);
            }
        }
        for (i, goal) in goals.into_iter().enumerate() {
            let at = camera + 0xb4 + i as u32 * 2;
            let current = lh(m, at);
            let difference = current.wrapping_sub(goal);
            let distance = if slt(difference, 0) {
                goal.wrapping_sub(current)
            } else {
                difference
            };
            if slt(distance, steps[i]) {
                m.sh(at, goal as u16);
            }
        }
    }
    chase_rotation(m, camera, frame);
    chase_view(m, camera, frame, camera + 0x2c);
    m.pop_frame(0x80);
}

/// 8007895C (camera): camera mode 4, the swaying chase view: pitch and
/// yaw +0xB4/+0xB6 from the frame counter's sine (gameplay +0xC), roll
/// cleared, then the chase tail with the eye offset +0x2C.
pub fn sway_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x88);
    let gameplay = m.gw(GAMEPLAY);
    let counter = m.lw(gameplay + 0xc);
    m.sh(camera + 0xb8, 0);
    let sine = sra(lhu(m, SINE + ((counter << 7) & 0x1f80)) << 16, 18);
    let rest = 0x1000u32.wrapping_sub(sine);
    m.sh(camera + 0xb4, rest.wrapping_neg() as u16);
    m.sh(camera + 0xb6, (rest << 1) as u16);
    chase_rotation(m, camera, frame);
    let offset = [
        lh(m, camera + 0x2c),
        lh(m, camera + 0x2e),
        lh(m, camera + 0x30),
    ];
    for (i, value) in offset.into_iter().enumerate() {
        m.sh(frame + 0x68 + i as u32 * 2, value as u16);
    }
    chase_view(m, camera, frame, frame + 0x68);
    m.pop_frame(0x88);
}

/// The approach of the timed orbit modes: `[camera +0xE4] += (rate * 10 /
/// 4096) << 12` while it is at most `limit`.
fn approach_distance(m: &mut Machine, camera: u32, rate: u32, limit: u32) {
    let distance = m.lw(camera + 0xe4);
    if !slt(limit, distance) {
        let step = div_pow2(rate.wrapping_mul(10), 12) << 12;
        m.sw(camera + 0xe4, distance.wrapping_add(step));
    }
}

/// The timer rate of the timed orbit modes: `[camera +0xD0] << shift`,
/// at most 0x1000.
fn orbit_rate(m: &mut Machine, camera: u32, shift: u32) -> u32 {
    let rate = m.lw(camera + 0xd0) << shift;
    if slt(rate, 0x1001) { rate } else { 0x1000 }
}

/// 80078B28 (camera): camera mode 16, the pull-away orbit: on its first
/// frame the distance +0xE4 from the eye offset +0x30 and the yaw from
/// the camera's +0x16 (+0x2000, wrapped); then it backs off (up to
/// 0xAEFFFF), pitches up to -0x7FF and turns with the timer +0xD0.
/// Skipped while the context is paused (bit 29).
pub fn pull_away_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x60);
    if context_word(m, 0) & 0x2000_0000 == 0 {
        let timer = m.lw(camera + 0xd0);
        let rate = orbit_rate(m, camera, 7);
        if timer == 0 {
            let offset = lh(m, camera + 0x30);
            let yaw = lhu(m, camera + 0x16);
            m.sw(camera + 0xe4, offset.wrapping_neg() << 6);
            let value = yaw.wrapping_add(0x2000);
            m.sh(camera + 0xd6, value as u16);
            if slt(sext16(value), (-0x2000_i32) as u32) {
                m.sh(camera + 0xd6, yaw.wrapping_add(0x6000) as u16);
            }
            if !slt(lh(m, camera + 0xd6), 0x2001) {
                let value = lhu(m, camera + 0xd6).wrapping_sub(0x4000);
                m.sh(camera + 0xd6, value as u16);
            }
        }
        approach_distance(m, camera, rate, 0xae_ffff);
        if !slt(lh(m, camera + 0xd4), (-0x7ff_i32) as u32) {
            let value = lhu(m, camera + 0xd4).wrapping_sub(div_pow2(rate, 7));
            m.sh(camera + 0xd4, value as u16);
        }
        let value = lhu(m, camera + 0xd6).wrapping_add(div_pow2(rate, 7));
        m.sh(camera + 0xd6, value as u16);
        if !slt(sext16(value), 0x2001) {
            m.sh(camera + 0xd6, value.wrapping_sub(0x4000) as u16);
        }
        orbit_view(m, camera, frame, 9, true);
    }
    m.pop_frame(0x60);
}

/// 80078FDC (camera): camera mode 64, the level pull-back: on its first
/// frame the distance 0x190000 - (offset +0x30 << 6) and the camera's yaw
/// and roll; then it backs off (up to 0x3E7FFF) while the roll +0xD8
/// settles toward 0 by 16. Skipped while the context is paused.
pub fn pull_back_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x60);
    if context_word(m, 0) & 0x2000_0000 == 0 {
        let timer = m.lw(camera + 0xd0);
        let rate = orbit_rate(m, camera, 7);
        if timer == 0 {
            let offset = lh(m, camera + 0x30);
            let yaw = lhu(m, camera + 0x16);
            let roll = lhu(m, camera + 0x18);
            m.sw(camera + 0xe4, 0x19_0000u32.wrapping_sub(offset << 6));
            m.sh(camera + 0xd6, yaw as u16);
            m.sh(camera + 0xd8, roll as u16);
        }
        approach_distance(m, camera, rate, 0x3e_7fff);
        if !slt(lh(m, camera + 0xd8), 0x11) {
            let value = lhu(m, camera + 0xd8).wrapping_sub(0x10);
            m.sh(camera + 0xd8, value as u16);
        }
        if slt(lh(m, camera + 0xd8), (-0x10_i32) as u32) {
            let value = lhu(m, camera + 0xd8).wrapping_add(0x10);
            m.sh(camera + 0xd8, value as u16);
        }
        if slt(abs(lh(m, camera + 0xd8)), 0x10) {
            m.sh(camera + 0xd8, 0);
        }
        orbit_view(m, camera, frame, 9, true);
    }
    m.pop_frame(0x60);
}

/// 800791E8 (camera): camera mode 256, the swing-around: pitch
/// -sin(t/2) / 8 - 0xE00 and yaw from the target's +0x12 plus 0x2000 -
/// 2 sin(t/4) (wrapped), distance from the eye offset +0x30 and the yaw
/// swing, with the timer +0xD0 << 5 (at most 0x1000) as t. Skipped
/// while the context is paused.
pub fn swing_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x60);
    if context_word(m, 0) & 0x2000_0000 == 0 {
        let rate = orbit_rate(m, camera, 5);
        let sine = sdk::rsin(m, sra(rate, 1));
        let pitch = sra(
            {
                let value = (sine << 9).wrapping_neg();
                if slt(value, 0) {
                    value.wrapping_add(0xfff)
                } else {
                    value
                }
            },
            12,
        )
        .wrapping_add(0xffff_f200);
        let sine = sdk::rsin(m, sra(rate, 2)) << 1;
        let target = m.lw(camera + 0x34);
        m.sh(camera + 0xd4, pitch as u16);
        let swing = 0x2000u32.wrapping_sub(sine);
        let value = lhu(m, target + 0x12).wrapping_add(swing);
        m.sh(camera + 0xd6, value as u16);
        if slt(sext16(value), (-0x2000_i32) as u32) {
            m.sh(camera + 0xd6, value.wrapping_add(0x4000) as u16);
        }
        if !slt(lh(m, camera + 0xd6), 0x2001) {
            let value = lhu(m, camera + 0xd6).wrapping_sub(0x4000);
            m.sh(camera + 0xd6, value as u16);
        }
        let offset = lh(m, camera + 0x30);
        let distance = (offset.wrapping_neg() << 10).wrapping_sub(swing << 9);
        m.sw(camera + 0xe4, distance);
        orbit_view(m, camera, frame, 9, true);
    }
    m.pop_frame(0x60);
}

// ==== phase-3 additions: lens flare, heat shimmer, explosion renderer ======================

/// 800B440C: the ten lens flare elements (8 bytes: kind, brightness
/// threshold, CLUT, half size, position scale).
const FLARE_ELEMENTS: u32 = 0x800b_440c;
/// SQR sf=0.
const SQR0: u32 = 0x0a0_0428;
/// OP sf=0.
const OP0: u32 = 0x170_000c;

/// 8008B7AC (state, cursors): the lens flare of the sun renderer: each
/// element whose threshold is below the brightness (state +0x10) is a
/// grey textured quad at the sun's screen offset (state +8) scaled by the
/// element (state +0x1C/+0x1E), four mirrored quarter quads for kinds 0
/// and 1, one quad otherwise. The chain goes at the head of the OT slot
/// `[cursors]`; `[cursors + 4]` is the primitive cursor.
pub fn lens_flare(m: &mut Machine, state: u32, cursors: u32) {
    m.push_frame(8);
    let mut packet = m.lw(cursors + 4);
    for index in 0..10u32 {
        let entry = FLARE_ELEMENTS + index * 8;
        let threshold = lbu(m, entry + 1);
        let level = m.lw(state + 0x10).wrapping_sub(threshold);
        if !slt(0, level) {
            continue;
        }
        let scale = lh(m, entry + 6);
        let grey = level | (level << 16) | (level << 8);
        let x = div_pow2(mul_lo(lh(m, state + 8), scale), 12);
        let y = div_pow2(mul_lo(lh(m, state + 0xa), scale), 12);
        m.sh(state + 0x1c, x as u16);
        m.sh(state + 0x1e, y as u16);
        let kind = lbu(m, entry);
        if slt(kind, 2) {
            let base = kind << 5;
            let u = base + 0xc0;
            let corner = base + 0xdf;
            let clut = lhu(m, entry + 2);
            m.sh(state + 0x2c, (u | 0x3f00) as u16);
            m.sw(state + 0x28, corner | 0x0038_2000);
            m.sh(state + 0x2e, (corner | 0x3f00) as u16);
            m.sw(state + 0x24, (clut << 16) | u | 0x2000);
            let size = lh(m, entry + 4);
            // Widescreen: the element widths squeezed like the projection.
            let size_x = widescreen::squeeze_x(m, size);
            for quarter in 0..4u32 {
                let next = packet + 0x28;
                m.sw(packet, link(next) | 0x0900_0000);
                m.sw(packet + 4, grey | 0x2e00_0000);
                let word = m.lw(state + 0x24);
                m.sw(packet + 0xc, word);
                let word = m.lw(state + 0x28);
                m.sw(packet + 0x14, word);
                let half = lhu(m, state + 0x2c);
                m.sh(packet + 0x1c, half as u16);
                let half = lhu(m, state + 0x2e);
                m.sh(packet + 0x24, half as u16);
                let word = m.lw(state + 0x1c);
                m.sw(packet + 0x20, word);
                let cx = lh(m, state + 0x1c);
                let x = if quarter & 1 != 0 {
                    cx.wrapping_add(size_x)
                } else {
                    cx.wrapping_sub(size_x)
                };
                m.sh(packet + 0x18, x as u16);
                let half = lhu(m, packet + 0x18);
                m.sh(packet + 8, half as u16);
                let half = lhu(m, state + 0x1c);
                m.sh(packet + 0x10, half as u16);
                let cy = lh(m, state + 0x1e);
                let y = if quarter >> 1 != 0 {
                    cy.wrapping_add(size)
                } else {
                    cy.wrapping_sub(size)
                };
                m.sh(packet + 0x12, y as u16);
                let half = lhu(m, packet + 0x12);
                m.sh(packet + 0xa, half as u16);
                let half = lhu(m, state + 0x1e);
                m.sh(packet + 0x1a, half as u16);
                packet = next;
            }
        } else {
            let base = (kind & 1) << 4;
            let u = base + 0xe0;
            let next = packet + 0x28;
            m.sw(packet, link(next) | 0x0900_0000);
            m.sw(packet + 4, grey | 0x2e00_0000);
            let corner = base + 0xef;
            let clut = lhu(m, entry + 2);
            m.sw(packet + 0x14, corner | 0x0038_0000);
            m.sh(packet + 0x1c, (u | 0xf00) as u16);
            m.sh(packet + 0x24, (corner | 0xf00) as u16);
            m.sw(packet + 0xc, (clut << 16) | u);
            let size = lh(m, entry + 4);
            let size_x = widescreen::squeeze_x(m, size);
            let vertices: [(u32, bool, bool); 4] = [
                (8, false, false),
                (0x10, true, false),
                (0x18, false, true),
                (0x20, true, true),
            ];
            for (offset, right, low) in vertices {
                let cx = lhu(m, state + 0x1c);
                let x = if right {
                    cx.wrapping_add(size_x)
                } else {
                    cx.wrapping_sub(size_x)
                };
                m.sh(packet + offset, x as u16);
                let cy = lhu(m, state + 0x1e);
                let y = if low {
                    cy.wrapping_add(size)
                } else {
                    cy.wrapping_sub(size)
                };
                m.sh(packet + offset + 2, y as u16);
            }
            packet = next;
        }
    }
    let slot = m.lw(cursors);
    let first = m.lw(cursors + 4);
    insert_chain(m, slot, first, packet.wrapping_sub(0x28));
    m.sw(cursors + 4, packet);
    m.pop_frame(8);
}

// JP-REVIEW(R0393): near function: PAL 8008AE18 -> JP 8008C644, imm:2, 2 instructions differ (whole routine)
/// 8008C644 (work, record): the heat shimmer of a lit object (callback
/// 8008C0A0, record +0x10 mask): up to 16 points interpolated from the
/// record's +0x14 offset toward its far colour by a decaying spread,
/// jittered by the noise halfwords at `[gp+0x41C]`, each a VRAM strip copy
/// of the frame below it and a 16x5 sprite drawn back over it; chained
/// between texture page packets at the record's depth slot.
// JP-PATCH: replaced by `crate::jp::res_p2_render::heat_shimmer` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn heat_shimmer(m: &mut Machine, work: u32, record: u32) {
    crate::jp::res_p2_render::heat_shimmer(m, work, record)
}

/// SDK OuterProduct0 (800A72E4), by original address until `sdk.rs` has
/// it: `out = a x b` (GTE OP sf=0 with the rotation diagonal borrowed and
/// restored).
fn outer_product0(m: &mut Machine, a: u32, b: u32, out: u32) {
    let r11r12 = m.gte.cfc2(ctrl::R11R12);
    let r22r23 = m.gte.cfc2(ctrl::R22R23);
    let r33 = m.gte.cfc2(ctrl::R33);
    let x = m.lw(a);
    let y = m.lw(a + 4);
    let z = m.lw(a + 8);
    m.gte.ctc2(ctrl::R11R12, x);
    m.gte.ctc2(ctrl::R22R23, y);
    m.gte.ctc2(ctrl::R33, z);
    lwc2(m, reg::IR3, b + 8);
    lwc2(m, reg::IR1, b);
    lwc2(m, reg::IR2, b + 4);
    cop2(m, OP0);
    store_mac123(m, out);
    m.gte.ctc2(ctrl::R11R12, r11r12);
    m.gte.ctc2(ctrl::R22R23, r22r23);
    m.gte.ctc2(ctrl::R33, r33);
}

/// 80092014 (normal, point) -> the signed distance of `point` from the
/// plane through the eye with normal `normal`: the dot product over the
/// normal's length (GTE SQR and LZCS, inverse square root table
/// 800B6DD4); 0x7FFFFFFF for a zero normal.
fn plane_distance(m: &mut Machine, normal: u32, point: u32) -> u32 {
    let nx = m.lw(normal);
    let ny = m.lw(normal + 4);
    let nz = m.lw(normal + 8);
    m.gte.mtc2(reg::IR1, nx);
    m.gte.mtc2(reg::IR2, ny);
    m.gte.mtc2(reg::IR3, nz);
    let px = m.lw(point);
    let py = m.lw(point + 4);
    cop2(m, SQR0);
    let dx = mul_lo(nx, px);
    let dy = mul_lo(py, ny);
    let pz = m.lw(point + 8);
    let dot = dx.wrapping_add(dy).wrapping_add(mul_lo(pz, nz));
    let sx = m.gte.mfc2(reg::MAC1);
    let sy = m.gte.mfc2(reg::MAC2);
    let sz = m.gte.mfc2(reg::MAC3);
    let squared = sx.wrapping_add(sy).wrapping_add(sz);
    m.gte.mtc2(reg::LZCS, squared);
    if !slt(0, squared) {
        return 0x7fff_ffff;
    }
    let zeros = m.gte.mfc2(reg::LZCR) & 0xfe;
    let index = if slt(zeros, 0x18) {
        sra(squared, 0x18 - zeros)
    } else {
        sllv(squared, zeros - 0x18)
    };
    let root = lh(
        m,
        0x800b_6dd4u32.wrapping_add(index.wrapping_sub(0x40) << 1),
    );
    let shift = sra(0x1fu32.wrapping_sub(zeros), 1);
    let length = sra(sllv(root, shift), 12);
    div_checked(m, dot, length, 0x8009_20e8)
}

/// 8009211C (from, to, out, t): `out = from + t (to - from)` (GTE INTPL
/// with `from` as IR and `to` as the far colour).
fn interpolate_point(m: &mut Machine, from: u32, to: u32, out: u32, t: u32) {
    m.gte.mtc2(reg::IR0, t);
    lwc2(m, reg::IR1, from);
    lwc2(m, reg::IR2, from + 4);
    lwc2(m, reg::IR3, from + 8);
    let y = m.lw(to + 4);
    let z = m.lw(to + 8);
    let x = m.lw(to);
    m.gte.ctc2(ctrl::RFC, x);
    m.gte.ctc2(ctrl::GFC, y);
    m.gte.ctc2(ctrl::BFC, z);
    cop2(m, INTPL);
    store_mac123(m, out);
}

// JP-REVIEW(R0394): near function: PAL 800908F8 -> JP 80092168, imm:2, 2 instructions differ (whole routine)
/// 80092168 (a, b, near, planes) -> 1 when the view-space segment a..b is
/// at least partly visible, clipped in place to z >= `near` and to the
/// four side planes of the frustum (corners (+-0xB8, +-0x98, H) at
/// `planes`, the current plane normal at `planes + 0x40`); 0 when it is
/// outside.
// JP-PATCH: replaced by `crate::jp::res_p2_render::clip_segment` (src/jp/mod.rs REPLACES, PATCHES.md)
fn clip_segment(m: &mut Machine, a: u32, b: u32, near: u32, planes: u32) -> u32 {
    crate::jp::res_p2_render::clip_segment(m, a, b, near, planes)
}

/// The 2D rotation of the explosion trail ends: the GPF-scaled direction
/// (MAC1, MAC2) as rows (c, -s, 0), (s, c, 0), (0, 0, 1).
fn trail_rotation(m: &mut Machine) {
    let x = m.gte.mfc2(reg::MAC1);
    let y = m.gte.mfc2(reg::MAC2);
    let low = y & 0xffff;
    m.gte.ctc2(ctrl::R11R12, (x.wrapping_neg() << 16) | low);
    m.gte.ctc2(ctrl::R22R23, low);
    m.gte.ctc2(ctrl::R13R21, x << 16);
    m.gte.ctc2(ctrl::R31R32, 0);
    m.gte.ctc2(ctrl::R33, 0x1000);
}

/// The trail end's screen outcode (terrain 800965D4) of the screen point
/// word at `point`.
fn outcode(m: &mut Machine, point: u32) -> u32 {
    dispatch::call(m, 0x8009_65d4, [point, 0, 0, 0])
}

/// One translucent gouraud quad of the explosion trail (code 0x3A,
/// colours 0x020202 except `colour` at the second vertex).
fn trail_quad(m: &mut Machine, packet: u32, xy: [u32; 4], colour: u32) -> u32 {
    let next = packet + 0x24;
    m.sw(packet, link(next) | 0x0800_0000);
    m.sw(packet + 8, xy[0]);
    m.sw(packet + 0x10, xy[1]);
    m.sw(packet + 0xc, colour);
    for offset in [0x1cu32, 0x14, 4] {
        m.sw(packet + offset, 0x3a02_0202);
    }
    m.sw(packet + 0x18, xy[2]);
    m.sw(packet + 0x20, xy[3]);
    next
}

/// 800916A8 (prim, work, end) -> next primitive: the half fan of an
/// explosion trail end: three quads from the end's centre (+0xC, colour
/// +4) out to its radius +0 along the sweep work +0x24 from the
/// direction work +0x20 (thirds through the sine table), the far points
/// kept at +0x14/+0x1C with their outcodes (+0x10/+0x18). A zero radius
/// only records the outcodes.
fn trail_fan(m: &mut Machine, prim: u32, work: u32, end: u32) -> u32 {
    m.push_frame(0x30);
    let radius = m.lw(end);
    let centre = m.lw(end + 0xc);
    let mut prim = prim;
    if radius == 0 {
        let code = outcode(m, end + 0xc);
        m.sw(end + 8, code);
        let code = m.lw(end + 8);
        let xy = m.lw(end + 0xc);
        m.sw(end + 0x18, code);
        m.sw(end + 0x1c, xy);
        let code = m.lw(end + 0x18);
        let xy = m.lw(end + 0x1c);
        m.sw(end + 0x10, code);
        m.sw(end + 0x14, xy);
    } else {
        // Widescreen: RTPS as a 2D transform (depth H) squeezes the
        // translation too; the squeezed centre goes in unsqueezed.
        let tx = widescreen::unsqueeze_x(m, sext16(centre));
        m.gte.ctc2(ctrl::TRX, tx);
        let angle = m.lw(work + 0x20);
        m.gte.ctc2(ctrl::TRY, sra(centre, 16));
        let direction = m.lw(SINES + ((angle & 0xfff) << 2));
        m.gte.mtc2(reg::IR0, radius);
        m.gte.mtc2(reg::IR1, sext16(direction));
        m.gte.mtc2(reg::IR2, sra(direction, 16));
        cop2(m, GPF);
        let mut code = outcode(m, end + 0xc);
        trail_rotation(m);
        m.gte.mtc2(reg::VXY0, 0x1000_0000);
        m.sw(end + 8, code);
        let sweep = m.lw(work + 0x24);
        cop2(m, RTPS);
        let twice = sweep << 1;
        let two_thirds = mul_hi(twice, 0x5555_5556).wrapping_sub(sra(twice, 31));
        let third = mul_hi(sweep, 0x5555_5556).wrapping_sub(sra(sweep, 31));
        let full = m.lw(SINES + ((sweep & 0xfff) << 2));
        let second = m.lw(SINES + ((two_thirds & 0xfff) << 2));
        let first = m.lw(SINES + ((third & 0xfff) << 2));
        m.sw(work + 0x2c, second);
        m.sw(work + 0x30, full);
        m.sw(work + 0x28, first);
        swc2(m, reg::SXY2, work + 0x34);
        lwc2(m, reg::VXY0, work + 0x28);
        lwc2(m, reg::VXY1, work + 0x2c);
        lwc2(m, reg::VXY2, work + 0x30);
        let x0 = lh(m, work + 0x28);
        let x1 = lh(m, work + 0x2c);
        cop2(m, RTPT);
        let x2 = lh(m, work + 0x30);
        m.sh(work + 0x28, x0.wrapping_neg() as u16);
        m.sh(work + 0x2c, x1.wrapping_neg() as u16);
        m.sh(work + 0x30, x2.wrapping_neg() as u16);
        if code != 0 {
            let start = outcode(m, work + 0x34);
            m.sw(work + 0x40, start);
        }
        swc2(m, reg::SXY0, work + 0x38);
        swc2(m, reg::SXY1, work + 0x3c);
        swc2(m, reg::SXY2, end + 0x14);
        lwc2(m, reg::VXY0, work + 0x28);
        lwc2(m, reg::VXY1, work + 0x2c);
        lwc2(m, reg::VXY2, work + 0x30);
        let far = outcode(m, end + 0x14);
        cop2(m, RTPT);
        m.sw(end + 0x10, far);
        let hidden = code != 0 && {
            let near = outcode(m, work + 0x38);
            m.sw(work + 0x44, near);
            let mut common = near & code & far;
            if common != 0 {
                common &= outcode(m, work + 0x3c);
            }
            code & common != 0
        };
        if !hidden {
            let xy = [
                m.lw(end + 0x14),
                m.lw(end + 0xc),
                m.lw(work + 0x3c),
                m.lw(work + 0x38),
            ];
            let colour = m.lw(end + 4);
            prim = trail_quad(m, prim, xy, colour);
        }
        swc2(m, reg::SXY0, work + 0x3c);
        let hidden = code != 0 && {
            code &= outcode(m, work + 0x3c);
            code & m.lw(work + 0x40) & m.lw(work + 0x44) != 0
        };
        if !hidden {
            let xy = [
                m.lw(work + 0x38),
                m.lw(end + 0xc),
                m.lw(work + 0x34),
                m.lw(work + 0x3c),
            ];
            let colour = m.lw(end + 4);
            prim = trail_quad(m, prim, xy, colour);
        }
        swc2(m, reg::SXY1, work + 0x38);
        swc2(m, reg::SXY2, end + 0x1c);
        let last = outcode(m, end + 0x1c);
        code &= last;
        m.sw(end + 0x18, last);
        let hidden = code != 0 && {
            code &= outcode(m, work + 0x38);
            code != 0
        };
        if !hidden {
            let xy = [
                m.lw(end + 0x1c),
                m.lw(end + 0xc),
                m.lw(work + 0x38),
                m.lw(work + 0x3c),
            ];
            let colour = m.lw(end + 4);
            prim = trail_quad(m, prim, xy, colour);
        }
    }
    m.pop_frame(0x30);
    prim
}

/// 80091A70 (prim, work, end) -> next primitive: the full round cap of an
/// explosion trail end (when one end's radius covers the other): eight
/// quads of three points each around the centre (+0xC, colour +4) at the
/// radius +0, rotated by the direction work +0x20.
fn trail_cap(m: &mut Machine, prim: u32, work: u32, end: u32) -> u32 {
    m.push_frame(0x38);
    let radius = m.lw(end);
    let centre = m.lw(end + 0xc);
    let mut prim = prim;
    if radius != 0 {
        // Widescreen: as in trail_fan, the centre goes in unsqueezed.
        let tx = widescreen::unsqueeze_x(m, sext16(centre));
        m.gte.ctc2(ctrl::TRX, tx);
        let angle = m.lw(work + 0x20);
        m.gte.ctc2(ctrl::TRY, sra(centre, 16));
        let direction = m.lw(SINES + ((angle & 0xfff) << 2));
        m.gte.mtc2(reg::IR0, radius);
        m.gte.mtc2(reg::IR1, sext16(direction));
        m.gte.mtc2(reg::IR2, sra(direction, 16));
        m.sw(work + 0x28, 0x1000_0000);
        cop2(m, GPF);
        let code = outcode(m, end + 0xc);
        trail_rotation(m);
        m.sw(work + 0x2c, 0x0ec8_061f);
        m.sw(work + 0x30, 0x0b50_0b50);
        let mut offset = 0x400u32;
        for segment in 0..8u32 {
            lwc2(m, reg::VXY0, work + 0x28);
            lwc2(m, reg::VXY1, work + 0x2c);
            lwc2(m, reg::VXY2, work + 0x30);
            let colour = m.lw(end + 4);
            let xy = m.lw(end + 0xc);
            cop2(m, RTPT);
            m.sw(prim + 0x14, 0x0003_0303);
            m.sw(prim + 0x1c, 0x0003_0303);
            m.sw(prim + 4, 0x3a03_0303);
            m.sw(prim + 0xc, colour);
            m.sw(prim + 0x10, xy);
            if segment & 1 != 0 {
                let a = m.lw(SINES + offset);
                let b = m.lw(SINES + offset + 0x400);
                let c = m.lw(SINES + 0x800 + offset);
                m.sw(work + 0x28, a);
                m.sw(work + 0x2c, b);
                m.sw(work + 0x30, c);
            } else {
                let a = lh(m, work + 0x28);
                let b = lh(m, work + 0x2c);
                let c = lh(m, work + 0x30);
                m.sh(work + 0x28, a.wrapping_neg() as u16);
                m.sh(work + 0x2c, b.wrapping_neg() as u16);
                m.sh(work + 0x30, c.wrapping_neg() as u16);
            }
            let visible = if code != 0 {
                swc2(m, reg::SXY0, work + 0x34);
                swc2(m, reg::SXY1, work + 0x38);
                swc2(m, reg::SXY2, work + 0x3c);
                let mut common = code & outcode(m, work + 0x34);
                if common != 0 {
                    common &= outcode(m, work + 0x38);
                    if common != 0 {
                        common &= outcode(m, work + 0x3c);
                    }
                }
                if common == 0 {
                    let a = m.lw(work + 0x34);
                    let b = m.lw(work + 0x38);
                    let c = m.lw(work + 0x3c);
                    m.sw(prim + 8, a);
                    m.sw(prim + 0x18, b);
                    m.sw(prim + 0x20, c);
                }
                common == 0
            } else {
                swc2(m, reg::SXY0, prim + 8);
                swc2(m, reg::SXY1, prim + 0x18);
                swc2(m, reg::SXY2, prim + 0x20);
                true
            };
            if visible {
                let next = prim + 0x24;
                m.sw(prim, link(next) | 0x0800_0000);
                prim = next;
            }
            offset += 0x400;
        }
    }
    m.pop_frame(0x38);
    prim
}

/// The two sides of an explosion trail between ends `a` and `b`: a quad
/// from the centres to the far points named by `offsets` (`a + offsets.0`,
/// `b + offsets.1`) unless all four outcodes share a side.
fn trail_side(m: &mut Machine, prim: u32, a: u32, b: u32, offsets: (u32, u32)) -> u32 {
    let next = prim + 0x24;
    m.sw(prim, link(next) | 0x0800_0000);
    let xy = m.lw(a + 0xc);
    let other = m.lw(b + 0xc);
    m.sw(prim + 8, xy);
    m.sw(prim + 0x10, other);
    let xy = m.lw(a + offsets.0);
    let other = m.lw(b + offsets.1);
    m.sw(prim + 0x18, xy);
    m.sw(prim + 0x20, other);
    let colour = m.lw(a + 4);
    let other = m.lw(b + 4);
    m.sw(prim + 0x1c, 0x0002_0202);
    m.sw(prim + 0x14, 0x0002_0202);
    m.sw(prim + 4, link(colour) | 0x3a00_0000);
    m.sw(prim + 0xc, other);
    next
}

/// 80091CE8 (ot, a, b, work): one glow layer of an explosion trail from
/// end `a` to end `b` (radius +0, colour +4, screen centre +0xC): a round
/// cap (80091A70) when one radius covers the distance, else the two half
/// fans (800916A8) at the tangent angle (80030EB8) and the two sides;
/// chained at `ot`, the view rotation saved and restored around it.
fn trail_layer(m: &mut Machine, ot: u32, a: u32, b: u32, work: u32) {
    m.push_frame(0x38);
    let dx = lh(m, a + 0xc).wrapping_sub(lh(m, b + 0xc));
    let dy = lh(m, a + 0xe).wrapping_sub(lh(m, b + 0xe));
    let angle = sdk::ratan2(m, dx.wrapping_neg(), dy);
    m.sw(work + 0x20, angle);
    let length = dispatch::call(m, 0x8008_3f40, [dx, dy, 0, 0]);
    save_rotation(m, work);
    for (index, register) in [ctrl::TRX, ctrl::TRY, ctrl::TRZ].into_iter().enumerate() {
        let value = m.gte.cfc2(register);
        m.sw(work + 0x14 + index as u32 * 4, value);
    }
    let depth = lhu(m, SCRATCH + 0x1a);
    m.gte.mtc2(reg::VZ0, depth);
    m.gte.mtc2(reg::VZ1, depth);
    m.gte.mtc2(reg::VZ2, depth);
    m.gte.ctc2(ctrl::TRZ, 0);
    let difference = m.lw(a).wrapping_sub(m.lw(b));
    let mut cursor = m.lw(SCRATCH + 4);
    if !slt(abs(difference), length) {
        let end = if slt(difference, 0) { b } else { a };
        cursor = trail_cap(m, cursor, work, end);
    } else {
        let ratio = div_checked(m, difference << 14, length, 0x8009_1e0c);
        let tangent = sra(dispatch::call(m, 0x8003_0eb8, [ratio, 0, 0, 0]), 2);
        m.sw(work + 0x24, tangent.wrapping_add(0x400));
        cursor = trail_fan(m, cursor, work, a);
        let angle = m.lw(work + 0x20);
        m.sw(work + 0x24, 0x400u32.wrapping_sub(tangent));
        m.sw(work + 0x20, angle.wrapping_add(0x800));
        cursor = trail_fan(m, cursor, work, b);
        let both = m.lw(a + 8) & m.lw(b + 8);
        if both & m.lw(a + 0x10) & m.lw(b + 0x18) == 0 {
            cursor = trail_side(m, cursor, a, b, (0x14, 0x1c));
        }
        if both & m.lw(a + 0x18) & m.lw(b + 0x10) == 0 {
            cursor = trail_side(m, cursor, a, b, (0x1c, 0x14));
        }
    }
    let first = m.lw(SCRATCH + 4);
    if first != cursor {
        insert_chain(m, ot, first, cursor - 0x24);
        m.sw(SCRATCH + 4, cursor);
    }
    load_rotation(m, work);
    m.gte.ctc2(ctrl::TRX, 0);
    m.gte.ctc2(ctrl::TRY, 0);
    m.pop_frame(0x38);
}

/// 8009236C (view, work, explosion): one explosion trail (28-byte record:
/// +0 phase, +2 kind, +4 and +0x10 the two world end points): both ends
/// relative to the eye (>> 12, or >> 15 for kind 2; skipped beyond 16
/// bits), rotated into view space and clipped (80092168), projected,
/// depth-sorted by the nearer end, coloured by depth cueing with the
/// pulse (sine of +0, or +0 itself for kind 3), sized by the projection
/// distance over depth, and drawn as an outer and an inner glow layer
/// (80091CE8).
fn draw_explosion(m: &mut Machine, view: u32, work: u32, explosion: u32) {
    m.push_frame(0x38);
    'body: {
        let kind = lh(m, explosion + 2);
        let phase = lh(m, explosion);
        let shift = if kind == 2 { 0xf } else { 0xc };
        let eye = [m.lw(view + 8), m.lw(view + 0xc), m.lw(view + 0x10)];
        let fits = |value: u32| value.wrapping_add(0x8000) <= 0xffff;
        let mut relative = [0u32; 3];
        for (i, value) in relative.iter_mut().enumerate() {
            let world = m.lw(explosion + 4 + i as u32 * 4);
            *value = sra(world.wrapping_sub(eye[i]), shift);
            if !fits(*value) {
                break 'body;
            }
        }
        for (i, value) in relative.into_iter().enumerate() {
            m.gte.mtc2(reg::IR1 + i, value);
        }
        let world = m.lw(explosion + 0x10);
        let x = sra(world.wrapping_sub(eye[0]), shift);
        cop2(m, MVMVA_RT_IR);
        if !fits(x) {
            break 'body;
        }
        let world = m.lw(explosion + 0x14);
        let y = sra(world.wrapping_sub(eye[1]), shift);
        if !fits(y) {
            break 'body;
        }
        let world = m.lw(explosion + 0x18);
        let z = sra(world.wrapping_sub(eye[2]), shift);
        if !fits(z) {
            break 'body;
        }
        store_mac123(m, work + 4);
        m.gte.mtc2(reg::IR1, x);
        m.gte.mtc2(reg::IR2, y);
        m.gte.mtc2(reg::IR3, z);
        let mut near = sra(lhu(m, view + 0x1a) << 16, 18);
        cop2(m, MVMVA_RT_IR);
        if !slt(0, near) {
            near = 1;
        }
        store_mac123(m, work + 0x14);
        if clip_segment(m, work + 4, work + 0x14, near, work + 0x24) == 0 {
            break 'body;
        }
        m.sw(work + 0x11c, 0x1000);
        m.sw(work + 0x124, 0x1000);
        m.sw(work + 0x12c, 0x1000);
        m.sw(work + 0x120, 0);
        m.sw(work + 0x128, 0);
        load_rotation(m, work + 0x11c);
        let y0 = m.lw(work + 8);
        let mut z0 = m.lw(work + 0xc);
        let xy0 = lhu(m, work + 4) | (y0 << 16);
        m.gte.mtc2(reg::VXY0, xy0);
        m.gte.mtc2(reg::VZ0, z0);
        let y1 = m.lw(work + 0x18);
        let mut z1 = m.lw(work + 0x1c);
        let xy1 = lhu(m, work + 0x14) | (y1 << 16);
        m.gte.mtc2(reg::VXY1, xy1);
        m.gte.mtc2(reg::VZ1, z1);
        let nearest = if slt(z0, z1) { z0 } else { z1 };
        cop2(m, RTPT);
        let mut depth = sra(nearest, 0xfu32.wrapping_sub(shift));
        if !slt(depth, 0xfff) {
            depth = 0xffe;
        } else if slt(depth, 0) {
            depth = 0;
        }
        let ot = m.lw(view).wrapping_add(depth << 2);
        m.sw(work, ot);
        let pulse = if kind == 3 {
            phase
        } else {
            let sine = lh(m, SINES + ((phase & 0xfff) << 2));
            if sine == 0 { 0xffff_f000 } else { sine }
        };
        swc2(m, reg::SXY0, work + 0x80);
        swc2(m, reg::SXY1, work + 0xa0);
        load_rotation(m, work + 0xfc);
        m.gte.ctc2(ctrl::RFC, 0x30);
        m.gte.ctc2(ctrl::GFC, 0x30);
        m.gte.ctc2(ctrl::BFC, 0x30);
        m.gte.mtc2(reg::IR0, pulse);
        m.gte.mtc2(reg::RGBC, 0);
        m.gte.mtc2(reg::RGB0, 0x0027_9327);
        m.gte.mtc2(reg::RGB1, 0x0083_8383);
        if z0 == 0 {
            z0 = 1;
        }
        if z1 == 0 {
            z1 = 1;
        }
        cop2(m, DPCT);
        let distance = lh(m, view + 0x1a);
        let (mut size0, mut size1) = if kind == 3 {
            let near_size = mul_lo(lbu(m, explosion + 4), distance);
            let size0 = div_checked(m, near_size, z0, 0x8009_26a4);
            let far_size = mul_lo(lbu(m, explosion + 8), distance);
            let size1 = div_checked(m, far_size, z1, 0x8009_26e8);
            (size0, size1)
        } else {
            let base = mul_lo(if kind == 2 { 0xc } else { 8 }, distance);
            let size0 = div_checked(m, base, z0, 0x8009_2740);
            let size1 = div_checked(m, base, z1, 0x8009_2774);
            (size0, size1)
        };
        if slt(size0, 2) {
            size0 = 2;
        }
        if slt(size1, 2) {
            size1 = 2;
        }
        m.sw(work + 0x74, size0);
        m.sw(work + 0x94, size1);
        let outer = m.gte.mfc2(reg::RGB0);
        let inner = m.gte.mfc2(reg::RGB1);
        let ot = m.lw(work);
        m.sw(work + 0x98, outer);
        m.sw(work + 0x78, outer);
        trail_layer(m, ot, work + 0x74, work + 0x94, work + 0xb4);
        let ot = m.lw(work);
        m.sw(work + 0x74, sra(size0, 1));
        m.sw(work + 0x98, inner);
        m.sw(work + 0x78, inner);
        m.sw(work + 0x94, sra(size1, 1));
        trail_layer(m, ot, work + 0x74, work + 0x94, work + 0xb4);
    }
    m.pop_frame(0x38);
}

// ==== phase 4: the table-reached renderers ==================================================

/// The OT slot of an object's screen record (`[work]` + depth * 4).
fn screen_slot(m: &mut Machine, work: u32, screen: u32) -> u32 {
    let depth = lh(m, screen + 4);
    let ot = m.lw(work);
    (depth << 2).wrapping_add(ot)
}

/// 80085EAC (work, screen, object): object class 2: an animated sprite
/// (frames of 32 texels across the 8- or 6-step animation of the object's
/// age over its lifetime, rows by the kind work +0xC), colour work +0x60,
/// half sizes from the projected size (1.5x below row 0x40), corners by
/// 8008591C.
fn draw_spark_sprite(m: &mut Machine, work: u32, screen: u32, object: u32) {
    m.push_frame(0x18);
    let state = m.lw(object);
    let kind = m.lw(work + 0xc);
    let packet = m.lw(work + 4);
    let mut life = sra(state, 16);
    let age = sra(state << 16, 16);
    if life == 0 {
        life = 1;
    }
    m.sh(
        packet + 0xe,
        ((kind.wrapping_add(0x104) << 6) | 0x16) as u16,
    );
    let steps = if kind == 3 {
        age << 3
    } else {
        (age << 1).wrapping_add(age) << 1
    };
    let u = div_checked(m, steps, life, 0x8008_5f08) << 5;
    let v = if slt(kind, 2) {
        0
    } else if kind == 3 {
        0x40
    } else {
        0x20
    };
    let top = v << 8;
    let right = u.wrapping_add(0x1f);
    m.sh(packet + 0x14, (top | right) as u16);
    let bottom = v.wrapping_add(0x1f) << 8;
    m.sh(packet + 0xc, (top | u) as u16);
    m.sh(packet + 0x1c, (bottom | u) as u16);
    m.sh(packet + 0x24, (bottom | right) as u16);
    let flags = m.lw(work + 8);
    let colour = m.lw(work + 0x60);
    m.sw(packet + 4, colour);
    let clut = if flags & 1 == 0 {
        ((flags & 3) << 5) | 0x18
    } else {
        0x38
    };
    m.sh(packet + 0x16, clut as u16);
    let next = link_sprite(m, work, screen, packet, flags);
    m.sw(work + 4, next);
    let raw = u32::from(m.lhu(screen + 6)) << 16;
    let mut size = sra(raw, 16);
    m.sh(work + 0x14, size as u16);
    if slt(v, 0x40) {
        size = size.wrapping_add(sra(raw, 17));
    }
    let offset = sra(sra(size, 3).wrapping_sub(size), 1);
    m.sh(work + 0x16, size as u16);
    m.sh(work + 0x18, 0);
    m.sh(work + 0x1a, offset as u16);
    let centre = m.lw(screen);
    m.sw(work + 0x1c, centre);
    sprite_corners(m, work, packet);
    m.pop_frame(0x18);
}

/// 800860A4 (work, screen, object): object class 3: a pulsing glow: the
/// object's age over its lifetime drives a cosine swell (table 800B7106),
/// depth-cued from colour work +0x60 toward 4x itself, the sprite growing
/// to its full size over the first half, a 64- or 32-texel cell chosen by
/// the lifetime's parity and the age; corners by 8008591C.
fn draw_glow_object(m: &mut Machine, work: u32, screen: u32, object: u32) {
    m.push_frame(0x20);
    let state = m.lw(object);
    let packet = m.lw(work + 4);
    let mut life = sra(state, 16);
    let age = sra(state << 16, 16);
    m.sh(work + 0x20, (age.wrapping_add(life) & 2) as u16);
    m.sh(work + 0x22, (((life & 1) << 6) + 0x80) as u16);
    if life == 0 {
        life = 1;
    }
    let phase = div_checked(m, age << 12, life, 0x8008_60fc);
    let early = slt(phase, 0x800);
    let swell = if early {
        let cosine = lh(m, ((phase << 1) & 0x3ffc).wrapping_add(0x800b_7106));
        0x1000u32.wrapping_sub(cosine)
    } else {
        lh(
            m,
            ((phase.wrapping_sub(0x800) << 1) & 0x3ffc).wrapping_add(0x800b_7106),
        )
    };
    let level = 0x1200u32.wrapping_sub(sra(swell, 1));
    let colour = m.lw(work + 0x60);
    m.gte.mtc2(reg::IR0, level);
    m.gte.mtc2(reg::RGBC, colour);
    m.gte.ctc2(ctrl::RFC, (colour & 0xff) << 2);
    let high = sra(colour, 8);
    m.gte.ctc2(ctrl::GFC, (high & 0xff) << 2);
    m.gte.ctc2(ctrl::BFC, sra(high, 6) & 0x3fc);
    let size = lh(m, screen + 6);
    let width = if early {
        let product = mul_lo(size, phase.wrapping_add(0x2aa));
        let scaled = mul_hi(product, 0xc00c_00c1).wrapping_add(product);
        sra(scaled, 11).wrapping_sub(sra(product, 31))
    } else {
        size
    };
    cop2(m, DPCS);
    let product = mul_lo(size, swell.wrapping_add(0x2aa));
    let product = if slt(product, 0) {
        product.wrapping_add(0xfff)
    } else {
        product
    };
    let height = sra(product, 12);
    m.sh(work + 0x14, width as u16);
    let small = u32::from(slt(height, 0x11));
    let offset = sra(sra(product, 15).wrapping_sub(height), 1);
    m.sh(work + 0x16, height as u16);
    let centre = m.lw(screen);
    m.sh(work + 0x1a, offset as u16);
    m.sh(work + 0x18, 0);
    m.sw(work + 0x1c, centre);
    m.sh(packet + 0xe, 0x40d6);
    m.sh(packet + 0x16, 0x37);
    let bottom = sra(0x40, small).wrapping_sub(1);
    m.sb(packet + 0x15, 0);
    m.sb(packet + 0xd, 0);
    m.sb(packet + 0x25, bottom as u8);
    m.sb(packet + 0x1d, bottom as u8);
    let flip = lh(m, work + 0x20);
    let u = lh(m, work + 0x22);
    let near = sra(u, small);
    let far = sra(u.wrapping_add(0x40), small).wrapping_sub(1);
    let (left, right) = if flip != 0 { (near, far) } else { (far, near) };
    m.sb(packet + 0x1c, left as u8);
    m.sb(packet + 0xc, left as u8);
    m.sb(packet + 0x24, right as u8);
    m.sb(packet + 0x14, right as u8);
    swc2(m, reg::RGB2, packet + 4);
    sprite_corners(m, work, packet);
    let slot = screen_slot(m, work, screen);
    let tag = m.lw(slot) | 0x0900_0000;
    m.sw(packet, tag);
    let slot = screen_slot(m, work, screen);
    m.sw(slot, link(packet));
    m.sw(work + 4, packet + 0x28);
    m.pop_frame(0x20);
}

/// `(value * radius) / 4096` rounded toward zero.
fn scale_q12(value: u32, radius: u32) -> u32 {
    let product = mul_lo(value, radius);
    sra(
        if slt(product, 0) {
            product.wrapping_add(0xfff)
        } else {
            product
        },
        12,
    )
}

// JP-REVIEW(R0399): near function: PAL 800887D4 -> JP 80089F44, imm:1, 1 instructions differ (whole routine)
/// 80089F44 (piece, work): debris kind 0, a fading spark star: projected
/// from the eye, grey by the remaining life over its second half (faded
/// beyond depth 10000), the radius by the piece size +0x16, the projection
/// distance and the sine of its age; a dot below radius 2, else a fan of
/// gouraud triangles (below 25) or a ring of quads around a hole, chained
/// with the draw-mode packet work +0x78 at its depth slot.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_debris_star` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_debris_star(m: &mut Machine, piece: u32, work: u32) {
    crate::jp::res_p2_render::draw_debris_star(m, piece, work)
}

// JP-REVIEW(R0400): near function: PAL 80089480 -> JP 8008ABF0, imm:1, 1 instructions differ (whole routine)
/// 8008ABF0 (piece, work): debris kind 2, a glowing ember: projected from
/// the eye, the radius by the piece size, the projection distance and the
/// sine of a quarter of its age over its life (at most 180), skipped off
/// screen; the colour 0x6F2FAF depth-cued toward 0x30 then by the age; a
/// fan of gouraud triangles between texture-page packets at its depth slot.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_debris_ember` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_debris_ember(m: &mut Machine, piece: u32, work: u32) {
    crate::jp::res_p2_render::draw_debris_ember(m, piece, work)
}

// ---- phase 4: the ambient renderers (table 800B4388) --------------------------------------

/// The wrapped ambient particle coordinate `((base + offset) & 0x1FFF) -
/// 0x1000`.
fn ambient_wrap(base: u32, offset: u32) -> u32 {
    (base.wrapping_add(offset) & 0x1fff).wrapping_sub(0x1000)
}

/// Store the ambient particle point at `at` (three halfwords) from the
/// eye-relative base and the random word `seed` (x from it, y from >> 8,
/// z from >> 16, sign-extending).
fn ambient_point(m: &mut Machine, at: u32, base: [u32; 3], seed: u32) {
    let x = ambient_wrap(base[0], seed);
    let seed = sra(seed, 8);
    let y = ambient_wrap(base[1], seed);
    let seed = sra(seed, 8);
    m.sh(at, x as u16);
    let z = ambient_wrap(base[2], seed);
    m.sh(at + 2, y as u16);
    m.sh(at + 4, z as u16);
}

/// The ambient cube base: the eye (>> `shift`, negated) less the view
/// direction (gameplay +0x2C rotation of (0, 0, 0x2000)), plus `lift` on
/// y; the GTE rotation is saved at work +0 around it and TRZ = 0x2000.
fn ambient_base(m: &mut Machine, shift: u32, lift: u32) -> [u32; 3] {
    let gameplay = m.gw(GAMEPLAY);
    load_rotation(m, gameplay + 0x2c);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0x2000);
    cop2(m, MVMVA_RT_V0);
    let mut base = [0u32; 3];
    for (axis, value) in base.iter_mut().enumerate() {
        *value = sra(m.lw(SCRATCH + 8 + axis as u32 * 4).wrapping_neg(), shift);
    }
    base[1] = base[1].wrapping_add(lift);
    for (axis, value) in base.iter_mut().enumerate() {
        *value = value.wrapping_sub(m.gte.mfc2(reg::MAC1 + axis));
    }
    load_rotation(m, WORK);
    m.gte.ctc2(ctrl::TRZ, 0x2000);
    base
}

/// The current ends of the ambient streaks: `blocks` packets of three
/// LINE_G2 from `first` (0x34 bytes each), points of the wrapped cube
/// around `base` stepped by the random words of `[gp+0x41C]`, depth-cued
/// (colour RGBC) by their distance from the cube centre.
fn ambient_lines(m: &mut Machine, first: u32, base: [u32; 3], blocks: u32) {
    let mut seeds = m.gw(OBJECT_SYSTEMS);
    let point = WORK + 0x20;
    for block in 0..blocks {
        let lines = first + block * 0x34;
        for line in 0..3u32 {
            let segment = lines + line * 0x10;
            lwc2(m, reg::VXY0, point);
            lwc2(m, reg::VZ0, point + 4);
            let seed = m.lw(seeds);
            seeds += 4;
            cop2(m, RTPS);
            ambient_point(m, point, base, seed);
            let depth = m.gte.mfc2(reg::SZ3);
            let screen = m.gte.mfc2(reg::SXY2);
            m.sw(segment + 8, screen);
            let mut distance = depth.wrapping_sub(0x2000);
            if slt(distance, 0) {
                distance = 0x2000u32.wrapping_sub(depth);
            }
            if !slt(distance, 0x1001) {
                distance = 0x1000;
            }
            m.gte.mtc2(reg::IR0, distance);
            cop2(m, DPCS);
            m.sw(segment + 0xc, 0x0003_0303);
            let cued = m.gte.mfc2(reg::RGB2);
            m.sw(segment + 4, cued);
        }
    }
}

/// The view of the previous frame for the streak tails: the camera's
/// angle change (scratch +0x14 less camera +0xAC..) transposed into the
/// rotation, and its motion (+0x9C..+0xA4 >> `shift`, y less `drop`) as
/// the translation (z + 0x2000).
fn ambient_motion(m: &mut Machine, shift: u32, drop: u32) {
    let gameplay = m.gw(GAMEPLAY);
    for (index, offset) in [0xacu32, 0xae, 0xb0].into_iter().enumerate() {
        let camera = m.lw(gameplay + 0x28);
        let angle = m
            .lhu(SCRATCH + 0x14 + index as u32 * 2)
            .wrapping_sub(m.lhu(camera + offset));
        m.sh(WORK + 0x20 + index as u32 * 2, angle);
    }
    shared::rotation_yxz_transposed(m, WORK + 0x20, WORK + 0x40);
    load_rotation(m, WORK + 0x40);
    let gameplay = m.gw(GAMEPLAY);
    for (index, offset) in [0x9cu32, 0xa0, 0xa4].into_iter().enumerate() {
        let camera = m.lw(gameplay + 0x28);
        let step = sra(m.lw(camera + offset), shift);
        m.sh(WORK + 0x20 + index as u32 * 2, step as u16);
        if index == 1 && drop != 0 {
            m.sh(WORK + 0x22, step.wrapping_sub(drop) as u16);
        }
    }
    lwc2(m, reg::VXY0, WORK + 0x20);
    lwc2(m, reg::VZ0, WORK + 0x24);
    cop2(m, MVMVA_RT_V0);
    let tx = m.gte.mfc2(reg::MAC1);
    m.gte.ctc2(ctrl::TRX, tx);
    let ty = m.gte.mfc2(reg::MAC2);
    m.gte.ctc2(ctrl::TRY, ty);
    let tz = m.gte.mfc2(reg::MAC3).wrapping_add(0x2000);
    m.gte.ctc2(ctrl::TRZ, tz);
}

/// The streak tails: the same points projected through the previous
/// view, three per packet (tags 0x0C), from `[scratch +4]`; returns the
/// end of the packets.
fn ambient_tails(m: &mut Machine, base: [u32; 3], blocks: u32) -> u32 {
    let mut packet = m.lw(SCRATCH + 4);
    m.sh(WORK + 0x20, ambient_wrap(base[0], 0) as u16);
    m.sh(WORK + 0x22, ambient_wrap(base[1], 0) as u16);
    let mut seeds = m.gw(OBJECT_SYSTEMS);
    m.sh(WORK + 0x24, ambient_wrap(base[2], 0) as u16);
    for at in [WORK + 0x28, WORK + 0x30] {
        let seed = m.lw(seeds);
        seeds += 4;
        ambient_point(m, at, base, seed);
    }
    for _ in 0..blocks {
        for (index, at) in [WORK + 0x20, WORK + 0x28, WORK + 0x30]
            .into_iter()
            .enumerate()
        {
            lwc2(m, reg::VXY0 + index * 2, at);
            lwc2(m, reg::VZ0 + index * 2, at + 4);
        }
        let seed = m.lw(seeds);
        seeds += 4;
        cop2(m, RTPT);
        ambient_point(m, WORK + 0x20, base, seed);
        for at in [WORK + 0x28, WORK + 0x30] {
            let seed = m.lw(seeds);
            seeds += 4;
            ambient_point(m, at, base, seed);
        }
        let next = packet + 0x34;
        m.sw(packet, link(next) | 0x0c00_0000);
        swc2(m, reg::SXY0, packet + 0x10);
        swc2(m, reg::SXY1, packet + 0x20);
        swc2(m, reg::SXY2, packet + 0x30);
        packet = next;
    }
    packet
}

// JP-REVIEW(R0401): near function: PAL 8008CC88 -> JP 8008E4F8, imm:2, 2 instructions differ (whole routine)
/// 8008E4F8: ambient type 4, rain: a grey fog TILE over the screen when
/// below the water/cloud height (`[gp+0x33C]` +4), and 64 blocks of three
/// LINE_G2 streaks: 192 points of the 0x2000-wide wrapped cube around the
/// eye (>> 10), depth-cued by their distance from the cube centre, each
/// from its projection this frame to its projection in the previous view
/// (camera motion >> 9), chained at OT slot 0xFFE.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_rain` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_rain(m: &mut Machine) {
    crate::jp::res_p2_render::draw_rain(m)
}

/// 8008EB58: ambient type 2, snow: up to 85 blocks (the weather record
/// gameplay +0x268: +0x1D count, +0x1C fade, +0xC height band) of three
/// falling LINE_G2 flakes in the wrapped cube around the eye (>> 8, lifted
/// by context +0x28), colour 0x25211D depth-cued by the fade and the
/// height (fewer flakes when high), tails from the previous view (motion
/// >> 8, falling 0x80), chained at OT slot 0x1C.
fn draw_snow(m: &mut Machine) {
    m.push_frame(0x30);
    'body: {
        let gameplay = m.gw(GAMEPLAY);
        let weather = m.lw(gameplay + 0x268);
        let band = m.lw(weather + 0xc);
        let mut count = lbu(m, weather + 0x1d);
        let fade = m.lw(weather + 0x1c);
        if count == 0 {
            break 'body;
        }
        for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
            m.gte.ctc2(register, 0x30);
        }
        m.gte.mtc2(reg::RGBC, 0x5225_211d);
        let fade = sra(fade, 16);
        m.gte.mtc2(reg::IR0, fade);
        if !slt(fade, 0x1000) {
            break 'body;
        }
        cop2(m, DPCS);
        let water = m.gw(SUN_STATE);
        let eye_y = m.lw(SCRATCH + 0xc);
        let height = eye_y.wrapping_add(m.lw(water + 4));
        if !slt(count, 0x56) {
            count = 0x55;
        }
        let level = if slt(height, 0) {
            let span = mul_lo(sra(band, 17) & 0x7f, band >> 24);
            sext16(band)
                .wrapping_sub(span)
                .wrapping_sub(sra(eye_y, 12).wrapping_add(0x800))
        } else {
            sra(height, 8)
        };
        let colour = m.gte.mfc2(reg::RGB2);
        if !slt(level, 0x1000) {
            break 'body;
        }
        m.gte.mtc2(reg::IR0, level);
        m.gte.mtc2(reg::RGBC, colour);
        if slt(0, level) {
            cop2(m, DPCS);
            count = count.wrapping_sub(scale_q12(count, level));
            let colour = m.gte.mfc2(reg::RGB2);
            if !slt(0, count) {
                break 'body;
            }
            m.gte.mtc2(reg::RGBC, colour);
        }
        save_rotation(m, WORK);
        let context = m.gw(crate::common::CONTEXT);
        let lift = m.lw(context + 0x28) << 7;
        m.sw(WORK + 0x74, count);
        let base = ambient_base(m, 8, lift);
        ambient_point(m, WORK + 0x20, base, 0);
        let first = m.lw(SCRATCH + 4);
        if slt(0, count) {
            ambient_lines(m, first, base, count);
        }
        ambient_motion(m, 8, 0x80);
        let blocks = m.lw(WORK + 0x74);
        let packet = ambient_tails(m, base, if slt(0, blocks) { blocks } else { 0 });
        let slot = m.lw(SCRATCH) + 0x70;
        let first = m.lw(SCRATCH + 4);
        insert_chain(m, slot, first, packet.wrapping_sub(0x34));
        m.sw(SCRATCH + 4, packet);
        load_rotation(m, WORK);
        m.gte.ctc2(ctrl::TRX, 0);
        m.gte.ctc2(ctrl::TRY, 0);
        m.gte.ctc2(ctrl::TRZ, 0);
    }
    m.pop_frame(0x30);
}

// JP-REVIEW(R0402): near function: PAL 8008D948 -> JP 8008F1B8, imm:1, 1 instructions differ (whole routine)
/// 8008F1B8: ambient type 3, drifting motes: up to `weather +0x1D`
/// textured sprites (weather record gameplay +0x268, +0x1C fade, +8
/// ceiling) in the 0x4000-wide wrapped cube around the eye (>> 8, lifted
/// by context +0x28; offsets from the random words of `[gp+0x41C]` >> 9),
/// grey 0x838383 faded by the fade and the height (fewer motes near the
/// ceiling), depth-cued toward 0x100 away from depth 0x1C00 and sized by
/// depth; chained at OT slot 0x1C.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_motes` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_motes(m: &mut Machine) {
    crate::jp::res_p2_render::draw_motes(m)
}

// JP-REVIEW(R0403): near function: PAL 8008DD10 -> JP 8008F580, imm:1, 1 instructions differ (whole routine)
/// 8008F580: ambient type 5, falling leaves: `weather +0x1C` (at least
/// one) textured quads in the 0x4000-wide wrapped cube around the eye
/// (>> 12, the height not wrapped), each a square of 0x320 plus a random
/// byte per side in x and z, mirrored by the random bits, depth-cued grey
/// 0x838383 near and far, sorted by depth.
// JP-PATCH: replaced by `crate::jp::res_p2_render::draw_leaves` (src/jp/mod.rs REPLACES, PATCHES.md)
fn draw_leaves(m: &mut Machine) {
    crate::jp::res_p2_render::draw_leaves(m)
}

// ---- phase 4: the lit and draw callbacks (tables 800B2C60, 800B2C48) ----------------------

/// Copy `words` words from `source` to `target` (load all, then store).
fn copy_words(m: &mut Machine, target: u32, source: u32, words: u32) {
    let values: Vec<u32> = (0..words).map(|i| m.lw(source + i * 4)).collect();
    for (i, value) in values.into_iter().enumerate() {
        m.sw(target + i as u32 * 4, value);
    }
}

/// 8008C0A0 (work): lit callback 1, the afterburner flames of the lit
/// aircraft (`[gp+0x2DC]` +0xC model parameters: +0xC nozzle count and
/// vectoring flag, +0x10/+0x14 flame colours, +0x18 + 32n flame shapes,
/// +0x58 + 16n nozzles, +0x78 shimmer level; work +0x3FC the aircraft
/// state, +6 throttle, +0 vectoring, +0xC flicker bits): per nozzle two
/// translucent gouraud quads from the nozzle to the flame tip interpolated
/// by the throttle (flickering grey on odd frames), chained at the flame's
/// depth slot, and the heat shimmer 8008C644 when its flicker bits ask.
fn draw_afterburner(m: &mut Machine, work: u32) {
    m.push_frame(0x38);
    'body: {
        let resources = m.gw(crate::common::RESOURCES);
        let parameters = m.lw(resources + 0xc);
        let setup = m.lw(parameters + 0xc);
        let mut nozzles = setup & 0xff;
        if !slt(0, nozzles) {
            break 'body;
        }
        if !slt(nozzles, 3) {
            nozzles = 2;
        }
        let state = m.lw(work + 0x3fc);
        let flame = work + 0x10;
        m.sw(work + 0x10, nozzles);
        let mut throttle = lh(m, state + 6);
        if !slt(0, throttle) {
            break 'body;
        }
        if !slt(throttle, 0x1001) {
            throttle = 0x1000;
        }
        let vectoring = if sra(setup, 8) & 0xff != 0 {
            m.lw(state)
        } else {
            0
        };
        m.sw(flame + 8, vectoring);
        let context = m.gw(crate::common::CONTEXT);
        let phase = m.lw(context + 0x28);
        m.sw(flame + 4, phase);
        let colour = if phase & 1 != 0 {
            0x33_3333
        } else {
            m.gte.mtc2(reg::IR0, throttle);
            let parameters = m.lw(resources + 0xc);
            let near = m.lw(parameters + 0x10);
            m.gte.mtc2(reg::RGBC, near);
            let far = m.lw(parameters + 0x14);
            m.gte.ctc2(ctrl::RFC, (far & 0xff) << 4);
            let high = sra(far, 8);
            m.gte.ctc2(ctrl::GFC, (high & 0xff) << 4);
            m.gte.ctc2(ctrl::BFC, sra(high, 4) & 0xff0);
            cop2(m, DPCS);
            m.gte.mfc2(reg::RGB2) & 0xff_ffff
        };
        let context = m.gw(crate::common::CONTEXT);
        let paused = m.lw(context) & 0x2000_0000;
        m.sw(flame + 0x50, paused);
        let state = m.lw(work + 0x3fc);
        let flicker = m.lw(state + 0xc);
        let paused = m.lw(flame + 0x50);
        m.sw(flame + 0xc, flicker);
        if paused == 0 {
            m.sw(flame + 0xc, flicker << 2);
        }
        let resources = m.gw(crate::common::RESOURCES);
        let parameters = m.lw(resources + 0xc);
        let level = m.lw(parameters + 0x78);
        m.sw(flame + 0x4c, level);
        for nozzle in 0..nozzles {
            let resources = m.gw(crate::common::RESOURCES);
            let parameters = m.lw(resources + 0xc);
            let shape = nozzle * 0x20;
            copy_words(m, flame + 0x14, parameters + 0x58 + (nozzle << 4), 4);
            let vectoring = m.lw(flame + 8);
            if vectoring != 0 {
                for (amount, negative, positive) in [
                    (sext16(vectoring), 0x1eu32, 0x26u32),
                    (sra(vectoring, 16), 0x36, 0x2e),
                ] {
                    if amount == 0 {
                        continue;
                    }
                    let resources = m.gw(crate::common::RESOURCES);
                    let parameters = m.lw(resources + 0xc);
                    let product = if slt(amount, 0) {
                        let gain = lh(m, parameters.wrapping_add(shape) + negative);
                        mul_lo(gain, amount.wrapping_neg())
                    } else {
                        let gain = lh(m, parameters.wrapping_add(shape) + positive);
                        mul_lo(amount, gain)
                    };
                    let value = u32::from(m.lhu(flame + 0x1e)).wrapping_add(sra(product, 12));
                    m.sh(flame + 0x1e, value as u16);
                }
            }
            for (index, offset) in [0x14u32, 0x16, 0x18].into_iter().enumerate() {
                let value = lh(m, flame + offset);
                m.gte.mtc2(reg::IR1 + index, value);
            }
            for (register, offset) in [(ctrl::RFC, 0x1cu32), (ctrl::GFC, 0x1e), (ctrl::BFC, 0x20)] {
                let value = lh(m, flame + offset);
                m.gte.ctc2(register, value);
            }
            m.gte.mtc2(reg::IR0, throttle);
            let resources = m.gw(crate::common::RESOURCES);
            let parameters = m.lw(resources + 0xc);
            let tip = parameters.wrapping_add(shape) + 0x18;
            cop2(m, INTPL);
            copy_words(m, flame + 0x2c, tip, 4);
            for (index, offset) in [0x24u32, 0x26, 0x28].into_iter().enumerate() {
                let value = m.gte.mfc2(reg::MAC1 + index);
                m.sh(flame + offset, value as u16);
            }
            lwc2(m, reg::VXY0, flame + 0x2c);
            lwc2(m, reg::VZ0, flame + 0x30);
            lwc2(m, reg::VXY1, flame + 0x34);
            lwc2(m, reg::VZ1, flame + 0x38);
            lwc2(m, reg::VXY2, tip + 0x10);
            lwc2(m, reg::VZ2, tip + 0x14);
            let packet = m.lw(work + 4);
            m.sw(packet + 0x14, colour);
            cop2(m, RTPT);
            m.sw(packet, link(packet + 0x24) | 0x0800_0000);
            m.sw(packet + 0x38, colour);
            m.sw(packet + 4, 0x3a03_0303);
            m.sw(packet + 0x24, link(packet + 0x48) | 0x0800_0000);
            m.sw(packet + 0x28, 0x3a03_0303);
            if m.lw(flame + 0x50) == 0 {
                let random = sdk::rand(m) & 0xfff;
                let chance = 0xf7eu32.wrapping_sub(throttle) << 1;
                if !slt(random, chance) {
                    let bits = m.lw(flame + 0xc) | sllv(1, nozzle);
                    m.sw(flame + 0xc, bits);
                }
            }
            let bits = (m.lw(flame + 0xc) >> (nozzle & 31)) & 0x5555_5555;
            m.sw(flame + 0x10, bits);
            swc2(m, reg::SXY0, packet + 0x2c);
            swc2(m, reg::SXY1, packet + 0x44);
            swc2(m, reg::SXY2, packet + 8);
            m.sw(packet + 0xc, 0x0001_0101);
            m.sw(packet + 0x30, 0x0001_0101);
            cop2(m, AVSZ3);
            let dx = m.lhu(flame + 0x14).wrapping_sub(m.lhu(flame + 0x34));
            let dy = m.lhu(flame + 0x16).wrapping_sub(m.lhu(flame + 0x2e));
            m.sh(flame + 0x48, dx);
            m.sh(flame + 0x4a, dy);
            let average = m.gte.mfc2(reg::MAC0);
            lwc2(m, reg::VXY0, flame + 0x14);
            lwc2(m, reg::VZ0, flame + 0x18);
            lwc2(m, reg::VXY1, flame + 0x24);
            lwc2(m, reg::VZ1, flame + 0x28);
            lwc2(m, reg::VXY2, tip + 0x18);
            lwc2(m, reg::VZ2, tip + 0x1c);
            m.sw(packet + 0x1c, 0x0003_0303);
            m.sw(packet + 0x40, 0x0003_0303);
            cop2(m, RTPT);
            let shift = m.lw(work + 0xc).wrapping_add(0xa);
            let bias = lh(m, work + 0x3f6);
            let mut depth = sra(average, shift).wrapping_add(4).wrapping_sub(bias);
            if slt(depth, (-0x4b_i32) as u32) {
                depth = (-0x4b_i32) as u32;
            } else if !slt(depth, 0x4d) {
                depth = 0x4c;
            }
            let depth = sra(depth, 2);
            m.sw(flame + 0x44, depth);
            let second = packet + 0x24;
            swc2(m, reg::SXY0, packet + 0x18);
            swc2(m, reg::SXY0, second + 0x18);
            swc2(m, reg::SXY1, packet + 0x10);
            swc2(m, reg::SXY1, second + 0x10);
            swc2(m, reg::SXY2, packet + 0x20);
            let slot = (depth << 2).wrapping_add(m.lw(work + 0x3f8));
            let first = m.lw(work + 4);
            insert_chain(m, slot, first, second);
            m.sw(work + 4, packet + 0x48);
            if m.lw(flame + 0x10) != 0 {
                heat_shimmer(m, work, flame);
            }
            let phase = m.lw(flame + 4).wrapping_add(0x20);
            m.sw(flame + 4, phase);
        }
        if m.lw(flame + 0x50) == 0 {
            let state = m.lw(work + 0x3fc);
            let bits = m.lw(flame + 0xc);
            m.sw(state + 0xc, bits);
        }
    }
    m.pop_frame(0x38);
}

/// The `ReadRotMatrix` of these callbacks: the rotation (five words) and
/// the translation (three words) at `out`.
fn read_rot_matrix(m: &mut Machine, out: u32) {
    save_rotation(m, out);
    for (index, register) in [ctrl::TRX, ctrl::TRY, ctrl::TRZ].into_iter().enumerate() {
        let value = m.gte.cfc2(register);
        m.sw(out + 0x14 + index as u32 * 4, value);
    }
}

/// 8008C9D4 (work): lit callback 2, the engine glow disc of the lit
/// object: with effect block +0x5C8 bit 2 and a colour (gameplay +0x244
/// +0x70), a 16-segment fan of translucent gouraud triangles (0x32030303
/// rim, the colour at the centre) around the point 0xF0 behind the light
/// direction (L11..L13 angles by 80089E48, RotMatrixYXZ, scaled by
/// MulMatrix2 with (0x1000, 0x999, 0xC00), ApplyMatrixSV), front-facing
/// segments only (NCLIP), culled by screen outcodes; between draw-mode
/// packets at OT slots 0x13 and -0x13.
fn draw_engine_glow(m: &mut Machine, work: u32) {
    m.push_frame(0x38);
    'body: {
        let effects = m.gw(OBJECT_SYSTEMS);
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(effects + 0x5c8);
        let source = m.lw(gameplay + 0x244);
        let colour = m.lw(source + 0x70);
        if flags & 4 == 0 || colour == 0 {
            break 'body;
        }
        let block = work + 0x10;
        read_rot_matrix(m, block);
        let light0 = m.gte.cfc2(ctrl::L11L12);
        let light1 = m.gte.cfc2(ctrl::L11L12 + 1);
        let angles = block + 0x70;
        let matrix = block + 0x20;
        m.sw(angles, light0);
        m.sw(angles + 4, light1);
        let packed = dispatch::call(m, 0x8008_9e48, [angles, 0, 0, 0]);
        m.sw(angles, packed);
        m.sh(angles + 4, 0);
        sdk::rot_matrix_yxz(m, angles, matrix);
        let scale = block + 0x40;
        m.sw(scale, 0x1000);
        m.sw(scale + 8, 0x999);
        m.sw(scale + 4, 0);
        m.sw(scale + 0xc, 0);
        m.sw(scale + 0x10, 0xc00);
        sdk::mul_matrix2(m, scale, matrix);
        m.sh(angles, 0);
        m.sh(angles + 2, 0);
        m.sh(angles + 4, 0xf0);
        sdk::apply_matrix_sv(m, matrix, angles, angles);
        let rim = block + 0x78;
        for segment in 0..16u32 {
            let point = rim + segment * 8;
            let cosine = lh(m, SINES + segment * 0x400);
            let x = sra(
                if slt(cosine, 0) {
                    cosine.wrapping_add(0x3f)
                } else {
                    cosine
                },
                6,
            );
            m.sh(point, x as u16);
            let sine = lh(m, SINES + 2 + segment * 0x400);
            let z = scale_q12(sine.wrapping_mul(3) << 5, 1);
            m.sh(point + 2, z as u16);
            m.sh(point + 4, 0xdc);
            sdk::apply_matrix_sv(m, matrix, point, point);
        }
        load_rotation(m, block);
        lwc2(m, reg::VXY0, angles);
        lwc2(m, reg::VZ0, angles + 4);
        let head = m.lw(work + 4);
        m.sw(head + 4, 0xe600_0000);
        cop2(m, RTPS);
        let slot = m.lw(work + 0x3f8) + 0x4c;
        let tag = m.lw(slot) | 0x0100_0000;
        m.sw(head, tag);
        let fan = head + 8;
        let mut packet = fan + 8;
        let slot = m.lw(work + 0x3f8) + 0x4c;
        m.sw(slot, link(head));
        m.sw(fan, link(packet) | 0x0100_0000);
        m.sw(fan + 4, 0xe600_0002);
        let centre = m.gte.mfc2(reg::SXY2);
        m.sw(angles, centre);
        for segment in 0..16u32 {
            let point = rim + segment * 8;
            lwc2(m, reg::VXY0, point);
            lwc2(m, reg::VZ0, point + 4);
            cop2(m, RTPS);
            let previous = angles + segment * 8;
            let code = dispatch::call(m, 0x8009_65d4, [previous, 0, 0, 0]);
            m.sw(previous + 4, code);
            swc2(m, reg::SXY2, point);
        }
        let last = rim + 15 * 8;
        let code = dispatch::call(m, 0x8009_65d4, [last, 0, 0, 0]);
        m.sw(last + 4, code);
        let first_xy = m.lw(rim);
        let first_code = m.lw(rim + 4);
        let centre_code = m.lw(angles + 4);
        m.sw(rim + 0x80, first_xy);
        m.sw(rim + 0x84, first_code);
        m.gte.mtc2(reg::SXY0, centre);
        let mut previous_code = m.lw(rim + 4);
        let mut previous = first_xy;
        for segment in 0..16u32 {
            let point = rim + 8 + segment * 8;
            let code = m.lw(point + 4);
            if centre_code & previous_code & code != 0 {
                previous_code = code;
                previous = m.lw(point);
                continue;
            }
            previous_code = code;
            let xy = m.lw(point);
            m.gte.mtc2(reg::SXY1, previous);
            m.gte.mtc2(reg::SXY2, xy);
            m.sw(packet + 8, previous);
            previous = xy;
            cop2(m, NCLIP);
            m.sw(packet + 0x10, xy);
            m.sw(packet + 0x18, centre);
            m.sw(packet + 0xc, 0x0003_0303);
            m.sw(packet + 0x14, colour);
            if slt(0, m.gte.mfc2(reg::MAC0)) {
                let next = packet + 0x1c;
                m.sw(packet, link(next) | 0x0600_0000);
                m.sw(packet + 4, 0x3203_0303);
                packet = next;
            }
        }
        m.sw(packet, 0x0100_0000);
        m.sw(packet + 4, 0xe600_0000);
        let slot = m.lw(work + 0x3f8).wrapping_sub(0x4c);
        insert_chain(m, slot, fan, packet);
        m.sw(work + 4, packet + 8);
    }
    m.pop_frame(0x38);
}

/// 8008CD80 (block, shift, distance) -> projected size or 0: project the
/// glint centre (block +0x20..+0x24 << `shift`) and its size +0x28 <<
/// `shift` rotated by the sine table entry of the angle +0x2C; 0 when the
/// depth (>> `shift`) is outside 0x1F..0x77F7 or the size `distance *
/// size / depth` is below 3; else the sprite record (+0x30.. rotation
/// rows, +0x50 depth slot, +0x54 screen centre) with 80086948 applied and
/// the translation set to the projected centre.
fn project_glint(m: &mut Machine, block: u32, shift: u32, distance: u32) -> u32 {
    m.push_frame(0x28);
    let x = sllv(lh(m, block + 0x20), shift);
    let z = sllv(lh(m, block + 0x24), shift);
    let y = sllv(lh(m, block + 0x22), shift);
    m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
    m.gte.mtc2(reg::VZ0, z);
    let angle = m.lw(block + 0x2c);
    let size = m.lw(block + 0x28);
    cop2(m, RTPS);
    let pair = m.lw(SINES + ((angle & 0xfff) << 2));
    let size = sllv(size, shift);
    let sine = sra(pair, 16);
    let cosine = sext16(pair);
    let across = mul_lo(cosine, size);
    let along = mul_lo(sine, size);
    let scaled = mul_lo(distance, size);
    m.sh(block + 0x38, size as u16);
    let across = sra(across, 12);
    let along = sra(along, 12);
    let depth = m.gte.mfc2(reg::MAC3);
    let slot = sra(depth, shift);
    let result = if slot.wrapping_sub(0x1f) >= 0x77d9 {
        0
    } else {
        let projected = div_checked(m, scaled, depth, 0x8008_ce4c);
        if slt(projected, 3) {
            0
        } else {
            swc2(m, reg::SXY2, block + 0x54);
            m.sw(block + 0x50, sra(slot, 3));
            m.sh(block + 0x34, across as u16);
            m.sh(block + 0x3c, across.wrapping_neg() as u16);
            m.sh(block + 0x40, along as u16);
            m.sh(block + 0x30, along as u16);
            for offset in [0x3eu32, 0x3a, 0x36, 0x32] {
                m.sh(block + offset, 0);
            }
            let tx = m.gte.mfc2(reg::MAC1);
            let ty = m.gte.mfc2(reg::MAC2);
            multiply_rotation(m, block + 0x30);
            m.gte.ctc2(ctrl::TRX, tx);
            m.gte.ctc2(ctrl::TRY, ty);
            m.gte.ctc2(ctrl::TRZ, depth);
            projected
        }
    };
    m.pop_frame(0x28);
    result
}

/// 8008CEFC (work, block, rays, spread): the glint rays: `rays` translucent
/// gouraud quads (colours 0x242828 to 0x030303 at the tip) around the
/// projected centre (block +0x54), swept by `spread` over the turn, between
/// texture-page packets at the block's depth slot +0x50.
fn glint_rays(m: &mut Machine, work: u32, block: u32, rays: u32, spread: u32) {
    m.push_frame(0x18);
    if slt(0, rays) {
        let first = m.lw(work + 4);
        let points = block + 0x58;
        let mut packet = first + 8;
        m.sw(first + 4, 0xe100_0240);
        m.sw(first, link(packet) | 0x0100_0000);
        let half = SINES + ((spread << 1) & 0x3ffc);
        let centre = m.lw(block + 0x54);
        m.sw(block + 0x58, 0);
        m.sh(points + 4, 0x1000);
        let pair = m.lw(half);
        m.sh(points + 0xa, 0x40);
        m.sh(points + 0xc, sra(pair, 16) as u16);
        m.sh(points + 8, pair as u16);
        let pair = m.lw(SINES + ((spread & 0xfff) << 2));
        m.sh(points + 0x12, 0x96);
        m.sh(points + 0x14, sra(pair, 16) as u16);
        let step = div_checked(m, 0x1000, rays, 0x8008_cfb8);
        m.sh(points + 0x10, pair as u16);
        let mut angle = 0u32;
        for _ in 0..rays {
            lwc2(m, reg::VXY0, points);
            lwc2(m, reg::VZ0, points + 4);
            lwc2(m, reg::VXY1, points + 8);
            lwc2(m, reg::VZ1, points + 0xc);
            lwc2(m, reg::VXY2, points + 0x10);
            lwc2(m, reg::VZ2, points + 0x14);
            angle = angle.wrapping_add(step);
            m.sw(packet + 0x18, centre);
            cop2(m, RTPT);
            let pair = m.lw(SINES + ((angle & 0xfff) << 2));
            m.sh(points + 4, sra(pair, 16) as u16);
            let middle = angle.wrapping_add(sra(spread, 1));
            m.sh(points, pair as u16);
            let pair = m.lw(SINES + ((middle & 0xfff) << 2));
            m.sh(points + 0xc, sra(pair, 16) as u16);
            let end = angle.wrapping_add(spread);
            m.sh(points + 8, pair as u16);
            let pair = m.lw(SINES + ((end & 0xfff) << 2));
            m.sh(points + 0x14, sra(pair, 16) as u16);
            let next = packet + 0x24;
            m.sh(points + 0x10, pair as u16);
            m.sw(packet, link(next) | 0x0800_0000);
            m.sw(packet + 4, 0x3a24_2828);
            m.sw(packet + 0xc, 0x001c_1f1f);
            m.sw(packet + 0x14, 0x0005_0707);
            m.sw(packet + 0x1c, 0x0001_0303);
            swc2(m, reg::SXY0, packet + 8);
            swc2(m, reg::SXY1, packet + 0x10);
            swc2(m, reg::SXY2, packet + 0x20);
            packet = next;
        }
        m.sw(packet, 0x0100_0000);
        m.sw(packet + 4, 0xe100_0220);
        let depth = m.lw(block + 0x50);
        let slot = (depth << 2).wrapping_add(m.lw(work));
        let first = m.lw(work + 4);
        insert_chain(m, slot, first, packet);
        m.sw(work + 4, packet + 8);
    }
    m.pop_frame(0x18);
}

/// The glints of draw callbacks 3..5: the block at work +0x10 (view
/// saved, offset +0x20.., size +0x28, angle +0x2C) drawn by 8008CD80 and
/// 8008CEFC with `rays` and `spread`, then the view restored. Returns
/// whether it was visible.
fn draw_glint(m: &mut Machine, work: u32, distance: u32, rays: u32, spread: u32) -> bool {
    let block = work + 0x10;
    let shift = m.lw(work + 0xc);
    if project_glint(m, block, shift, distance) == 0 {
        return false;
    }
    glint_rays(m, work, block, rays, spread);
    load_rotation(m, block);
    load_translation(m, block + 0x14);
    true
}

/// 8008D140 (work): draw callback 3, a three-ray glint spinning with
/// context +0x28 (x513) at (0, -1, 4), size 0x28.
fn draw_glint_3(m: &mut Machine, work: u32) {
    m.push_frame(0x20);
    let block = work + 0x10;
    let context = m.gw(crate::common::CONTEXT);
    let phase = m.lw(context + 0x28);
    read_rot_matrix(m, block);
    m.sh(block + 0x22, 0xffff);
    m.sh(block + 0x24, 4);
    m.sh(block + 0x20, 0);
    m.sw(block + 0x28, 0x28);
    m.sw(block + 0x2c, (phase << 9).wrapping_add(phase));
    let distance = m.gte.cfc2(ctrl::H);
    draw_glint(m, work, distance, 3, 0x155);
    m.pop_frame(0x20);
}

/// 8008D244 (work): draw callback 4, a five-ray glint at (0, -1, 2), size
/// 0x24.
fn draw_glint_4(m: &mut Machine, work: u32) {
    m.push_frame(0x20);
    let block = work + 0x10;
    let context = m.gw(crate::common::CONTEXT);
    let phase = m.lw(context + 0x28);
    read_rot_matrix(m, block);
    m.sh(block + 0x22, 0xffff);
    m.sh(block + 0x24, 2);
    m.sh(block + 0x20, 0);
    m.sw(block + 0x28, 0x24);
    m.sw(block + 0x2c, (phase << 9).wrapping_add(phase));
    let distance = m.gte.cfc2(ctrl::H);
    draw_glint(m, work, distance, 5, 0x13e);
    m.pop_frame(0x20);
}

/// 8008D348 (work): draw callback 5, a pair of three-ray glints at (+-0x28,
/// -0xF, 0), size 0x25, counter-spinning (x511, the second mirrored and
/// offset by 0x3E8).
fn draw_glint_5(m: &mut Machine, work: u32) {
    m.push_frame(0x28);
    let block = work + 0x10;
    let context = m.gw(crate::common::CONTEXT);
    let phase = m.lw(context + 0x28);
    let distance = m.gte.cfc2(ctrl::H);
    read_rot_matrix(m, block);
    m.sh(block + 0x20, 0x28);
    m.sh(block + 0x22, (-0xf_i32) as u16);
    m.sh(block + 0x24, 0);
    m.sw(block + 0x28, 0x25);
    m.sw(block + 0x2c, (phase << 9).wrapping_sub(phase));
    if draw_glint(m, work, distance, 3, (-0x13e_i32) as u32) {
        let x = m.lhu(block + 0x20);
        let angle = m.lw(block + 0x2c);
        m.sh(block + 0x20, x.wrapping_neg());
        m.sw(block + 0x2c, angle.wrapping_neg().wrapping_add(0x3e8));
        draw_glint(m, work, distance, 3, 0x13e);
    }
    m.pop_frame(0x28);
}

/// Set the halfwords at `packet + offsets` to `value`.
fn set_halves(m: &mut Machine, packet: u32, offsets: &[u32], value: u32) {
    for &offset in offsets {
        m.sh(packet + offset, value as u16);
    }
}

/// 8008D678 (work): draw callback 2, a star sparkle: three packets of two
/// translucent gouraud quads (a cross and a diamond) at the projection of
/// (0, 0, -32), sized `30 H / depth` (at least 8), colour 0xC04343 cued by
/// 0x45E (the last cross keeps it raw), at the object's depth slot.
fn draw_sparkle(m: &mut Machine, work: u32) {
    let base = work + 0x10;
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0);
    }
    m.gte.mtc2(reg::RGBC, 0x00c0_4343);
    m.gte.mtc2(reg::IR0, 0x45e);
    let first = m.lw(work + 4);
    cop2(m, DPCS);
    let code = 0x3a01_0101u32;
    m.sw(first + 0x44, link(first + 0x88) | 0x1000_0000);
    m.sw(first + 4, code);
    m.sw(first + 0x48, code);
    m.sw(first + 0x88, link(first + 0xcc) | 0x1000_0000);
    m.sw(first + 0x8c, code);
    m.sw(first, link(first + 0x44) | 0x1000_0000);
    m.sw(base + 0x1c, 0x00c0_4343);
    let cued = m.gte.mfc2(reg::RGB2);
    m.sw(base + 0x20, cued);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, (-0x20_i32) as u32);
    cop2(m, RTPS);
    for block in 0..3u32 {
        let packet = first + block * 0x44;
        for offset in [0x3cu32, 0x34, 0x24, 0x1c, 0x14] {
            m.sw(packet + offset, code);
        }
    }
    swc2(m, reg::SXY2, base + 8);
    let depth = m.gte.mfc2(reg::SZ3);
    let h = m.gte.cfc2(ctrl::H);
    let depth = sra(depth, m.lw(work + 0xc));
    if depth.wrapping_sub(0x1f) >= 0x77d9 {
        return;
    }
    let size = div_checked(m, (h << 4).wrapping_sub(h) << 1, depth, 0x8008_d788);
    let slot = sra(depth, 3);
    m.sw(base + 0x18, size);
    let at = (slot << 2).wrapping_add(m.lw(work));
    insert_chain(m, at, first, first + 0x88);
    m.sw(work + 4, first + 0xcc);
    if slt(m.lw(base + 0x18), 8) {
        m.sw(base + 0x18, 8);
    }
    let centre = m.lw(base + 8);
    let mut colour = m.lw(base + 0x20);
    for block in 0..3u32 {
        let packet = first + block * 0x44;
        m.sw(packet + 0x30, centre);
        m.sw(packet + 0x10, centre);
        m.sw(packet + 0x2c, colour);
        m.sw(packet + 0xc, colour);
        if block == 1 {
            colour = m.lw(base + 0x1c);
        }
    }
    let x = |m: &mut Machine| u32::from(m.lhu(base + 8));
    let y = |m: &mut Machine| u32::from(m.lhu(base + 0xa));
    let size = m.lw(base + 0x18);
    let mut thin = sra(size, 4);
    if thin == 0 {
        thin = 1;
    }
    let cross = first;
    // Widescreen: the X extents squeezed like the projection (H-sized).
    let (size_x, thin_x) = (
        widescreen::squeeze_x(m, size),
        widescreen::squeeze_x(m, thin),
    );
    let v = x(m).wrapping_sub(size_x);
    set_halves(m, cross, &[0x28, 8], v);
    let v = y(m).wrapping_sub(size);
    set_halves(m, cross, &[0x2a, 0xa], v);
    let v = x(m).wrapping_add(size_x);
    set_halves(m, cross, &[0x40, 0x20], v);
    let v = y(m).wrapping_add(size);
    set_halves(m, cross, &[0x42, 0x22], v);
    let v = x(m).wrapping_add(thin_x);
    set_halves(m, cross, &[0x18], v);
    let v = y(m).wrapping_sub(thin);
    set_halves(m, cross, &[0x1a], v);
    let v = x(m).wrapping_sub(thin_x);
    set_halves(m, cross, &[0x38], v);
    let v = y(m).wrapping_add(thin);
    set_halves(m, cross, &[0x3a], v);
    let diamond = first + 0x44;
    let half = sra(m.lw(base + 0x18), 1);
    let half_x = widescreen::squeeze_x(m, half);
    let v = x(m).wrapping_add(half_x);
    set_halves(m, diamond, &[0x28, 8], v);
    let v = y(m).wrapping_sub(half);
    set_halves(m, diamond, &[0x2a, 0xa], v);
    let v = x(m).wrapping_sub(half_x);
    set_halves(m, diamond, &[0x40, 0x20], v);
    let v = y(m).wrapping_add(half);
    set_halves(m, diamond, &[0x42, 0x22], v);
    let v = x(m).wrapping_sub(thin_x);
    set_halves(m, diamond, &[0x18], v);
    let v = y(m).wrapping_sub(thin);
    set_halves(m, diamond, &[0x1a], v);
    let v = x(m).wrapping_add(thin_x);
    set_halves(m, diamond, &[0x38], v);
    let v = y(m).wrapping_add(thin);
    set_halves(m, diamond, &[0x3a], v);
    let size = m.lw(base + 0x18);
    let core = first + 0x88;
    let v = x(m);
    set_halves(m, core, &[0x38, 0x18], v);
    let quarter = sra(size, 2);
    let quarter_x = widescreen::squeeze_x(m, quarter);
    let v = x(m).wrapping_sub(quarter_x);
    set_halves(m, core, &[0x28, 8], v);
    let v = x(m).wrapping_add(quarter_x);
    set_halves(m, core, &[0x40, 0x20], v);
    let v = y(m);
    set_halves(m, core, &[0x42, 0x2a, 0x22, 0xa], v);
    let v = y(m).wrapping_sub(quarter);
    set_halves(m, core, &[0x1a], v);
    let v = y(m).wrapping_add(quarter);
    set_halves(m, core, &[0x3a], v);
}

// ---- indirect calls -----------------------------------------------------------------------

/// The `index`th stack argument (5th argument = 0) of a call entered with
/// `m.sp` at the callee's entry, as the original callee reads it.
fn stack_argument(m: &mut Machine, index: u32) -> u32 {
    m.lw(m.sp + 16 + index * 4)
}

/// Every routine this module translates, by original address, with the
/// original `a0..a3` (further arguments from the caller's stack words
/// +0x10..) and `v0` (0 for routines that return nothing).
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let v0 = match target {
        0x8003_342c => draw_entity_models(m),
        0x8006_8334 => entity_model_pass(m, a0, a1, a2, a3),
        0x8006_7144 => list_entities(m, a0),
        0x8006_7380 => list_objects(m, a0),
        0x8006_7494 => {
            build_display_lists(m, a0, a1, a2, a3);
            0
        }
        0x8006_6a9c => blend_vertices(m, a0, a1, a2, a3),
        0x8006_6c18 => {
            let (to_a, out, temp) = (
                stack_argument(m, 0),
                stack_argument(m, 1),
                stack_argument(m, 2),
            );
            blend_vertices_2d(m, a0, a1, a2, a3, to_a, out, temp)
        }
        0x8006_6ef8 => {
            let temp = stack_argument(m, 0);
            blend_by_angles(m, a0, a1, a2, a3, temp)
        }
        0x8006_701c => blend_pair(m, a0, a1, a2),
        0x8006_704c => blend_signed(m, a0, a1, a2),
        0x8006_6c80 => {
            prepare_plain_list(m, a0, a1);
            0
        }
        0x8006_6dcc => {
            prepare_textured_list(m, a0, a1);
            0
        }
        0x8006_7088 => {
            prepare_lit_list(m, a0, a1);
            0
        }
        0x8006_26e0 => {
            draw_plain_list(m, a0, a1);
            0
        }
        0x8006_2bd4 => {
            draw_textured_list(m, a0, a1);
            0
        }
        0x8006_2d68 => {
            draw_textured_triangles(m, a0);
            0
        }
        0x8006_2ec8 => {
            draw_textured_quads(m, a0);
            0
        }
        0x8008_d4e8 => {
            draw_glow_sprite(m, a0);
            0
        }
        0x8006_37b0 => {
            draw_lit_list(m, a0, a1);
            0
        }
        0x8003_33d8 => draw_effects(m),
        0x8006_c5ec => effect_pass(m, a0, a1),
        0x8006_c20c => {
            sort_effect_segments(m, a0, a1, a2, a3);
            0
        }
        0x8009_0a74 => draw_effect_lines(m, a0, a1, a2),
        0x8009_0bf8 => draw_thin_trails(m, a0, a1, a2),
        0x8009_0ed0 => draw_medium_trails(m, a0, a1, a2),
        0x8009_11b0 => draw_sparks(m, a0, a1, a2),
        0x8009_1370 => draw_wide_trails(m, a0, a1, a2),
        0x8008_de18 => {
            build_trail_quad(m, a0, a1);
            0
        }
        0x8003_3324 => draw_lit_systems(m),
        0x8007_db00 => draw_trail_systems(m, a0, a1),
        0x8007_c00c => draw_smoke_trails(m, a0, a1),
        0x8007_cfa4 => draw_ribbon_trails(m, a0, a1),
        0x8008_e038 => {
            draw_ambient_particles(m);
            0
        }
        0x8008_54b8 => {
            draw_objects(m, a0);
            0
        }
        0x8008_700c => {
            draw_projectiles(m, a0);
            0
        }
        0x8008_b048 => {
            draw_debris(m, a0);
            0
        }
        0x8009_2850 => {
            draw_explosions(m, a0);
            0
        }
        0x8008_ba8c => {
            draw_sun(m, a0, a1, a2);
            0
        }
        0x8008_b310 => {
            draw_moon(m, a0, a1);
            0
        }
        0x8009_044c => {
            draw_lock_marker(m, a0);
            0
        }
        0x8007_b438 => {
            update_camera(m, a0);
            0
        }
        0x8007_7a30 => {
            cockpit_camera(m, a0);
            0
        }
        0x8007_79c8 => {
            look_toward(m, a0, a1, a2);
            0
        }
        0x8003_13a8 => {
            view_angles(m, a0, a1, a2);
            0
        }
        0x8003_17f0 => aim_angles(m, a0, a1, a2),
        0x8008_a61c => {
            draw_shock_ring(m, a0, a1);
            0
        }
        0x8008_488c => noise_overlay(m, a0, a1, a2),
        0x8007_7dd0 => {
            look_camera(m, a0);
            0
        }
        0x8007_affc => {
            replay_camera(m, a0);
            0
        }
        0x8007_ae40 => {
            ease_pose(m, a0, a1);
            0
        }
        0x8007_867c => {
            manual_look(m, a0);
            0
        }
        0x8007_9b1c => ground_height(m, a0, a1, a2),
        0x8007_9cb0 => ground_clearance(m, a0),
        0x8007_9cfc => {
            clamp_to_box(m, a0, a1, a2);
            0
        }
        0x8003_1cd8 => {
            direction_angles(m, a0, a1);
            0
        }
        0x8007_995c => {
            toward_reference(m, a0, a1, a2);
            0
        }
        0x8007_99d4 => {
            behind_subject(m, a0, a1, a2, a3);
            0
        }
        0x8007_9ae8 => reference_distance(m, a0),
        0x8007_9dac => {
            orbit_point(m, a0, a1, a2);
            0
        }
        0x8007_9f2c => {
            swing_point(m, a0, a1, a2, a3);
            0
        }
        0x8007_a060 => {
            aim_at_subject(m, a0, a1, a2);
            0
        }
        0x8007_a0f0 => shot_fixed_near(m, a0, a1, a2),
        0x8007_a260 => shot_tracking_near(m, a0, a1, a2),
        0x8007_a3b4 => shot_overhead(m, a0, a1, a2),
        0x8007_a52c => shot_swing_behind(m, a0, a1, a2),
        0x8007_a6cc => shot_swing_past(m, a0, a1, a2),
        0x8007_a8c4 => shot_drift_behind(m, a0, a1, a2),
        0x8007_aab4 => shot_chase(m, a0, a1, a2),
        0x8007_ac50 => shot_pull_away(m, a0, a1, a2),
        0x8003_32e4 => view_matrix(m, a0, a1, a2),
        0x8006_305c => {
            draw_body_triangles(m, a0);
            0
        }
        0x8006_31f8 => {
            draw_body_quads(m, a0);
            0
        }
        0x8006_33d8 => {
            draw_coloured_triangles(m, a0);
            0
        }
        0x8006_358c => {
            draw_coloured_quads(m, a0);
            0
        }
        0x8008_591c => {
            sprite_corners(m, a0, a1);
            0
        }
        0x8008_5a94 => {
            draw_burst(m, a0, a1, a2);
            0
        }
        0x8008_5c8c => {
            draw_puff(m, a0, a1, a2);
            0
        }
        0x8008_67e4 => {
            angle_matrix(m, a0, a1, a2);
            0
        }
        0x8008_6948 => {
            multiply_rotation(m, a0);
            0
        }
        0x8009_0210 => {
            tint_lock_light(m, a0, a1, a2);
            0
        }
        0x8008_ffac => {
            draw_overlays(m, a0);
            0
        }
        0x8008_f950 => screen_blend(m, a0, a1, a2),
        0x8008_6320 => {
            draw_tracer(m, a0, a1, a2);
            0
        }
        0x8008_6a0c => {
            draw_exhaust(m, a0, a1, a2);
            0
        }
        0x8007_8d70 => {
            orbit_camera(m, a0);
            0
        }
        0x8002_77f0 => debug_hex(m, a0, a1, a2, a3),
        0x8007_7460 => {
            debug_camera(m, a0);
            0
        }
        0x8003_6440 => copy_frame(m, a0, a1),
        0x8003_64c4 => star_screen(m, a0, a1),
        0x8003_65fc => border_quads(m, a0, a1, a2, a3),
        0x8003_6a84 => fade_screen(m, a0, a1),
        0x8005_e40c => loading_tiles(m, a0, a1, a2),
        0x8008_3fe0 => starfield(m, a0, a1),
        0x8007_3928 => {
            select_camera_script(m, a0);
            0
        }
        0x8007_6dbc => {
            reset_camera_control(m);
            0
        }
        0x8007_961c => {
            start_camera(m, a0);
            0
        }
        0x8007_7004 => {
            free_camera(m, a0, a1, a2, a3);
            0
        }
        0x8007_754c => {
            debug_orbit_camera(m, a0, a1);
            0
        }
        0x8007_8234 => {
            chase_camera(m, a0, a1);
            0
        }
        0x8007_895c => {
            sway_camera(m, a0);
            0
        }
        0x8007_8b28 => {
            pull_away_camera(m, a0);
            0
        }
        0x8007_8fdc => {
            pull_back_camera(m, a0);
            0
        }
        0x8007_91e8 => {
            swing_camera(m, a0);
            0
        }
        0x8008_b7ac => {
            lens_flare(m, a0, a1);
            0
        }
        0x8008_c644 => {
            heat_shimmer(m, a0, a1);
            0
        }
        0x8009_16a8 => trail_fan(m, a0, a1, a2),
        0x8009_1a70 => trail_cap(m, a0, a1, a2),
        0x8009_1ce8 => {
            trail_layer(m, a0, a1, a2, a3);
            0
        }
        0x8009_2014 => plane_distance(m, a0, a1),
        0x8009_211c => {
            interpolate_point(m, a0, a1, a2, a3);
            0
        }
        0x8009_2168 => clip_segment(m, a0, a1, a2, a3),
        0x8009_236c => {
            draw_explosion(m, a0, a1, a2);
            0
        }
        0x8008_5eac => {
            draw_spark_sprite(m, a0, a1, a2);
            0
        }
        0x8008_60a4 => {
            draw_glow_object(m, a0, a1, a2);
            0
        }
        0x8008_9f44 => {
            draw_debris_star(m, a0, a1);
            0
        }
        0x8008_abf0 => {
            draw_debris_ember(m, a0, a1);
            0
        }
        0x8008_e4f8 => {
            draw_rain(m);
            0
        }
        0x8008_eb58 => {
            draw_snow(m);
            0
        }
        0x8008_f1b8 => {
            draw_motes(m);
            0
        }
        0x8008_f580 => {
            draw_leaves(m);
            0
        }
        0x8008_c0a0 => {
            draw_afterburner(m, a0);
            0
        }
        0x8008_c9d4 => {
            draw_engine_glow(m, a0);
            0
        }
        0x8008_cd80 => project_glint(m, a0, a1, a2),
        0x8008_cefc => {
            glint_rays(m, a0, a1, a2, a3);
            0
        }
        0x8008_d140 => {
            draw_glint_3(m, a0);
            0
        }
        0x8008_d244 => {
            draw_glint_4(m, a0);
            0
        }
        0x8008_d348 => {
            draw_glint_5(m, a0);
            0
        }
        0x8008_d678 => {
            draw_sparkle(m, a0);
            0
        }
        // SDK OuterProduct0, here until `sdk.rs` translates it.
        0x800a_72e4 => {
            outer_product0(m, a0, a1, a2);
            0
        }
        _ => return None,
    };
    Some(v0)
}
