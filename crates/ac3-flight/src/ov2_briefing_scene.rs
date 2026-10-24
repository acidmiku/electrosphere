//! Overlay module 2, briefing sub-mode: the 3D briefing scene and the 2D
//! map overlays drawn by the briefing tick 800EF01C (see `ov2_briefing`).
//!
//! - 800ED834 draws the scene while the scene flag is set: the view matrix
//!   (800EBCD8), the 15 x 15 terrain grid around the camera
//!   from the work block's 4-bit height map (800EBF68), the timer marker
//!   (800ED63C) and the 32 objects as textured quads (800ECE7C).
//! - 800ED8B0 draws the minimap (object dots and the camera frame).
//! - 800EB754 draws a label's two connector lines.
//!
//! Translations follow the original instructions, store order and GTE
//! register traffic exactly (every `mfc2`/`swc2`, which have a storage side
//! effect). The terrain renderer keeps its working state on the scratchpad
//! (0x1F800000..0x1F80003F), as the original does. These routines are
//! claimed by `ov2_briefing::dispatch`.
use crate::{
    machine::Machine,
    ov2_briefing::{
        CAMERA, DRAW_TEXT, FLAGS, GET_VISUAL_OFFSET, MARKER, MARKER_ID, OBJECT_SIZE, OBJECTS,
        PANEL_OFFSETS, SPRINTF, STRLEN, VIEW_ANGLES, VIEW_MATRIX, VIEW_OFFSETS, WORK_HIGH, ZOOM,
        abs, add_prim, briefing, div, div_pow2, external, half, lbu, lh, lhu, mul, mulhi, sb,
        sext16, sh, slt, sra, work,
    },
};
use ac3_gte::{ctrl, reg};

/// Scratchpad base: the terrain renderer's working state.
const SCRATCH: u32 = 0x1f80_0000;
/// 800CB340: label connector colors (five RGB words), by label style.
const CONNECTOR_COLORS: u32 = 0x800c_b340;
/// 800CB354: the full-screen drawing area RECT {0, 0, 320, 511}.
const SCREEN_AREA: u32 = 0x800c_b354;
/// 800CB35C: `"%2d:%2d"`, the marker's minutes:seconds.
const MARKER_FORMAT: u32 = 0x800c_b35c;
/// 800F6820: terrain colors by height / 128.
const TERRAIN_COLORS: u32 = 0x800f_6820;

// SDK routines called by original address.
const SET_DRAW_AREA: u32 = 0x800a_73f0;
const SET_GEOM_OFFSET: u32 = 0x800a_55e8;
const ROT_MATRIX_YXZ: u32 = 0x800a_5be8;
const ROT_MATRIX: u32 = 0x800a_5958;
/// TransMatrix (`m->t = *v`).
const TRANS_MATRIX: u32 = 0x800a_53a8;
const TRANSPOSE_MATRIX: u32 = 0x800a_5918;
const APPLY_MATRIX_LV: u32 = 0x800a_4d78;
const SQUARE_ROOT0: u32 = 0x800a_47c8;
const RATAN2: u32 = 0x800a_6088;
/// ov2core: 800EF1F4 doubles row 1 of a rotation matrix (a handler of the
/// visual-script table 0x800F695C, also called here).
const DOUBLE_ROW1: u32 = 0x800e_f1f4;

// GTE command words, exactly as encoded in the original instructions.
/// RTPS sf=1.
const RTPS: u32 = 0x018_0001;
/// RTPT sf=1.
const RTPT: u32 = 0x028_0030;
/// MVMVA sf=1, rotation x V0 + translation.
const MVMVA_RT_V0_TR: u32 = 0x048_0012;
/// MVMVA sf=1, rotation x IR, no translation.
const MVMVA_RT_IR: u32 = 0x049_e012;

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
/// Inline `gte_SetRotMatrix`: five words into R11R12..R33.
fn load_rotation(m: &mut Machine, matrix: u32) {
    let t4 = m.lw(matrix);
    let t5 = m.lw(matrix + 4);
    m.gte.ctc2(ctrl::R11R12, t4);
    m.gte.ctc2(ctrl::R13R21, t5);
    let t4 = m.lw(matrix + 8);
    let t5 = m.lw(matrix + 0xc);
    let t6 = m.lw(matrix + 0x10);
    m.gte.ctc2(ctrl::R22R23, t4);
    m.gte.ctc2(ctrl::R31R32, t5);
    m.gte.ctc2(ctrl::R33, t6);
}
/// Inline `gte_SetTransMatrix`: the matrix's words +0x14..+0x1C into TR.
fn load_translation(m: &mut Machine, matrix: u32) {
    let t4 = m.lw(matrix + 0x14);
    let t5 = m.lw(matrix + 0x18);
    m.gte.ctc2(ctrl::TRX, t4);
    let t6 = m.lw(matrix + 0x1c);
    m.gte.ctc2(ctrl::TRY, t5);
    m.gte.ctc2(ctrl::TRZ, t6);
}
/// `swc2 IR0`, `cfc2 FLAG`, and `mfc2 SZ3 >> 2` into three words (the
/// inline `RotTransPers` result stores).
fn store_depth(m: &mut Machine, ir0: u32, flag: u32, depth: u32) {
    swc2(m, reg::IR0, ir0);
    let value = m.gte.cfc2(ctrl::FLAG);
    m.sw(flag, value);
    let z = m.gte.mfc2(reg::SZ3);
    m.sw(depth, sra(z, 2));
}
/// Copy the 8-byte RECT at `source` (the compiled `lwl`/`lwr` pairs on
/// aligned words) to `target`.
fn copy_rect(m: &mut Machine, source: u32, target: u32) {
    let first = m.lw(source);
    let second = m.lw(source + 4);
    m.sw(target, first);
    m.sw(target + 4, second);
}

// ---- 800EB754: label connectors ---------------------------------------------------

/// 800EB754: for a shown label (not fading or highlighted, not hidden by
/// its object), two LINE_F2 packets from the label's visual slot (offset of
/// slot 2 + label position + (78, 90/91)) to its object's screen position,
/// in the label style's color, linked at the head of the ordering table.
pub fn label_connector(m: &mut Machine, label: u32) {
    let frame = m.push_frame(0x40);
    let colors = frame + 0x10;
    for word in 0..5 {
        let value = m.lw(CONNECTOR_COLORS + word * 4);
        m.sw(colors + word * 4, value);
    }
    let flags = m.lw(label);
    let shown = flags & 0x64 == 0 && (flags & 8 != 0 || (flags & 0x10 == 0 && flags & 2 != 0));
    if shown {
        connector_lines(m, frame, label);
    }
    m.pop_frame(0x40);
}

fn connector_lines(m: &mut Machine, frame: u32, label: u32) {
    external(m, GET_VISUAL_OFFSET, [2, frame + 0x28, frame + 0x2a, 0]);
    let index = lbu(m, label + 5);
    let object = briefing(m)
        .wrapping_add(OBJECTS)
        .wrapping_add(index.wrapping_mul(OBJECT_SIZE));
    if m.lw(object) & 2 != 0 {
        return;
    }
    let block = work(m).wrapping_add(WORK_HIGH);
    let style = lbu(m, label + 4);
    let color = (frame + 0x10).wrapping_add(style << 2);
    for second in [false, true] {
        let block_high = if second {
            work(m).wrapping_add(WORK_HIGH)
        } else {
            block
        };
        let line = m.lw(block_high + 0x94);
        m.sw(block_high + 0x94, line.wrapping_add(0x10));
        sb(m, line + 3, 3);
        sb(m, line + 7, 0x40);
        for k in 0..3 {
            let channel = lbu(m, color.wrapping_add(k));
            sb(m, line + 4 + k, channel);
        }
        let x = lhu(m, label + 8);
        let dx = lhu(m, frame + 0x28);
        sh(m, line + 8, x.wrapping_add(dx).wrapping_add(0x4e));
        let y = lhu(m, label + 0xa);
        let dy = lhu(m, frame + 0x2a);
        sh(
            m,
            line + 0xa,
            y.wrapping_add(dy).wrapping_add(0x5a + u32::from(second)),
        );
        let sx = lhu(m, object + 0x1e);
        sh(m, line + 0xc, sx);
        let sy = lhu(m, object + 0x20);
        sh(m, line + 0xe, sy.wrapping_add(u32::from(second)));
        let block = work(m);
        let table = m.lw(block + 0x90);
        add_prim(m, table, line);
    }
}

// ---- 800EBCD8: the view matrix ------------------------------------------------------

/// 800EBCD8: build the briefing view. The geometry offset follows the view
/// offsets and panel slide; an orbit matrix (view angles, zoom distance,
/// centered on the camera) gives the eye position, and the view matrix
/// looks from the eye at the camera point (row 1 doubled by ov2core's
/// 800EF1F4), loaded into the
/// GTE and kept at state +0x554.
pub fn view_matrix(m: &mut Machine) {
    let frame = m.push_frame(0xd8);
    let state = briefing(m);
    let ox = lh(m, state + VIEW_OFFSETS).wrapping_add(0x9a);
    let oy = lh(m, state + VIEW_OFFSETS + 2).wrapping_add(0xae);
    let panel = lh(m, state + PANEL_OFFSETS + 2);
    external(m, SET_GEOM_OFFSET, [ox, panel.wrapping_add(oy), 0, 0]);
    let state = briefing(m);
    let angles = frame + 0x10;
    let orbit = frame + 0x20;
    let pitch = lhu(m, state + VIEW_ANGLES);
    sh(m, angles, pitch);
    let yaw = lhu(m, state + VIEW_ANGLES + 2);
    sh(m, angles + 4, 0);
    sh(m, angles + 8, 0);
    sh(m, angles + 0xa, 0);
    sh(m, angles + 2, yaw);
    let zoom = lhu(m, state + ZOOM);
    sh(m, angles + 0xc, zoom);
    external(m, ROT_MATRIX_YXZ, [angles, orbit, 0, 0]);
    let state = briefing(m);
    external(m, TRANS_MATRIX, [orbit, state + CAMERA, 0, 0]);
    load_rotation(m, orbit);
    load_translation(m, orbit);
    lwc2(m, reg::VXY0, frame + 0x18);
    lwc2(m, reg::VZ0, frame + 0x1c);
    cop2(m, MVMVA_RT_V0_TR);
    let eye = frame + 0x40;
    swc2(m, reg::MAC1, eye);
    swc2(m, reg::MAC2, eye + 4);
    swc2(m, reg::MAC3, eye + 8);
    let flag = m.gte.cfc2(ctrl::FLAG);
    m.sw(frame + 0xb8, flag);
    let state = briefing(m);
    let eye_x = m.lw(eye);
    let dx = m.lw(state + CAMERA).wrapping_sub(eye_x);
    let eye_z = m.lw(eye + 8);
    let dz = m.lw(state + CAMERA + 8).wrapping_sub(eye_z);
    let eye_y = m.lw(eye + 4);
    let dy = m.lw(state + CAMERA + 4).wrapping_sub(eye_y);
    let squares = mul(dx, dx).wrapping_add(mul(dz, dz));
    let distance = external(m, SQUARE_ROOT0, [squares, 0, 0, 0]);
    let elevation = external(m, RATAN2, [dy, distance, 0, 0]);
    let look = frame + 0x50;
    sh(m, look, 0u32.wrapping_sub(elevation));
    let heading = external(m, RATAN2, [dx, dz, 0, 0]);
    let rotation = frame + 0x68;
    sh(m, look + 2, heading);
    sh(m, look + 4, 0);
    external(m, ROT_MATRIX_YXZ, [look, rotation, 0, 0]);
    let offset = frame + 0x58;
    let x = m.lw(eye);
    let z = m.lw(eye + 8);
    m.sw(offset, 0u32.wrapping_sub(x));
    let y = m.lw(eye + 4);
    m.sw(offset + 8, 0u32.wrapping_sub(z));
    m.sw(offset + 4, 0u32.wrapping_sub(y));
    external(m, TRANS_MATRIX, [rotation, offset, 0, 0]);
    let view = frame + 0x88;
    external(m, TRANSPOSE_MATRIX, [rotation, view, 0, 0]);
    external(m, DOUBLE_ROW1, [view, 0, 0, 0]);
    let translated = frame + 0xa8;
    external(m, APPLY_MATRIX_LV, [view, offset, translated, 0]);
    external(m, TRANS_MATRIX, [view, translated, 0, 0]);
    load_rotation(m, view);
    load_translation(m, view);
    let state = briefing(m);
    for half_index in 0..2 {
        let words: Vec<u32> = (0..4)
            .map(|k| m.lw(view + half_index * 0x10 + k * 4))
            .collect();
        for (k, word) in words.into_iter().enumerate() {
            m.sw(state + VIEW_MATRIX + half_index * 0x10 + k as u32 * 4, word);
        }
    }
    m.pop_frame(0xd8);
}

// ---- 800EBF68: the terrain grid ------------------------------------------------------

/// One 4-bit height of the work block's height map, as compiled: the byte
/// at `half(sum)` (the map is 258 nibbles per row, offset 128), the nibble
/// selected by bit 0 of `column`, times 128.
fn height(m: &mut Machine, map: u32, sum: u32, column: u32) -> u32 {
    let byte = lbu(m, map.wrapping_add(half(sum)));
    ((byte >> ((column & 1) << 2)) & 0xf) << 7
}

/// `(row + offset) * 258 + bias` as compiled (`sll 7` + `addu`, `sll 1`).
fn row_sum(row: u32, offset: u32, bias: u32) -> u32 {
    let r = sext16(row).wrapping_add(offset);
    (((r << 7).wrapping_add(r)) << 1).wrapping_add(bias)
}

/// The interpolated color of three channels of the scratch words at `from`
/// and `to` by `t / 128`, keeping `from`'s top byte.
fn lerp_color(m: &mut Machine, from: u32, to: u32, t: u32) -> u32 {
    let mut channels = [0u32; 3];
    for (k, channel) in channels.iter_mut().enumerate() {
        let k = k as u32;
        let a = lbu(m, SCRATCH + from + k);
        let d = lbu(m, SCRATCH + to + k).wrapping_sub(a);
        *channel = a.wrapping_add(div_pow2(mul(d, t), 7));
    }
    let top = lbu(m, SCRATCH + from + 3);
    channels[0] | (channels[1] << 8) | (channels[2] << 16) | (top << 24)
}

/// Edge interpolation of a height: `base + (to - from) * t / 128` (signed
/// 16-bit heights).
fn lerp_height(base: u32, from: u32, to: u32, t: u32) -> u32 {
    let d = sext16(to).wrapping_sub(sext16(from));
    base.wrapping_add(div_pow2(mul(d, t), 7))
}

/// 800EBF68: the terrain: a full-screen DR_AREA, then 15 x 15 Gouraud
/// quads (POLY_G4) of the height map around the camera, the outer ring
/// clipped to the camera's sub-cell position (heights and colors
/// interpolated), linked at `slot`; then a DR_AREA for the panel view.
/// Returns the next free primitive.
pub fn terrain(m: &mut Machine, prim: u32, slot: u32) -> u32 {
    let frame = m.push_frame(0x50);
    copy_rect(m, SCREEN_AREA, frame + 0x10);
    external(m, SET_DRAW_AREA, [prim, frame + 0x10, 0, 0]);
    add_prim(m, slot, prim);
    let mut cursor = prim + 0xc;
    let s = SCRATCH;
    let mut row = 0xffff_fff9u32;
    loop {
        let state = briefing(m);
        let z = m.lw(state + CAMERA + 8);
        sh(m, s + 2, row.wrapping_add(div_pow2(z, 7)));
        let z = m.lw(state + CAMERA + 8);
        sh(m, s + 6, z.wrapping_sub(div_pow2(z, 7) << 7));
        if (m.lw(state + CAMERA + 8) as i32) < 0 {
            let gz = lhu(m, s + 2);
            let fz = lhu(m, s + 6);
            sh(m, s + 2, gz.wrapping_sub(1));
            sh(m, s + 6, fz.wrapping_add(0x80));
        }
        let row16 = sext16(row);
        let mut column = 0xffff_fff9u32;
        loop {
            let state = briefing(m);
            let x = m.lw(state + CAMERA);
            m.sw(frame + 0x20, x);
            let q = div_pow2(x, 7);
            sh(m, s, column.wrapping_add(q));
            sh(m, s + 4, x.wrapping_sub(q << 7));
            if (x as i32) < 0 {
                let gx = lhu(m, s);
                let fx = lhu(m, s + 4);
                sh(m, s, gx.wrapping_sub(1));
                sh(m, s + 4, fx.wrapping_add(0x80));
            }
            let poly = cursor;
            cursor = cursor.wrapping_add(0x24);
            let left = sext16(column) == 0xffff_fff9;
            let right = sext16(column) == 7;
            if left {
                let gz = lhu(m, s + 2);
                let gx = lh(m, s);
                let sum = gx.wrapping_add(row_sum(gz, 0x80, 0x80));
                let block = work(m).wrapping_add(WORK_HIGH);
                let map = m.lw(block + 0x98);
                let gx_raw = lhu(m, s);
                let gx = lh(m, s);
                let h0 = height(m, map, sum, gx_raw);
                sh(m, s + 0x20, gx << 7);
                sh(m, s + 8, h0);
                let gz = lhu(m, s + 2);
                sh(m, s + 0x22, 0u32.wrapping_sub(h0));
                sh(m, s + 0x24, sext16(gz) << 7);
            } else {
                let x = lhu(m, s + 0x28);
                let y = lhu(m, s + 0x2a);
                let z = lhu(m, s + 0x2c);
                let h = lhu(m, s + 0xa);
                sh(m, s + 0x20, x);
                sh(m, s + 0x22, y);
                sh(m, s + 0x24, z);
                sh(m, s + 8, h);
            }
            // The other three corners: (x+1, z), (x, z+1), (x+1, z+1).
            let gz = lhu(m, s + 2);
            let gx0 = lh(m, s);
            let block = work(m).wrapping_add(WORK_HIGH);
            let gx = lh(m, s);
            let sum = gx0.wrapping_add(row_sum(gz, 0x80, 0x81));
            let map = m.lw(block + 0x98);
            let h1 = height(m, map, sum, gx0.wrapping_add(1));
            let gz = lhu(m, s + 2);
            sh(m, s + 0xa, h1);
            let sum = lh(m, s).wrapping_add(row_sum(gz, 0x81, 0x80));
            let map = m.lw(block + 0x98);
            let gx_raw = lhu(m, s);
            let h2 = height(m, map, sum, gx_raw);
            let gz = lhu(m, s + 2);
            let sum = gx.wrapping_add(row_sum(gz, 0x81, 0x81));
            sh(m, s + 0xc, h2);
            let map = m.lw(block + 0x98);
            let byte = lbu(m, map.wrapping_add(half(sum)));
            let h1 = lhu(m, s + 0xa);
            sh(m, s + 0x28, gx.wrapping_add(1) << 7);
            let gz = lhu(m, s + 2);
            sh(m, s + 0x2a, 0u32.wrapping_sub(h1));
            sh(m, s + 0x30, gx << 7);
            let h2 = lhu(m, s + 0xc);
            sh(m, s + 0x2c, sra(gz << 16, 9));
            let gz = lhu(m, s + 2);
            sh(m, s + 0x32, 0u32.wrapping_sub(h2));
            let h3 = ((byte >> ((gx.wrapping_add(1) & 1) << 2)) & 0xf) << 7;
            sh(m, s + 0x34, sra(gz << 16, 9).wrapping_add(0x80));
            sh(m, s + 0xe, h3);
            sh(m, s + 0x38, gx.wrapping_add(1) << 7);
            let gz = lhu(m, s + 2);
            sh(m, s + 0x3a, 0u32.wrapping_sub(h3));
            sh(m, s + 0x3c, sra(gz << 16, 9).wrapping_add(0x80));
            if left {
                let h0 = lhu(m, s + 8);
                let h1 = lhu(m, s + 0xa);
                let fx = sext16(lhu(m, s + 4));
                let x0 = lhu(m, s + 0x20);
                let fx_raw = lhu(m, s + 4);
                let x2 = lhu(m, s + 0x30);
                sh(m, s + 0x20, x0.wrapping_add(fx_raw));
                sh(m, s + 0x30, x2.wrapping_add(fx_raw));
                let h3 = lhu(m, s + 0xe);
                let h2 = lhu(m, s + 0xc);
                let fx2 = sext16(lhu(m, s + 4));
                sh(m, s + 8, lerp_height(h0, h0, h1, fx));
                sh(m, s + 0xc, lerp_height(h2, h2, h3, fx2));
                let h0 = lhu(m, s + 8);
                let h2 = lhu(m, s + 0xc);
                sh(m, s + 0x22, 0u32.wrapping_sub(h0));
                sh(m, s + 0x32, 0u32.wrapping_sub(h2));
            }
            if right {
                let h1 = lhu(m, s + 0xa);
                let h0 = lhu(m, s + 8);
                let t = sext16(lhu(m, s + 4)).wrapping_sub(0x80);
                let x1 = lhu(m, s + 0x28).wrapping_sub(0x80);
                let fx = lhu(m, s + 4);
                sh(m, s + 0x28, fx.wrapping_add(x1));
                let x3 = lhu(m, s + 0x38).wrapping_sub(0x80);
                let fx = lhu(m, s + 4);
                sh(m, s + 0x38, fx.wrapping_add(x3));
                let h3 = lhu(m, s + 0xe);
                let h2 = lhu(m, s + 0xc);
                let t2 = sext16(lhu(m, s + 4)).wrapping_sub(0x80);
                sh(m, s + 0xa, lerp_height(h1, h0, h1, t));
                sh(m, s + 0xe, lerp_height(h3, h2, h3, t2));
                let h1 = lhu(m, s + 0xa);
                let h3 = lhu(m, s + 0xe);
                sh(m, s + 0x2a, 0u32.wrapping_sub(h1));
                sh(m, s + 0x3a, 0u32.wrapping_sub(h3));
            }
            if row16 == 0xffff_fff9 {
                let h2 = lhu(m, s + 0xc);
                let h0 = lhu(m, s + 8);
                let fz = sext16(lhu(m, s + 6));
                let z0 = lhu(m, s + 0x24);
                let fz_raw = lhu(m, s + 6);
                let z1 = lhu(m, s + 0x2c);
                sh(m, s + 0x24, z0.wrapping_add(fz_raw));
                sh(m, s + 0x2c, z1.wrapping_add(fz_raw));
                let h3 = lhu(m, s + 0xe);
                let h1 = lhu(m, s + 0xa);
                let fz2 = sext16(lhu(m, s + 6));
                sh(m, s + 8, lerp_height(h0, h0, h2, fz));
                sh(m, s + 0xa, lerp_height(h1, h1, h3, fz2));
                let h0 = lhu(m, s + 8);
                let h1 = lhu(m, s + 0xa);
                sh(m, s + 0x22, 0u32.wrapping_sub(h0));
                sh(m, s + 0x2a, 0u32.wrapping_sub(h1));
            }
            if row16 == 7 {
                let h2 = lhu(m, s + 0xc);
                let h0 = lhu(m, s + 8);
                let t = sext16(lhu(m, s + 6)).wrapping_sub(0x80);
                let z2 = lhu(m, s + 0x34).wrapping_sub(0x80);
                let fz = lhu(m, s + 6);
                sh(m, s + 0x34, fz.wrapping_add(z2));
                let z3 = lhu(m, s + 0x3c).wrapping_sub(0x80);
                let fz = lhu(m, s + 6);
                sh(m, s + 0x3c, fz.wrapping_add(z3));
                let h3 = lhu(m, s + 0xe);
                let h1 = lhu(m, s + 0xa);
                let t2 = sext16(lhu(m, s + 6)).wrapping_sub(0x80);
                sh(m, s + 0xc, lerp_height(h2, h0, h2, t));
                sh(m, s + 0xe, lerp_height(h3, h1, h3, t2));
                let h2 = lhu(m, s + 0xc);
                let h3 = lhu(m, s + 0xe);
                sh(m, s + 0x32, 0u32.wrapping_sub(h2));
                sh(m, s + 0x3a, 0u32.wrapping_sub(h3));
            }
            if left {
                lwc2(m, reg::VXY0, s + 0x20);
                lwc2(m, reg::VZ0, s + 0x24);
                cop2(m, RTPS);
                swc2(m, reg::SXY2, poly + 8);
            } else {
                swc2(m, reg::SXY0, poly + 8);
            }
            lwc2(m, reg::VXY0, s + 0x28);
            lwc2(m, reg::VZ0, s + 0x2c);
            lwc2(m, reg::VXY1, s + 0x30);
            lwc2(m, reg::VZ1, s + 0x34);
            lwc2(m, reg::VXY2, s + 0x38);
            lwc2(m, reg::VZ2, s + 0x3c);
            cop2(m, RTPT);
            for (k, offset) in [8u32, 0xa, 0xc, 0xe].into_iter().enumerate() {
                let h = sext16(lhu(m, s + offset));
                let color = m.lw(TERRAIN_COLORS.wrapping_add(div_pow2(h, 7) << 2));
                m.sw(s + 0x10 + k as u32 * 4, color);
            }
            swc2(m, reg::SXY0, poly + 0x10);
            swc2(m, reg::SXY1, poly + 0x18);
            swc2(m, reg::SXY2, poly + 0x20);
            let z = m.gte.mfc2(reg::SZ3);
            m.sw(frame + 0x20, sra(z, 2));
            if left {
                let fx = sext16(lhu(m, s + 4));
                let c0 = lerp_color(m, 0x10, 0x14, fx);
                let fx = sext16(lhu(m, s + 4));
                let c2 = lerp_color(m, 0x18, 0x1c, fx);
                m.sw(s + 0x10, c0);
                m.sw(s + 0x18, c2);
            }
            if row16 == 0xffff_fff9 {
                let fz = sext16(lhu(m, s + 6));
                let c0 = lerp_color(m, 0x10, 0x18, fz);
                m.sw(s + 0x10, c0);
                let fz = sext16(lhu(m, s + 6));
                let c1 = lerp_color(m, 0x14, 0x1c, fz);
                m.sw(s + 0x14, c1);
            }
            if right {
                let t = sext16(lhu(m, s + 4)).wrapping_sub(0x80);
                let c1 = lerp_color(m, 0x14, 0x10, t);
                m.sw(s + 0x14, c1);
                let t = sext16(lhu(m, s + 4)).wrapping_sub(0x80);
                let c3 = lerp_color(m, 0x1c, 0x18, t);
                m.sw(s + 0x1c, c3);
            }
            if row16 == 7 {
                let t = sext16(lhu(m, s + 6)).wrapping_sub(0x80);
                let c2 = lerp_color(m, 0x18, 0x10, t);
                m.sw(s + 0x18, c2);
                let t = sext16(lhu(m, s + 6)).wrapping_sub(0x80);
                let c3 = lerp_color(m, 0x1c, 0x14, t);
                m.sw(s + 0x1c, c3);
            }
            column = column.wrapping_add(1);
            let c0 = m.lw(s + 0x10);
            m.sw(poly + 4, c0 | 0x3800_0000);
            let c1 = m.lw(s + 0x14);
            m.sw(poly + 0xc, c1);
            let c2 = m.lw(s + 0x18);
            m.sw(poly + 0x14, c2);
            let c3 = m.lw(s + 0x1c);
            m.sw(poly + 0x1c, c3);
            let link = m.lw(slot);
            m.sw(poly, link | 0x0800_0000);
            m.sw(slot, (poly << 8) >> 8);
            if !slt(sext16(column), 8) {
                break;
            }
        }
        row = row.wrapping_add(1);
        if !slt(sext16(row), 8) {
            break;
        }
    }
    let area = cursor;
    let state = briefing(m);
    let rect = frame + 0x18;
    sh(m, rect, 0x54);
    let offset = lhu(m, state + VIEW_OFFSETS + 2);
    let panel = lhu(m, state + PANEL_OFFSETS + 2);
    sh(m, rect + 4, 0x96);
    sh(m, rect + 6, 0x140);
    sh(m, rect + 2, offset.wrapping_add(panel).wrapping_sub(0x42));
    external(m, SET_DRAW_AREA, [area, rect, 0, 0]);
    add_prim(m, slot, area);
    m.pop_frame(0x50);
    area + 0xc
}

// ---- 800ECE7C: the objects --------------------------------------------------------------

/// The appearance divisions of 800ECE7C: (corner halfword at `frame +`,
/// negated, `div` address). The corners are (-s, 0, s), (s, 0, s),
/// (-s, 0, -s), (s, 0, -s), with `s` growing to 128 while the object
/// appears; each x halfword (offsets 0x10/0x18/0x20/0x28) follows the zero
/// y halfword after it.
const QUAD_DIVISIONS: [(u32, bool, u32); 8] = [
    (0x10, true, 0x800e_cfa0),
    (0x14, false, 0x800e_cfe8),
    (0x18, false, 0x800e_d030),
    (0x1c, false, 0x800e_d078),
    (0x20, true, 0x800e_d0c4),
    (0x24, true, 0x800e_d110),
    (0x28, false, 0x800e_d158),
    (0x2c, true, 0x800e_d1a4),
];

/// 800ECE7C: every placed object within 0x380 of the camera (others get
/// flag 2) as a textured quad (POLY_FT4, texture row = type * 16, CLUT
/// 0xF5B, tpage 9) at its position and heading, scaled up during its
/// appearance timer; its screen position (+0x1E/+0x20) is the midpoint of
/// the first and fourth corners. Returns the next free primitive.
pub fn objects(m: &mut Machine, prim: u32, table: u32) -> u32 {
    let frame = m.push_frame(0xd8);
    m.sw(frame + 0xdc, table);
    let corners = frame + 0x10;
    let rotation = frame + 0x50;
    let product = frame + 0x70;
    let mut cursor = prim;
    let mut quad = prim.wrapping_sub(0x28);
    for index in 0..32u32 {
        let state = briefing(m);
        let object = state + OBJECTS + index * OBJECT_SIZE;
        if m.lw(object) & 1 == 0 {
            continue;
        }
        sh(m, object + 0x1e, 0xffff);
        sh(m, object + 0x20, 0xffff);
        let near_x = {
            let x = lh(m, object + 0xe);
            let camera = m.lw(state + CAMERA);
            slt(abs(x.wrapping_sub(camera)), 0x381)
        };
        let near = near_x && {
            let z = lh(m, object + 0x12);
            let camera = m.lw(state + CAMERA + 8);
            slt(abs(z.wrapping_sub(camera)), 0x381)
        };
        if !near {
            let flags = m.lw(object);
            m.sw(object, flags | 2);
            continue;
        }
        let flags = m.lw(object) & !2;
        let kind = lh(m, object + 6);
        m.sw(object, flags);
        if kind == 1 && (lh(m, object + 8) as i32) > 0 {
            for (offset, negated, pc) in QUAD_DIVISIONS {
                let total = lh(m, object + 0xa);
                let left = lh(m, object + 8);
                let mut scaled = total.wrapping_sub(left) << 7;
                if negated {
                    scaled = 0u32.wrapping_sub(scaled);
                }
                let s = div(m, scaled, total, pc);
                if offset % 8 == 0 {
                    sh(m, frame + offset + 2, 0);
                }
                sh(m, frame + offset, s);
            }
            let left = lhu(m, object + 8);
            sh(m, object + 8, left.wrapping_sub(1));
        } else {
            for (offset, value) in [
                (0x10, 0xff80),
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
                sh(m, frame + offset, value);
            }
        }
        external(m, ROT_MATRIX, [object + 0x16, rotation, 0, 0]);
        let position = frame + 0x90;
        for k in 0..3 {
            let value = lh(m, object + 0xe + k * 2);
            m.sw(position + k * 4, value);
        }
        external(m, TRANS_MATRIX, [rotation, position, 0, 0]);
        // Inline MulMatrix: product = view rotation x object rotation, by
        // columns.
        let view = briefing(m) + VIEW_MATRIX;
        load_rotation(m, view);
        for column in 0..3 {
            let from = rotation + column * 2;
            let t4 = lhu(m, from);
            let t5 = lhu(m, from + 6);
            let t6 = lhu(m, from + 0xc);
            m.gte.mtc2(reg::IR1, t4);
            m.gte.mtc2(reg::IR2, t5);
            m.gte.mtc2(reg::IR3, t6);
            cop2(m, MVMVA_RT_IR);
            let to = product + column * 2;
            let t4 = m.gte.mfc2(reg::IR1);
            let t5 = m.gte.mfc2(reg::IR2);
            let t6 = m.gte.mfc2(reg::IR3);
            sh(m, to, t4);
            sh(m, to + 6, t5);
            sh(m, to + 0xc, t6);
        }
        // The object's translation through the view matrix.
        let view = briefing(m) + VIEW_MATRIX;
        load_translation(m, view);
        let t5 = lhu(m, rotation + 0x18);
        let t4 = lhu(m, rotation + 0x14);
        m.gte.mtc2(reg::VXY0, t4 | (t5 << 16));
        lwc2(m, reg::VZ0, rotation + 0x1c);
        cop2(m, MVMVA_RT_V0_TR);
        swc2(m, reg::MAC1, product + 0x14);
        swc2(m, reg::MAC2, product + 0x18);
        swc2(m, reg::MAC3, product + 0x1c);
        load_rotation(m, product);
        load_translation(m, product);
        lwc2(m, reg::VXY0, corners);
        lwc2(m, reg::VZ0, corners + 4);
        lwc2(m, reg::VXY1, corners + 8);
        lwc2(m, reg::VZ1, corners + 0xc);
        lwc2(m, reg::VXY2, corners + 0x10);
        lwc2(m, reg::VZ2, corners + 0x14);
        cop2(m, RTPT);
        let screen = frame + 0x30;
        swc2(m, reg::SXY0, screen);
        swc2(m, reg::SXY1, screen + 8);
        swc2(m, reg::SXY2, screen + 0x10);
        store_depth(m, frame + 0xa0, frame + 0xa4, frame + 0xa8);
        lwc2(m, reg::VXY0, corners + 0x18);
        lwc2(m, reg::VZ0, corners + 0x1c);
        cop2(m, RTPS);
        swc2(m, reg::SXY2, screen + 0x18);
        store_depth(m, frame + 0xa0, frame + 0xa4, frame + 0xa8);
        let x = lh(m, screen).wrapping_add(lh(m, screen + 0x18));
        sh(m, object + 0x1e, half(x));
        quad = quad.wrapping_add(0x28);
        let y = lh(m, screen + 2).wrapping_add(lh(m, screen + 0x1a));
        sh(m, object + 0x20, half(y));
        sb(m, quad + 3, 9);
        sb(m, quad + 7, 0x2c);
        sh(m, quad + 0xe, 0xf5b);
        sh(m, quad + 0x16, 9);
        for k in 0..4 {
            let xy = screen + k * 8;
            let sx = lhu(m, xy);
            sh(m, quad + 8 + k * 8, sx);
            let sy = lhu(m, xy + 2);
            sh(m, quad + 0xa + k * 8, sy);
        }
        let kind = lh(m, object + 4);
        sb(m, quad + 0xd, 0);
        sb(m, quad + 0xc, kind << 4);
        let kind = lh(m, object + 4);
        sb(m, quad + 0x15, 0);
        sb(m, quad + 0x14, (kind << 4).wrapping_add(0xf));
        let kind = lh(m, object + 4);
        sb(m, quad + 0x1c, kind << 4);
        sb(m, quad + 0x1d, 0xf);
        let kind = lh(m, object + 4);
        sb(m, quad + 0x25, 0xf);
        sb(m, quad + 4, 0x80);
        sb(m, quad + 5, 0x80);
        sb(m, quad + 6, 0x80);
        sb(m, quad + 0x24, (kind << 4).wrapping_add(0xf));
        let table = m.lw(frame + 0xdc);
        add_prim(m, table, quad);
        cursor = cursor.wrapping_add(0x28);
    }
    m.pop_frame(0xd8);
    cursor
}

// ---- 800ED63C: the timer marker ---------------------------------------------------------

/// 800ED63C: while a marker is set (state +0x514 >= 0) and within 0x380 of
/// the camera, its time as minutes:seconds (format at 0x800CB35C, spaces
/// as '0') in font 11 at its projected position (x - 10). Returns the next
/// free primitive.
pub fn marker(m: &mut Machine, cursor: u32, table: u32) -> u32 {
    let frame = m.push_frame(0x60);
    let result = marker_text(m, frame, cursor, table);
    m.pop_frame(0x60);
    result
}

fn marker_text(m: &mut Machine, frame: u32, cursor: u32, table: u32) -> u32 {
    let state = briefing(m);
    if (m.lw(state + MARKER_ID) as i32) < 0 {
        return cursor;
    }
    let view = state + VIEW_MATRIX;
    load_rotation(m, view);
    load_translation(m, view);
    lwc2(m, reg::VXY0, state + MARKER);
    lwc2(m, reg::VZ0, state + MARKER + 4);
    cop2(m, RTPS);
    let screen = frame + 0x20;
    swc2(m, reg::SXY2, screen);
    store_depth(m, frame + 0x40, frame + 0x44, frame + 0x48);
    let state = briefing(m);
    let x = lh(m, state + MARKER);
    let camera = m.lw(state + CAMERA);
    if !slt(abs(x.wrapping_sub(camera)), 0x381) {
        return cursor;
    }
    let z = lh(m, state + MARKER + 4);
    let camera = m.lw(state + CAMERA + 8);
    if !slt(abs(z.wrapping_sub(camera)), 0x381) {
        return cursor;
    }
    let time = m.lw(state + MARKER_ID);
    // time / 60: mult by 0x88888889, mfhi + time, sra 5, minus the sign.
    let minutes = sra(mulhi(time, 0x8888_8889).wrapping_add(time), 5).wrapping_sub(sra(time, 31));
    let seconds = time.wrapping_sub(((minutes << 4).wrapping_sub(minutes)) << 2);
    let text = frame + 0x28;
    external(m, SPRINTF, [text, MARKER_FORMAT, minutes, seconds]);
    let length = external(m, STRLEN, [text, 0, 0, 0]);
    let mut index = 0u32;
    while slt(index, length) {
        let at = text.wrapping_add(index);
        if lbu(m, at) == 0x20 {
            sb(m, at, 0x30);
        }
        index = index.wrapping_add(1);
    }
    let sx = lh(m, screen);
    m.sw(frame + 0x10, sx.wrapping_sub(0xa));
    let sy = lh(m, screen + 2);
    m.sw(frame + 0x18, 0x0080_8080);
    m.sw(frame + 0x14, sy);
    external(m, DRAW_TEXT, [0xb, text, table, cursor])
}

// ---- 800ED834: the scene ----------------------------------------------------------------

/// 800ED834: while the scene flag (state flags & 1) is set: the view
/// matrix, the terrain (linked at `table + 4`), the marker and the objects
/// (at `table`). Returns the next free primitive.
pub fn draw(m: &mut Machine, cursor: u32, table: u32) -> u32 {
    m.push_frame(0x20);
    let state = briefing(m);
    let result = if m.lw(state + FLAGS) & 1 != 0 {
        view_matrix(m);
        let cursor = terrain(m, cursor, table.wrapping_add(4));
        let cursor = marker(m, cursor, table);
        objects(m, cursor, table)
    } else {
        cursor
    };
    m.pop_frame(0x20);
    result
}

// ---- 800ED8B0: the minimap ----------------------------------------------------------------

/// 800ED8B0: the minimap at `slot`: a full-screen DR_AREA; a 3 x 6 TILE
/// per placed object (types 0..10 except 9, colored by type through the
/// jump table at 0x800CB368) relative to the camera; the map texture
/// (POLY_FT4 128 x 128, CLUT 0x5714, tpage 5) scrolled by the camera; and a
/// DR_AREA clipping to the map window. Returns the next free primitive.
pub fn minimap(m: &mut Machine, prim: u32, slot: u32) -> u32 {
    let frame = m.push_frame(0x50);
    copy_rect(m, SCREEN_AREA, frame + 0x10);
    external(m, SET_DRAW_AREA, [prim, frame + 0x10, 0, 0]);
    add_prim(m, slot, prim);
    let mut cursor = prim + 0xc;
    let state = briefing(m);
    let camera_x = div_pow2(m.lw(state + CAMERA), 7).wrapping_add(0x80);
    let camera_z = div_pow2(0u32.wrapping_sub(m.lw(state + CAMERA + 8)), 7).wrapping_add(0x80);
    let center = sext16(camera_x).wrapping_sub(0x80);
    for index in 0..32u32 {
        let object = briefing(m) + OBJECTS + index * OBJECT_SIZE;
        if m.lw(object) & 1 == 0 {
            continue;
        }
        let kind = lh(m, object + 4);
        if kind == 9 || !slt(kind, 0xb) {
            continue;
        }
        let x = div_pow2(lh(m, object + 0xe), 7);
        let z = 0u32.wrapping_sub(lh(m, object + 0x12));
        let dx = half(x.wrapping_sub(center));
        let tile = cursor;
        sb(m, tile + 3, 3);
        sb(m, tile + 7, 0x60);
        let state = briefing(m);
        sh(m, tile + 8, dx.wrapping_add(0x113));
        let panel = lhu(m, state + PANEL_OFFSETS);
        sh(m, tile + 0xc, 3);
        sh(m, tile + 0xe, 6);
        let y = div_pow2(z, 7)
            .wrapping_sub(camera_z.wrapping_sub(0x80))
            .wrapping_add(panel.wrapping_add(0xa4));
        sh(m, tile + 0xa, y);
        let kind = lh(m, object + 4);
        cursor = cursor.wrapping_add(0x10);
        // Jump table 0x800CB368: 0, 1, 10 -> 800EDAC0; 2, 3 -> 800EDAD8;
        // 4, 5 -> 800EDAEC; 6, 7 -> 800EDB00; 8 -> 800EDB18; 9 -> no color.
        let color = match kind {
            0 | 1 | 10 => Some([0x38, 0x60, 0xc8]),
            2 | 3 => Some([0xff, 0xff, 0xff]),
            4 | 5 => Some([0xe8, 0xd0, 0x68]),
            6 | 7 => Some([0xf0, 0x50, 0x68]),
            8 => Some([0x90, 0xd0, 0xd0]),
            _ => None,
        };
        if let Some(rgb) = color {
            for (k, channel) in rgb.into_iter().enumerate() {
                sb(m, tile + 4 + k as u32, channel);
            }
        }
        add_prim(m, slot, tile);
    }
    let poly = cursor;
    sb(m, poly + 3, 9);
    sb(m, poly + 7, 0x2c);
    let shift = half(sext16(camera_x));
    let left = 0x113u32.wrapping_sub(shift);
    let right = 0x193u32.wrapping_sub(shift);
    sb(m, poly + 4, 0x80);
    sb(m, poly + 5, 0x80);
    sb(m, poly + 6, 0x80);
    let state = briefing(m);
    let area = poly + 0x28;
    sh(m, poly + 8, left);
    let panel = lhu(m, state + PANEL_OFFSETS);
    sh(m, poly + 0x10, right);
    sh(
        m,
        poly + 0xa,
        panel.wrapping_add(0xa4).wrapping_sub(camera_z),
    );
    let panel = lhu(m, state + PANEL_OFFSETS);
    sh(m, poly + 0x18, left);
    sh(
        m,
        poly + 0x12,
        panel.wrapping_add(0xa4).wrapping_sub(camera_z),
    );
    let panel = lhu(m, state + PANEL_OFFSETS);
    sh(m, poly + 0x20, right);
    sh(
        m,
        poly + 0x1a,
        panel.wrapping_sub(camera_z).wrapping_add(0x1a4),
    );
    let panel = lhu(m, state + PANEL_OFFSETS);
    sh(m, poly + 0x16, 5);
    sb(m, poly + 0xc, 0);
    sb(m, poly + 0xd, 0);
    sb(m, poly + 0x14, 0x80);
    sb(m, poly + 0x15, 0);
    sb(m, poly + 0x1c, 0);
    sb(m, poly + 0x1d, 0x80);
    sb(m, poly + 0x24, 0x80);
    sb(m, poly + 0x25, 0x80);
    sh(m, poly + 0xe, 0x5714);
    sh(
        m,
        poly + 0x22,
        panel.wrapping_sub(camera_z).wrapping_add(0x1a4),
    );
    add_prim(m, slot, poly);
    let state = briefing(m);
    let window = frame + 0x20;
    sh(m, window, 0xf3);
    let panel = lhu(m, state + PANEL_OFFSETS);
    sh(m, window + 4, 0x40);
    sh(m, window + 6, 0x80);
    sh(m, window + 2, panel.wrapping_add(0x64));
    copy_rect(m, window, frame + 0x18);
    external(m, SET_DRAW_AREA, [area, frame + 0x18, 0, 0]);
    add_prim(m, slot, area);
    m.pop_frame(0x50);
    area + 0xc
}
