//! JP overlay 2: the story-event block `[0x80112DB0]` (800ED15C..800F00A7):
//! its picture script (13 opcodes, 800EDBCC), the broadcast picture, the
//! selection list and the 3D map transitions; translated from the JP
//! instructions (class `new`, except 800ED468 `changed`: PAL 800CF014 is a
//! different routine now).
//!
//! Block layout (as far as these routines use it): +2 a halfword cleared at
//! the script end, +0x10 the script clock, +0x14 the script pointer;
//! +0x2C / +0x2AC the two buffers of 16 POLY_FT4 of the 160-byte slots, and
//! the slots at +0x52C + 0xA0 * n (+0 mode, +2..+0x16 move parameters,
//! +0x18, +0x20.. a 16-byte texture record per frame); +0xF34 the stream
//! resource, +0xF38 a flag, +0xF3C the stream's frame count, +0xF40 the
//! progress; +0xF4C / +0x11CC the POLY_FT4 of the 32-byte slots at
//! +0x1450 + 0x20 * n; +0x1650 / +0x1654 / +0x1674 the stream's quads;
//! +0x16B0 the list, +0x16B1 its state, +0x16B2 its cursor, +0x16B4 /
//! +0x1790 the list's two sprite buffers (11 SPRT), +0x186C / +0x1884
//! their DR_MODE; +0x189C the event record pointer.
use super::ov2_p4::{At, call, div, lh, lt, magic_div, sext16, sra};
use super::ov2_p4_load::release_ecd20;
use super::ov2_p4_stream::open_ec6b4;
use crate::common::{CONTEXT, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112DB0]`: the story-event block.
pub const BLOCK: u32 = 0x8011_2db0;
/// `[0x80112DC4]`: the hub block.
const HUB: u32 = 0x8011_2dc4;
/// 0x8010B980: the 13 picture-script opcodes.
const OPCODES: u32 = 0x8010_b980;
/// 0x8010B37C: six event ids with a list; 0x8010B400: 8-byte event
/// records (+0 id, +6 kind); 0x8010B388: the 20-byte lists (+0 entry count,
/// +4 two style bytes per entry).
const LIST_EVENTS: u32 = 0x8010_b37c;
const EVENTS: u32 = 0x8010_b400;
const LISTS: u32 = 0x8010_b388;
/// libgpu SetDrawMode; the sound request (8002DB4C).
const SET_DRAW_MODE: u32 = 0x800a_c7f8;
const PLAY_SOUND: u32 = 0x8002_db4c;
/// Overlay 2 (other parts): 800DEF94 / 800DEE08 / 800DEE84 style tables,
/// 800F7244 a shape into a picture.
const STYLE_BYTE: u32 = 0x800d_ef94;
const STYLE_ENTRY: u32 = 0x800d_ee08;
const STYLE_HALF: u32 = 0x800d_ee84;
const DRAW_SHAPE: u32 = 0x800f_7244;

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x800e_d15c => op_end_ed15c(m),
        0x800e_d16c => op_flag_ed16c(m),
        0x800e_d190 => op_quad_ed190(m),
        0x800e_d2b4 => op_quad_move_ed2b4(m),
        0x800e_d398 => op_quad_return_ed398(m),
        0x800e_d468 => op_release_ed468(m),
        0x800e_d498 => op_picture_ed498(m, a1),
        0x800e_d6e8 => op_picture_move_ed6e8(m),
        0x800e_d7d4 => op_picture_set_ed7d4(m),
        0x800e_d880 => op_picture_return_ed880(m),
        0x800e_d958 => op_picture_frame_ed958(m),
        0x800e_da60 => op_picture_hold_eda60(m),
        0x800e_dad4 => op_stream_edad4(m),
        0x800e_dbcc => run_script_edbcc(m),
        0x800e_dc80 => find_edc80(m, a0, a1, a2),
        0x800e_dcc0 => open_broadcast_edcc0(m, a0),
        0x800e_ddf4 => list_init_eddf4(m),
        0x800e_df84 => progress_edf84(m),
        0x800e_e0d8 => list_tick_ee0d8(m),
        0x800e_e570 => list_tick_all_ee570(m),
        0x800e_e9e4 => wireframe_ee9e4(m),
        // v0 for flags other than 1 and 2 is the caller's s1: not modelled
        0x800e_f4f4 => {
            slots_ef4f4(m);
            0
        }
        _ => return None,
    })
}

fn block(m: &mut Machine) -> u32 {
    m.lw(BLOCK)
}

/// Signed halfword `x / 2` as compiled (`(x + sign) >> 1`).
fn half16(x: u32) -> u32 {
    let s = sext16(x);
    sra(s.wrapping_add(s >> 31), 1)
}

/// The script command at `[block + 0x14]`, which advances by `size`.
fn command(m: &mut Machine, size: u32) -> (u32, u32) {
    let b = block(m);
    let cmd = m.lw(b.at(0x14));
    m.sw(b.at(0x14), cmd.at(size));
    (b, cmd)
}

/// A 32-byte slot (+0x1450).
fn slot32(m: &mut Machine, b: u32, cmd: u32) -> u32 {
    let slot = lh(m, cmd);
    b.wrapping_add((slot << 5).wrapping_add(0x1450))
}

/// A 160-byte slot (+0x52C).
fn slot160(m: &mut Machine, b: u32, cmd: u32) -> u32 {
    let slot = lh(m, cmd);
    b.wrapping_add(slot.wrapping_mul(0xa0).wrapping_add(0x52c))
}

/// Opcode 0, 800ED15C: the script ends (block +2 = 0). Returns the block.
pub fn op_end_ed15c(m: &mut Machine) -> u32 {
    let b = block(m);
    m.sh(b.at(2), 0);
    b
}

/// Opcode 1, 800ED16C: flag +0xF38 = 1. Returns the next command.
pub fn op_flag_ed16c(m: &mut Machine) -> u32 {
    let b = block(m);
    let cmd = m.lw(b.at(0x14));
    m.sw(b.at(0xf38), 1);
    let next = cmd.at(4);
    m.sw(b.at(0x14), next);
    next
}

/// Opcode 2, 800ED190 (slot, blend, rgb, rectangle): a 32-byte slot's
/// rectangle (+0x18) and its two POLY_FT4: colour, semi-transparent unless
/// the blend is -1 (blend modes 0..3), tpage 0x11D. Returns 0.
pub fn op_quad_ed190(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 0x10);
    let e = slot32(m, b, cmd);
    for offset in [0x18u32, 0x1a, 0x1c, 0x1e] {
        let v = m.lhu(cmd.at(offset - 0x10));
        m.sh(e.at(offset), v);
    }
    let blend = lh(m, cmd.at(2));
    let raw = u32::from(m.lhu(cmd.at(2)));
    let (semi, mode) = if blend == u32::MAX {
        (false, 0)
    } else {
        (true, if raw < 4 { blend } else { 0 })
    };
    let tpage = ((mode & 3) << 5) | 0x11d;
    let mut first = 0xf4cu32;
    for _ in 0..2 {
        let b = block(m);
        let slot = lh(m, cmd);
        let poly = b.wrapping_add(first).wrapping_add(slot.wrapping_mul(40));
        m.sb(poly.at(3), 9);
        m.sb(poly.at(7), 0x2c);
        m.sb(poly.at(7), if semi { 0x2e } else { 0x2c });
        let r = m.lbu(cmd.at(4));
        m.sb(poly.at(4), r);
        let g = m.lbu(cmd.at(5));
        m.sb(poly.at(5), g);
        let blue = m.lbu(cmd.at(6));
        m.sh(poly.at(0x16), tpage as u16);
        m.sh(poly.at(0xe), 0);
        m.sb(poly.at(6), blue);
        first = first.wrapping_add(0x280);
    }
    0
}

/// The move of mode 1 into slot `e` from `cmd` (opcodes 3 and 7): +2..+6
/// from the command, +8/+0xA the rectangle's centre (y - 1), +0xC = 1, +0xE
/// = 2, +0x10..+0x16 the rectangle. Returns the rectangle's height.
fn move_from_centre(m: &mut Machine, e: u32, cmd: u32) -> u32 {
    m.sh(e, 1);
    for offset in [2u32, 4, 6] {
        let v = m.lhu(cmd.at(offset));
        m.sh(e.at(offset), v);
    }
    let w = u32::from(m.lhu(cmd.at(0xc)));
    let x = u32::from(m.lhu(cmd.at(8)));
    m.sh(e.at(8), x.wrapping_add(half16(w)) as u16);
    let h = u32::from(m.lhu(cmd.at(0xe)));
    let y = u32::from(m.lhu(cmd.at(0xa)));
    m.sh(e.at(0xc), 1);
    m.sh(e.at(0xe), 2);
    m.sh(e.at(0xa), y.wrapping_add(half16(h)).wrapping_sub(1) as u16);
    let mut last = 0;
    for offset in [0x10u32, 0x12, 0x14, 0x16] {
        let v = m.lhu(cmd.at(offset - 8));
        m.sh(e.at(offset), v);
        last = u32::from(v);
    }
    last
}

/// Opcode 3, 800ED2B4: a 32-byte slot starts moving out of its centre.
pub fn op_quad_move_ed2b4(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 0x10);
    let e = slot32(m, b, cmd);
    move_from_centre(m, e, cmd)
}

/// Opcode 4, 800ED398: a 32-byte slot moves back into its centre (mode 4:
/// the current rectangle becomes the start, a 1x2 one at its centre the
/// end). Returns the end y.
pub fn op_quad_return_ed398(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 0x10);
    let e = slot32(m, b, cmd);
    m.sh(e, 4);
    let v = m.lhu(cmd.at(2));
    let x = u32::from(m.lhu(e.at(0x10)));
    let h = u32::from(m.lhu(e.at(0x16)));
    m.sh(e.at(2), v);
    let v = m.lhu(cmd.at(4));
    m.sh(e.at(4), v);
    let speed = m.lhu(cmd.at(6));
    let y = m.lhu(e.at(0x12));
    let w = u32::from(m.lhu(e.at(0x14)));
    m.sh(e.at(0xe), h as u16);
    m.sh(e.at(8), x as u16);
    m.sh(e.at(0xa), y);
    m.sh(e.at(0xc), w as u16);
    m.sh(e.at(0x14), 1);
    m.sh(e.at(0x16), 2);
    m.sh(e.at(0x10), x.wrapping_add(half16(w)) as u16);
    let y = u32::from(m.lhu(e.at(0xa)));
    m.sh(e.at(6), speed);
    let end = y.wrapping_add(half16(h)).wrapping_sub(1);
    m.sh(e.at(0x12), end as u16);
    end
}

/// Opcode 5, 800ED468 (JP; PAL 800CF014, changed): release the loaded
/// resource (800ECD20), flag +0xF38 = 0. Returns the block.
pub fn op_release_ed468(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x18);
    let _ = frame;
    release_ecd20(m);
    let b = block(m);
    m.sw(b.at(0xf38), 0);
    m.pop_frame(0x18);
    b
}

/// Opcode 6, 800ED498 (slot, blend, texture rectangle in 16/8/4-bit units,
/// clut, rgb, depth): a 160-byte slot's picture: +0x18 the frame count, the
/// texture record at +0x20 (u, v, w, h; tpage; clut; rgb) and its two
/// POLY_FT4. Depth 0/1/2 scales u and w by 4/2/1; another depth keeps the
/// scale of the caller's `a1` (the original's register). Returns 0.
pub fn op_picture_ed498(m: &mut Machine, a1: u32) -> u32 {
    let (b, cmd) = command(m, 0x18);
    let e = slot160(m, b, cmd);
    let frames = m.lhu(cmd.at(0x10));
    m.sh(e.at(0x18), frames);
    let depth = m.lbu(cmd.at(0x17));
    let record = e.at(0x20);
    let mut scale = a1;
    if depth == 0 {
        scale = 4;
    }
    if depth == 1 {
        scale = 2;
    }
    if depth == 2 {
        scale = 1;
    }
    let u = u32::from(m.lhu(cmd.at(4))) & 0x3f;
    let blend = lh(m, cmd.at(2));
    m.sh(e.at(0x20), u.wrapping_mul(scale) as u16);
    let v = m.lbu(cmd.at(6));
    m.sh(record.at(2), u16::from(v));
    let w = lh(m, cmd.at(8));
    m.sh(record.at(4), w.wrapping_mul(scale) as u16);
    let h = m.lhu(cmd.at(0xa));
    m.sh(record.at(6), h);
    let depth = u32::from(m.lbu(cmd.at(0x17)));
    let abr = u32::from(m.lhu(cmd.at(2)));
    let y = u32::from(m.lhu(cmd.at(6)));
    let x = u32::from(m.lhu(cmd.at(4)));
    let tpage = ((depth & 3) << 7)
        | ((abr & 3) << 5)
        | ((((y & 0x100) << 16) as i32 >> 20) as u32)
        | ((x & 0x3ff) >> 6)
        | ((y & 0x200) << 2);
    m.sh(record.at(8), tpage as u16);
    let clut_y = u32::from(m.lhu(cmd.at(0xe)));
    let clut_x = u32::from(m.lhu(cmd.at(0xc)));
    m.sh(
        record.at(0xa),
        ((clut_y << 6) | ((clut_x >> 4) & 0x3f)) as u16,
    );
    let r = m.lbu(cmd.at(0x14));
    let semi = blend != u32::MAX;
    m.sb(record.at(0xc), r);
    let g = m.lbu(cmd.at(0x15));
    m.sb(record.at(0xd), g);
    let blue = m.lbu(cmd.at(0x16));
    m.sb(record.at(0xe), blue);
    let mut first = 0x2cu32;
    for _ in 0..2 {
        let b = block(m);
        let slot = lh(m, cmd);
        let poly = b.wrapping_add(first).wrapping_add(slot.wrapping_mul(40));
        m.sb(poly.at(3), 9);
        m.sb(poly.at(7), 0x2c);
        m.sb(poly.at(7), if semi { 0x2e } else { 0x2c });
        for (to, from) in [(4u32, 0x14u32), (5, 0x15), (6, 0x16)] {
            let c = m.lbu(cmd.at(from));
            m.sb(poly.at(to), c);
        }
        let tpage = m.lhu(record.at(8));
        m.sh(poly.at(0x16), tpage);
        let clut = m.lhu(record.at(0xa));
        m.sh(poly.at(0xe), clut);
        let u = m.lbu(record);
        m.sb(poly.at(0xc), u);
        let v = m.lbu(record.at(2));
        m.sb(poly.at(0xd), v);
        let w = m.lbu(record.at(4));
        let u = m.lbu(record);
        m.sb(poly.at(0x14), u.wrapping_add(w.wrapping_sub(1)));
        let v = m.lbu(record.at(2));
        first = first.wrapping_add(0x280);
        m.sb(poly.at(0x15), v);
        let u = m.lbu(record);
        m.sb(poly.at(0x1c), u);
        let h = m.lbu(record.at(6));
        let v = m.lbu(record.at(2));
        m.sb(poly.at(0x1d), v.wrapping_add(h.wrapping_sub(1)));
        let w = m.lbu(record.at(4));
        let u = m.lbu(record);
        m.sb(poly.at(0x24), u.wrapping_add(w.wrapping_sub(1)));
        let h = m.lbu(record.at(6));
        let v = m.lbu(record.at(2));
        m.sb(poly.at(0x25), v.wrapping_add(h.wrapping_sub(1)));
    }
    0
}

/// Opcode 7, 800ED6E8: a 160-byte slot starts moving out of its centre.
pub fn op_picture_move_ed6e8(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 0x10);
    let e = slot160(m, b, cmd);
    move_from_centre(m, e, cmd)
}

/// Opcode 8, 800ED7D4: a 160-byte slot moves from its current rectangle
/// to the command's (mode 3). Returns the new height.
pub fn op_picture_set_ed7d4(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 0x10);
    let e = slot160(m, b, cmd);
    m.sh(e, 3);
    let v = m.lhu(cmd.at(2));
    let x = m.lhu(e.at(0x10));
    let w = m.lhu(e.at(0x14));
    let h = m.lhu(e.at(0x16));
    m.sh(e.at(2), v);
    let v = m.lhu(cmd.at(4));
    m.sh(e.at(4), v);
    let speed = m.lhu(cmd.at(6));
    let y = m.lhu(e.at(0x12));
    m.sh(e.at(8), x);
    m.sh(e.at(0xc), w);
    m.sh(e.at(0xe), h);
    m.sh(e.at(0xa), y);
    m.sh(e.at(6), speed);
    let mut last = 0;
    for offset in [0x10u32, 0x12, 0x14, 0x16] {
        let v = m.lhu(cmd.at(offset - 8));
        m.sh(e.at(offset), v);
        last = u32::from(v);
    }
    last
}

/// Opcode 9, 800ED880: a 160-byte slot moves back into its centre (mode
/// 4), as opcode 4 with its own store order. Returns the end y.
pub fn op_picture_return_ed880(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 0x10);
    let e = slot160(m, b, cmd);
    m.sh(e, 4);
    let v = m.lhu(cmd.at(2));
    let x = u32::from(m.lhu(e.at(0x10)));
    let w = u32::from(m.lhu(e.at(0x14)));
    m.sh(e.at(2), v);
    let v = m.lhu(cmd.at(4));
    m.sh(e.at(4), v);
    let speed = m.lhu(cmd.at(6));
    let y = m.lhu(e.at(0x12));
    let h = u32::from(m.lhu(e.at(0x16)));
    m.sh(e.at(8), x as u16);
    m.sh(e.at(0xc), w as u16);
    m.sh(e.at(0xa), y);
    m.sh(e.at(0x14), 1);
    m.sh(e.at(0x16), 2);
    m.sh(e.at(0xe), h as u16);
    m.sh(e.at(0x10), x.wrapping_add(half16(w)) as u16);
    let y = u32::from(m.lhu(e.at(0xa)));
    m.sh(e.at(6), speed);
    let end = y.wrapping_add(half16(h)).wrapping_sub(1);
    m.sh(e.at(0x12), end as u16);
    end
}

/// Opcode 10, 800ED958 (slot, blend, texture rectangle in 4-bit units,
/// clut, frame, rgb, depth): one frame's 16-byte texture record of a
/// 160-byte slot (+0x20 + 16 * frame). Returns the blue byte.
pub fn op_picture_frame_ed958(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 0x18);
    let e = slot160(m, b, cmd);
    let frame = lh(m, cmd.at(0x10));
    let u = u32::from(m.lhu(cmd.at(4)));
    let record = e.wrapping_add((frame << 4).wrapping_add(0x20));
    let blend = lh(m, cmd.at(2));
    m.sh(record, ((u & 0x3f) << 2) as u16);
    let v = m.lbu(cmd.at(6));
    m.sh(record.at(2), u16::from(v));
    let w = lh(m, cmd.at(8));
    m.sh(record.at(4), (w << 2) as u16);
    let h = m.lhu(cmd.at(0xa));
    let semi = u32::from(blend != u32::MAX) << 5;
    m.sh(record.at(6), h);
    let depth = u32::from(m.lbu(cmd.at(0x17)));
    let y = u32::from(m.lhu(cmd.at(6)));
    let x = u32::from(m.lhu(cmd.at(4)));
    let tpage = ((depth & 3) << 7)
        | semi
        | ((((y & 0x100) << 16) as i32 >> 20) as u32)
        | ((x & 0x3ff) >> 6)
        | ((y & 0x200) << 2);
    m.sh(record.at(8), tpage as u16);
    let clut_y = u32::from(m.lhu(cmd.at(0xe)));
    let clut_x = u32::from(m.lhu(cmd.at(0xc)));
    m.sh(
        record.at(0xa),
        ((clut_y << 6) | ((clut_x >> 4) & 0x3f)) as u16,
    );
    let r = m.lbu(cmd.at(0x14));
    m.sb(record.at(0xc), r);
    let g = m.lbu(cmd.at(0x15));
    m.sb(record.at(0xd), g);
    let blue = m.lbu(cmd.at(0x16));
    m.sb(record.at(0xe), blue);
    u32::from(blue)
}

/// Opcode 11, 800EDA60 (slot, frames): a 160-byte slot holds its current
/// rectangle (mode 5) for `frames`. Returns `frames`.
pub fn op_picture_hold_eda60(m: &mut Machine) -> u32 {
    let (b, cmd) = command(m, 4);
    let e = slot160(m, b, cmd);
    m.sh(e, 5);
    let frames = m.lhu(cmd.at(2));
    let x = m.lhu(e.at(0x10));
    let y = m.lhu(e.at(0x12));
    let w = m.lhu(e.at(0x14));
    let h = m.lhu(e.at(0x16));
    m.sh(e.at(0x1c), 0);
    m.sh(e.at(0x1e), 0);
    m.sh(e.at(0x1a), frames);
    m.sh(e.at(8), x);
    m.sh(e.at(0xa), y);
    m.sh(e.at(0xc), w);
    m.sh(e.at(0xe), h);
    u32::from(frames)
}

/// Opcode 12, 800EDAD4 (picture rect, clut rect, screen rect, depth): open
/// the stream of resource +0xF34 with one quad (800EC6B4; its count
/// argument is the word the original never stores, at frame + 0x14).
/// Returns the frame count.
pub fn op_stream_edad4(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x40);
    let (b, cmd) = command(m, 0x14);
    for (to, from) in [(0x18u32, 0u32), (0x1a, 2), (0x1c, 4), (0x1e, 6)] {
        let v = m.lhu(cmd.at(from));
        m.sh(sp.at(to), v);
    }
    for (to, from) in [
        (0x20u32, 0u32),
        (0x22, 2),
        (0x24, 4),
        (0x26, 6),
        (0x28, 0xc),
    ] {
        let v = m.lhu(cmd.at(from));
        m.sh(sp.at(to), v);
    }
    let clut_y = m.lhu(cmd.at(0xe));
    m.sh(sp.at(0x2c), 0x100);
    m.sh(sp.at(0x2e), 1);
    m.sh(sp.at(0x2a), clut_y);
    let v = m.lhu(cmd.at(0x10));
    m.sh(sp.at(0x30), v);
    let v = m.lhu(cmd.at(0x12));
    m.sh(sp.at(0x32), v);
    let v = m.lhu(cmd.at(4));
    m.sh(sp.at(0x34), v);
    let h = lh(m, cmd.at(6));
    m.sh(sp.at(0x36), (h << 1) as u16);
    let resource = m.lw(b.at(0xf34));
    m.sw(sp.at(0x10), resource);
    let v = open_ec6b4(m, sp.at(0x18), sp.at(0x20), sp.at(0x28), sp.at(0x30));
    m.pop_frame(0x40);
    v
}

/// 800EDBCC: run the picture script up to the clock (+0x10): each command
/// (+0 opcode, +2 time, then its operands) whose time has come, through
/// the opcode table; opcode 0 ends the run. Returns 1, or the last
/// opcode's result when it was 0.
pub fn run_script_edbcc(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x20);
    let _ = frame;
    let b = block(m);
    let cmd = m.lw(b.at(0x14));
    let clock = m.lw(b.at(0x10));
    let mut time = lh(m, cmd.at(2));
    let mut opcode = u32::from(m.lhu(cmd));
    let mut v = 1;
    if !lt(clock, time) {
        loop {
            let b = block(m);
            let op = sext16(opcode);
            let cmd = m.lw(b.at(0x14));
            m.sw(b.at(0x14), cmd.at(4));
            let handler = m.lw(OPCODES.wrapping_add(op << 2));
            // a1 holds 0x80110000 for the first command (the original's
            // `lui a1, 0x8011`); only opcode 6's invalid depths read it
            v = call(m, handler, [time, 0x8011_0000, 0, 0]);
            if op == 0 {
                break;
            }
            let b = block(m);
            let cmd = m.lw(b.at(0x14));
            let clock = m.lw(b.at(0x10));
            time = lh(m, cmd.at(2));
            opcode = u32::from(m.lhu(cmd));
            if lt(clock, time) {
                v = 1;
                break;
            }
        }
    }
    m.pop_frame(0x20);
    v
}

/// 800EDC80: the index of halfword `value` among `count` halfwords at
/// `list`, or 0.
pub fn find_edc80(m: &mut Machine, list: u32, value: u32, count: u32) -> u32 {
    if !lt(0, count) {
        return 0;
    }
    let value = sext16(value);
    let (mut at, mut i) = (list, 0u32);
    loop {
        if lh(m, at) == value {
            return i;
        }
        i = i.wrapping_add(1);
        at = at.at(2);
        if !lt(i, count) {
            return 0;
        }
    }
}

/// 800EDCC0: open the broadcast picture stream of kind `kind` (0..9: the
/// picture size per kind at VRAM (0x380, 0x100), clut (0x300, 1); other
/// kinds keep the frame's stale size words) with the block's two quads
/// (+0x1674 / +0x1654, count +0x1650). The frame count goes to +0xF3C.
pub fn open_broadcast_edcc0(m: &mut Machine, kind: u32) -> u32 {
    let sp = m.push_frame(0x48);
    let rect = sp.at(0x18);
    m.sh(rect, 0x380);
    m.sh(rect.at(2), 0x100);
    m.sh(rect.at(0xc), 0x300);
    m.sh(rect.at(0xe), 1);
    let size = match kind {
        0 | 1 | 6 => Some((0x50, 0x50)),
        2 => Some((0xa0, 0x50)),
        3 => Some((0x90, 0x40)),
        4 | 9 => Some((0x80, 0x60)),
        5 => Some((0xa0, 0x40)),
        _ => None,
    };
    if let Some((w, h)) = size {
        m.sh(rect.at(4), w);
        m.sh(rect.at(6), h);
    }
    let x = m.lhu(rect);
    let b = block(m);
    m.sh(sp.at(0x30), x);
    for (to, from) in [(0x32u32, 2u32), (0x34, 4), (0x36, 6), (0x38, 0xc)] {
        let v = m.lhu(rect.at(from));
        m.sh(sp.at(to), v);
    }
    let clut_y = m.lhu(rect.at(0xe));
    m.sh(sp.at(0x3c), 0x100);
    m.sh(sp.at(0x3e), 1);
    m.sh(sp.at(0x3a), clut_y);
    let resource = m.lw(b.at(0xf34));
    m.sw(sp.at(0x10), resource);
    let count = m.lw(b.at(0x1650));
    m.sw(sp.at(0x14), count);
    let frames = open_ec6b4(m, sp.at(0x30), b.at(0x1674), sp.at(0x38), b.at(0x1654));
    let b = block(m);
    m.sw(b.at(0xf3c), frames);
    m.pop_frame(0x48);
    frames
}

/// 800EDDF4: the selection list: cursor -1, state 0, styles 1/2 and entries
/// 3/4 cleared; the list of the event record `[+0x189C]` (800EDC80 over the
/// six ids at 0x8010B37C); each buffer's DR_MODE (tpage 0) and 11 sprites
/// (0x87x0x12 at (0x70, 0x58 + 0x18 n), v = 0x12 n, grey); sound 0x3E.
/// Returns the sound request's result.
pub fn list_init_eddf4(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x38);
    let b = block(m);
    m.sh(b.at(0x16b2), 0xffff);
    m.sb(b.at(0x16b1), 0);
    call(m, STYLE_BYTE, [1, 0, 0, 0]);
    call(m, STYLE_BYTE, [2, 0, 0, 0]);
    call(m, STYLE_ENTRY, [3, 0, 0, 0]);
    call(m, STYLE_ENTRY, [4, 0, 0, 0]);
    let b = block(m);
    let event = m.lw(b.at(0x189c));
    let id = lh(m, event);
    let key = lh(m, EVENTS.wrapping_add(id << 3));
    let list = find_edc80(m, LIST_EVENTS, key, 6);
    let b = block(m);
    m.sb(b.at(0x16b0), list as u8);
    let (mut mode, mut sprites) = (0x186cu32, 0x16b4u32);
    for _ in 0..2 {
        let b = block(m);
        m.sw(sp.at(0x10), 0);
        call(m, SET_DRAW_MODE, [b.wrapping_add(mode), 0, 0, 0]);
        let mut s = b.wrapping_add(sprites);
        let (mut y, mut v) = (0x58u32, 0u32);
        for _ in 0..11 {
            m.sb(s.at(3), 4);
            m.sb(s.at(7), 0x64);
            m.sb(s.at(4), 0x80);
            m.sb(s.at(5), 0x80);
            m.sb(s.at(6), 0x80);
            m.sh(s.at(8), 0x70);
            m.sh(s.at(0xa), y as u16);
            m.sh(s.at(0x10), 0x87);
            m.sh(s.at(0x12), 0x12);
            m.sb(s.at(0xc), 0);
            m.sb(s.at(0xd), v as u8);
            s = s.at(0x14);
            v = v.wrapping_add(0x12);
            y = y.wrapping_add(0x18);
        }
        mode = mode.wrapping_add(0x18);
        sprites = sprites.wrapping_add(0xdc);
    }
    let v = call(m, PLAY_SOUND, [0x3e, 0, 0, 0]);
    m.pop_frame(0x38);
    v
}

/// 800EDF84: the download progress bar: a 2-pixel wide shape from (0x6B,
/// 0x97) of height 106 * progress (+0xF40) / frames (+0xF3C), colour
/// (0xE5, 0xE5, 0xD5), drawn into the hub's picture (800F7244); the result
/// becomes the hub's picture (+0x274).
pub fn progress_edf84(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x38);
    let shape = sp.at(0x10);
    m.sb(shape.at(0x13), 0);
    let b = block(m);
    m.sh(shape, 0x6b);
    m.sh(shape.at(2), 0x97);
    m.sh(shape.at(4), 0x6d);
    m.sh(shape.at(6), 0x97);
    m.sh(shape.at(8), 0x6b);
    let progress = m.lw(b.at(0xf40));
    let total = m.lw(b.at(0xf3c));
    let (q, _) = div(
        m,
        progress.wrapping_mul(106),
        total,
        0x800e_dfec,
        0x800e_e004,
    );
    m.sh(shape.at(0xc), 0x6d);
    m.sh(shape.at(0xa), q.wrapping_add(0x97) as u16);
    let progress = m.lw(b.at(0xf40));
    let total = m.lw(b.at(0xf3c));
    let (q, _) = div(
        m,
        progress.wrapping_mul(106),
        total,
        0x800e_e044,
        0x800e_e05c,
    );
    for offset in [0x12u32, 0x16, 0x1a, 0x1e] {
        m.sb(shape.at(offset), 0xd5);
    }
    for offset in [0x10u32, 0x11, 0x14, 0x15, 0x18, 0x19, 0x1c, 0x1d] {
        m.sb(shape.at(offset), 0xe5);
    }
    let hub = m.lw(HUB);
    m.sh(shape.at(0xe), q.wrapping_add(0x97) as u16);
    let picture = m.lw(hub.at(0x274));
    let source = m.lw(hub.at(8));
    let v = call(m, DRAW_SHAPE, [shape, picture, source, 0x6d]);
    let hub = m.lw(HUB);
    m.sw(hub.at(0x274), v);
    m.pop_frame(0x38);
    v
}

/// `AddPrim(table, packet)`: the packet's next pointer from the table
/// entry, the entry's from the packet (24 bits each), in the original's
/// load/store order.
fn add_prim(m: &mut Machine, table: u32, packet: u32) {
    let tag = m.lw(packet);
    let head = m.lw(table);
    m.sw(packet, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let head = m.lw(table);
    m.sw(table, (head & 0xff00_0000) | (packet & 0x00ff_ffff));
}

/// The style byte of list `list`'s entry `cursor`, column `column`
/// (`LISTS + 20 * list + 4 + 2 * cursor + column`).
fn list_style(m: &mut Machine, column: u32) -> u32 {
    let b = block(m);
    let cursor = lh(m, b.at(0x16b2));
    let list = u32::from(m.lbu(b.at(0x16b0)));
    let at = (cursor << 1)
        .wrapping_add(list.wrapping_mul(20).wrapping_add(column))
        .wrapping_add(LISTS);
    u32::from(m.lbu(at.at(4)))
}

/// 800EE0D8: draw the selection list into the hub's table (`[[HUB] + 8]`):
/// the frame's DR_MODE (tpage 0), unless the event kind is 8 the 11
/// sprites (clut row 0x128 + cursor % 4) and a DR_MODE with the cursor's
/// tpage; then step the cursor while style 5 is idle (to the list's end:
/// hub +0x128 = 1), the entry's style bytes into styles 1 and 2 (starting
/// entries 3 / 4 and setting the state 1 / 2 when idle), the state into
/// style 3; with pad bit 0x10 the entries 3..5 are reset.
pub fn list_tick_ee0d8(m: &mut Machine) -> u32 {
    list_tick(m, true, 2)
}

/// 800EE570: as 800EE0D8 for every event kind, the cursor running to the
/// list's last entry.
pub fn list_tick_all_ee570(m: &mut Machine) -> u32 {
    list_tick(m, false, 1)
}

/// The list tick: `by_kind` skips the sprites for event kind 8; the cursor
/// stops `margin` entries before the list's count.
fn list_tick(m: &mut Machine, by_kind: bool, margin: u32) -> u32 {
    let sp = m.push_frame(0x28);
    let context = m.gw(CONTEXT);
    let index = m.lw(context.at(4));
    let b = block(m);
    let mut sprite = b.wrapping_add(index.wrapping_mul(0xdc).wrapping_add(0x16b4));
    let mode = b.wrapping_add(index.wrapping_mul(0x18).wrapping_add(0x186c));
    let hub = m.lw(HUB);
    let table = m.lw(hub.at(8));
    add_prim(m, table, mode);
    let kind = if by_kind {
        let event = m.lw(b.at(0x189c));
        let id = lh(m, event);
        m.lbu(EVENTS.wrapping_add(id << 3).at(6))
    } else {
        0
    };
    let mode = mode.at(0xc);
    if kind != 8 {
        for _ in 0..11 {
            let cursor = lh(m, b.at(0x16b2));
            let row = (cursor as i32 % 4) as u32;
            let clut = (row.wrapping_add(0x128) << 6) | 0x16;
            let tag = m.lw(sprite);
            m.sh(sprite.at(0xe), clut as u16);
            let head = m.lw(table);
            m.sw(sprite, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
            let head = m.lw(table);
            m.sw(table, (head & 0xff00_0000) | (sprite & 0x00ff_ffff));
            sprite = sprite.at(0x14);
        }
        let b = block(m);
        let cursor = u32::from(m.lhu(b.at(0x16b2)));
        m.sw(sp.at(0x10), 0);
        let page = half16(cursor);
        let parity = sext16(page) as i32 % 2;
        let x = ((parity << 6) + 0x240) as u32 & 0x3ff;
        call(m, SET_DRAW_MODE, [mode, 0, 0, sra(x, 6) | 0x10]);
        add_prim(m, table, mode);
    }
    if call(m, STYLE_HALF, [5, 0, 0, 0]) == 0 {
        let b = block(m);
        let list = u32::from(m.lbu(b.at(0x16b0)));
        let count = m.lw(LISTS.wrapping_add(list.wrapping_mul(20)));
        let cursor = lh(m, b.at(0x16b2));
        if lt(cursor, count.wrapping_sub(margin)) {
            m.sb(b.at(0x16b1), 0);
            let b = block(m);
            let cursor = m.lhu(b.at(0x16b2)).wrapping_add(1);
            m.sh(b.at(0x16b2), cursor);
            call(m, STYLE_ENTRY, [5, 1, 0, 0xa]);
        } else {
            let hub = m.lw(HUB);
            m.sw(hub.at(0x128), 1);
        }
    }
    for (column, style, entry, state) in [(0u32, 1u32, 3u32, 1u8), (1, 2, 4, 2)] {
        let byte = list_style(m, column);
        call(m, STYLE_BYTE, [style, byte, 0, 0]);
        if list_style(m, column) != 0 && call(m, STYLE_HALF, [entry, 0, 0, 0]) == 0 {
            let hub = m.lw(HUB);
            if m.lw(hub.at(0x128)) == 0 {
                call(m, STYLE_ENTRY, [entry, 1, 0, 0xa]);
            }
            let b = block(m);
            m.sb(b.at(0x16b1), state);
        }
    }
    let b = block(m);
    let state = u32::from(m.lbu(b.at(0x16b1)));
    call(m, STYLE_BYTE, [3, state, 0, 0]);
    let context = m.gw(CONTEXT);
    let pressed = m.lw(context.at(0x12c)) & 0x10;
    let v = if pressed != 0 {
        if call(m, STYLE_HALF, [3, 0, 0, 0]) == 2 {
            call(m, STYLE_ENTRY, [3, 3, u32::MAX, u32::MAX]);
        }
        if call(m, STYLE_HALF, [4, 0, 0, 0]) == 2 {
            call(m, STYLE_ENTRY, [4, 3, u32::MAX, u32::MAX]);
        }
        call(m, STYLE_ENTRY, [5, 3, u32::MAX, u32::MAX])
    } else {
        pressed
    };
    m.pop_frame(0x28);
    v
}

/// 800E1040 (x, y, w, h, 0 on the stack) and 800E0F88 (x0, y0, x1, y1):
/// the hub's line primitives (other parts); libgte rcos, rsin.
const BAR: u32 = 0x800e_1040;
const LINE: u32 = 0x800e_0f88;
const RCOS: u32 = 0x800a_61c8;
const RSIN: u32 = 0x800a_60f8;

fn bar(m: &mut Machine, frame: u32, x: u32, y: u32, w: u32, h: u32) {
    m.sw(frame.at(0x10), 0);
    call(m, BAR, [x, y, w, h]);
}

fn line(m: &mut Machine, x0: u32, y0: u32, x1: u32, y1: u32) {
    call(m, LINE, [x0, y0, x1, y1]);
}

/// Whether the hub's step (+0x86) is one of 5..13 (the frame drawn whole).
fn settled(m: &mut Machine) -> bool {
    let hub = m.lw(HUB);
    u32::from(m.lhu(hub.at(0x86))).wrapping_sub(5) < 9
}

fn clock(m: &mut Machine) -> u32 {
    let b = block(m);
    m.lw(b.at(0x10))
}

/// 800EE9E4: the hub's wireframe. For hub steps 3..14 (and negative ones)
/// the frame lines grow with the script clock (+0x10) until it settles
/// (steps 5..13 draw them whole): the centre cross (clock < 23); with
/// +0x28 = 1 the centre box (clock < 11) widening to the outer box (clock
/// < 23); with +0x28 = 0 the side bars and the arc of up to 16 segments
/// (rcos/rsin). Returns 0 before step 3 and at step 15, the block's +0x28
/// when it is not 0 after the boxes, else 1.
pub fn wireframe_ee9e4(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x70);
    let v = wireframe(m, frame);
    m.pop_frame(0x70);
    v
}

fn wireframe(m: &mut Machine, frame: u32) -> u32 {
    let hub = m.lw(HUB);
    let step = lh(m, hub.at(0x86));
    if !lt(step, 0) && (lt(step, 3) || step == 15) {
        return 0;
    }
    const K23: u32 = 0xb216_42c9;
    let c = if settled(m) { None } else { Some(clock(m)) };
    match c {
        Some(c) if lt(c, 0x17) => {
            let d = magic_div(c.wrapping_mul(160), K23, true, 4);
            bar(m, frame, 0xa0u32.wrapping_sub(d), 0xf0, d, 2);
            let d = magic_div(clock(m).wrapping_mul(160), K23, true, 4);
            bar(m, frame, 0xa0, 0xf0, d, 2);
            let e = magic_div(clock(m).wrapping_mul(240), K23, true, 4);
            line(m, 0xa0, 0xf0u32.wrapping_sub(e), 0xa0, 0xf0);
            let e = magic_div(clock(m).wrapping_mul(240), K23, true, 4);
            line(m, 0xa0, 0xf0, 0xa0, e.wrapping_add(0xf0));
        }
        _ => {
            bar(m, frame, 0, 0xf0, 0xa0, 2);
            bar(m, frame, 0xa0, 0xf0, 0xa0, 2);
            line(m, 0xa0, 0, 0xa0, 0xf0);
            line(m, 0xa0, 0xf0, 0xa0, 0x1e0);
        }
    }
    let b = block(m);
    if m.lw(b.at(0x28)) == 1 {
        boxes(m, frame, b);
    }
    let b = block(m);
    let flag = m.lw(b.at(0x28));
    if flag != 0 {
        return flag;
    }
    side_bars(m, frame, b);
    arc(m)
}

/// The boxes of +0x28 = 1 (800EEC18..800EF07C).
fn boxes(m: &mut Machine, frame: u32, b: u32) {
    let c = if settled(m) {
        None
    } else {
        Some(m.lw(b.at(0x10)))
    };
    match c {
        Some(c) if lt(c, 0xb) => {
            let half = sext16(magic_div(c.wrapping_mul(120), 0x2e8b_a2e9, false, 1));
            let rise = magic_div(c.wrapping_mul(160), 0x2e8b_a2e9, false, 1);
            let left = 0x8cu32.wrapping_sub(half);
            bar(m, frame, left, 0xdc, half, 2);
            bar(m, frame, 0xb4, 0xdc, half, 2);
            bar(m, frame, left, 0x104, half, 2);
            bar(m, frame, 0xb4, 0x104, half, 2);
            let rise = sext16(rise);
            let top = 0xdcu32.wrapping_sub(rise);
            line(m, 0x8c, top, 0x8c, 0xdc);
            line(m, 0xb4, top, 0xb4, 0xdc);
            let bottom = rise.wrapping_add(0x104);
            line(m, 0x8c, 0x104, 0x8c, bottom);
            line(m, 0xb4, 0x104, 0xb4, bottom);
        }
        Some(c) if lt(c, 0x17) => {
            let grow = c.wrapping_sub(0xb).wrapping_mul(5);
            let rise = magic_div(grow << 5, 0x2aaa_aaab, false, 1);
            bar(m, frame, 0x14, 0xdc, 0x78, 2);
            bar(m, frame, 0xb4, 0xdc, 0x78, 2);
            bar(m, frame, 0x14, 0x104, 0x78, 2);
            bar(m, frame, 0xb4, 0x104, 0x78, 2);
            line(m, 0x8c, 0x3c, 0x8c, 0xdc);
            line(m, 0xb4, 0x3c, 0xb4, 0xdc);
            line(m, 0x8c, 0x104, 0x8c, 0x1a4);
            line(m, 0xb4, 0x104, 0xb4, 0x1a4);
            let width = sext16(grow << 1);
            let left = 0x8cu32.wrapping_sub(width);
            bar(m, frame, left, 0x3c, width, 2);
            bar(m, frame, 0xb4, 0x3c, width, 2);
            bar(m, frame, left, 0x1a4, width, 2);
            bar(m, frame, 0xb4, 0x1a4, width, 2);
            let rise = sext16(rise);
            let top = 0xdcu32.wrapping_sub(rise);
            line(m, 0x14, top, 0x14, 0xdc);
            line(m, 0x12c, top, 0x12c, 0xdc);
            let bottom = rise.wrapping_add(0x104);
            line(m, 0x14, 0x104, 0x14, bottom);
            line(m, 0x12c, 0x104, 0x12c, bottom);
        }
        _ => {
            for (x, y) in [(0x14, 0xdc), (0xb4, 0xdc), (0x14, 0x104), (0xb4, 0x104)] {
                bar(m, frame, x, y, 0x78, 2);
            }
            line(m, 0x8c, 0x3c, 0x8c, 0xdc);
            line(m, 0xb4, 0x3c, 0xb4, 0xdc);
            line(m, 0x8c, 0x104, 0x8c, 0x1a4);
            line(m, 0xb4, 0x104, 0xb4, 0x1a4);
            for (x, y) in [(0x14, 0x3c), (0xb4, 0x3c), (0x14, 0x1a4), (0xb4, 0x1a4)] {
                bar(m, frame, x, y, 0x78, 2);
            }
            line(m, 0x14, 0x3c, 0x14, 0xdc);
            line(m, 0x12c, 0x3c, 0x12c, 0xdc);
            line(m, 0x14, 0x104, 0x14, 0x1a4);
            line(m, 0x12c, 0x104, 0x12c, 0x1a4);
        }
    }
}

/// The side bars of +0x28 = 0 (800EF09C..800EF298).
fn side_bars(m: &mut Machine, frame: u32, b: u32) {
    const K7: u32 = 0x9249_2493;
    let c = if settled(m) {
        None
    } else {
        Some(m.lw(b.at(0x10)))
    };
    match c {
        Some(c) if lt(c, 7) => {
            let d = magic_div(c.wrapping_mul(80), K7, true, 2);
            bar(m, frame, 0xa0u32.wrapping_sub(d), 0x122, d, 2);
            let d = magic_div(clock(m).wrapping_mul(80), K7, true, 2);
            bar(m, frame, 0xa0, 0x122, d, 2);
        }
        _ => {
            bar(m, frame, 0x50, 0x122, 0x50, 2);
            bar(m, frame, 0xa0, 0x122, 0x50, 2);
        }
    }
    if settled(m) {
        bar(m, frame, 0x50, 0x49, 0x50, 2);
        bar(m, frame, 0xa0, 0x49, 0x50, 2);
        return;
    }
    let c = clock(m);
    if lt(c, 0x17) {
        return;
    }
    if lt(c, 0x1e) {
        let d = magic_div(c.wrapping_sub(0x17).wrapping_mul(80), K7, true, 2);
        bar(m, frame, 0x50, 0x49, d, 2);
        let d = magic_div(clock(m).wrapping_sub(0x17).wrapping_mul(80), K7, true, 2);
        bar(m, frame, 0xf0u32.wrapping_sub(d), 0x49, d, 2);
    } else {
        bar(m, frame, 0x50, 0x49, 0x50, 2);
        bar(m, frame, 0xa0, 0x49, 0x50, 2);
    }
}

/// `x * scale` in 4.12 fixed point, rounded toward zero (`(x + 0xFFF) >>
/// 12` for a negative product).
fn fixed(x: u32, scale: u32) -> u32 {
    let p = x.wrapping_mul(scale);
    sra(if lt(p, 0) { p.wrapping_add(0xfff) } else { p }, 12)
}

/// The arc (800EF2A0..800EF4C0): up to 16 segments of two mirrored line
/// pairs, angle 0xC00 - 0x80 n, radii 54 / 108 / 109 (rcos/rsin in 4.12).
/// The original keeps the segment ends in halfwords of its frame: they are
/// 16-bit values here too.
fn arc(m: &mut Machine) -> u32 {
    let count = if settled(m) {
        0x10
    } else {
        let c = clock(m);
        if lt(c, 7) {
            0
        } else if lt(c, 0x17) {
            c.wrapping_sub(7)
        } else {
            0x10
        }
    };
    let (mut x28, mut x38, mut y40) = (0x50u16, 0x50u16, 0x123u16);
    let (mut x20, mut x30) = (0xf0u16, 0xf0u16);
    let mut s3 = 0x122u32;
    if !lt(0, count) {
        return 1;
    }
    let mut i = 1u32;
    loop {
        let angle = 0xc00u32.wrapping_sub(i << 7);
        let cos = call(m, RCOS, [angle, 0, 0, 0]);
        let x = fixed(cos, 54);
        let sin = call(m, RSIN, [angle, 0, 0, 0]);
        let y = fixed(sin, 108);
        let y0 = sext16(s3);
        let dx = sext16(x);
        let right = dx.wrapping_add(0x50);
        let ybase = 0xb6u32.wrapping_sub(sext16(y));
        line(m, sext16(u32::from(x28)), y0, right, ybase);
        let left = 0xf0u32.wrapping_sub(dx);
        line(m, sext16(u32::from(x20)), y0, left, ybase);
        let near = x.wrapping_add(0x50);
        let far = 0xf0u32.wrapping_sub(x);
        s3 = 0xb6u32.wrapping_sub(y);
        x28 = near as u16;
        x20 = far as u16;
        let sin = call(m, RSIN, [angle, 0, 0, 0]);
        let z = fixed(sin, 109);
        let zbase = 0xb6u32.wrapping_sub(sext16(z));
        let y1 = sext16(u32::from(y40));
        line(m, sext16(u32::from(x38)), y1, right, zbase);
        line(m, sext16(u32::from(x30)), y1, left, zbase);
        x38 = near as u16;
        x30 = far as u16;
        y40 = 0xb6u32.wrapping_sub(z) as u16;
        i = i.wrapping_add(1);
        if lt(count, i) {
            return 1;
        }
    }
}

/// 800ECC70: one frame of the broadcast stream (this part).
const STREAM_TICK: u32 = 0x800e_cc70;

/// `(to - from) * t / span` of two halfword fields of slot base `e` (the
/// original's `mult` and checked `div`, traps at `zero` / `zero + 0x18`).
fn lerp(m: &mut Machine, e: u32, to: u32, from: u32, t: u32, span: u32, zero: u32) -> u32 {
    let delta = lh(m, e.at(to)).wrapping_sub(lh(m, e.at(from)));
    div(m, delta.wrapping_mul(t), span, zero, zero + 0x18).0
}

/// The quad `q` over the rectangle at `e + x..` (x, y, w, h halfwords at
/// +0, +2, +4, +6), in the original's store order.
fn quad_rect(m: &mut Machine, q: u32, r: u32) {
    let x = m.lhu(r);
    m.sh(q.at(8), x);
    let y = m.lhu(r.at(2));
    m.sh(q.at(0xa), y);
    let (x, w) = (m.lhu(r), m.lhu(r.at(4)));
    m.sh(q.at(0x10), x.wrapping_add(w));
    let y = m.lhu(r.at(2));
    m.sh(q.at(0x12), y);
    let x = m.lhu(r);
    m.sh(q.at(0x18), x);
    let (y, h) = (m.lhu(r.at(2)), m.lhu(r.at(6)));
    m.sh(q.at(0x1a), y.wrapping_add(h));
    let (x, w) = (m.lhu(r), m.lhu(r.at(4)));
    m.sh(q.at(0x20), x.wrapping_add(w));
    let (y, h) = (m.lhu(r.at(2)), m.lhu(r.at(6)));
    m.sh(q.at(0x22), y.wrapping_add(h));
}

/// The quad's corners from `x, y, w, h` (800EFD04 / 800EF73C order: x
/// first when `x_first`).
fn quad_corners(m: &mut Machine, q: u32, x: u32, y: u32, w: u32, h: u32, x_first: bool) {
    if x_first {
        m.sh(q.at(8), x as u16);
        m.sh(q.at(0x18), x as u16);
        m.sh(q.at(0x10), x.wrapping_add(w) as u16);
        m.sh(q.at(0x20), x.wrapping_add(w) as u16);
        m.sh(q.at(0xa), y as u16);
        m.sh(q.at(0x12), y as u16);
        m.sh(q.at(0x1a), y.wrapping_add(h) as u16);
        m.sh(q.at(0x22), y.wrapping_add(h) as u16);
    } else {
        m.sh(q.at(0xa), y as u16);
        m.sh(q.at(0x12), y as u16);
        m.sh(q.at(0x1a), y.wrapping_add(h) as u16);
        m.sh(q.at(0x22), y.wrapping_add(h) as u16);
        m.sh(q.at(8), x as u16);
        m.sh(q.at(0x18), x as u16);
        m.sh(q.at(0x10), x.wrapping_add(w) as u16);
        m.sh(q.at(0x20), x.wrapping_add(w) as u16);
    }
}

/// 800EF4F4: animate and link the 16 picture slots (+0x52C + 0xA0 n) of
/// the frame's buffer into the hub's table (`[[HUB] + 8]`): mode 1 opens
/// from the centre line (widen until +4, then grow in height until +6,
/// then mode 2), mode 2 shows the target rectangle, mode 3 moves from the
/// start rectangle to the target (then mode 2), mode 4 closes (shrink in
/// height until +4, narrow until +6, then mode 0), mode 5 shows the
/// current rectangle with the frame-animated texture records. With flag
/// +0xF38 = 1 the broadcast stream runs (800ECC70 into table entry +8,
/// frame into +0xF40) and, for event kind 0, the progress bar. Returns the
/// stream's result for flag 1, 2 for flag 2, and for other flags the
/// original's caller-saved `s1` (unknown here: 0).
pub fn slots_ef4f4(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x20);
    let _ = frame;
    let hub = m.lw(HUB);
    let table = m.lw(hub.at(8));
    for i in 0..16u32 {
        let context = m.gw(CONTEXT);
        let index = m.lw(context.at(4));
        let b = block(m);
        let q = b
            .wrapping_add(index.wrapping_mul(0x280).wrapping_add(0x2c))
            .wrapping_add(i * 40);
        let e = b.wrapping_add(i * 0xa0);
        let mode = lh(m, e.at(0x52c));
        if mode == 0 || mode >= 6 {
            continue;
        }
        slot_tick(m, mode, i, q, table);
    }
    let b = block(m);
    let flag = m.lw(b.at(0xf38));
    let v = if flag == 1 {
        let hub = m.lw(HUB);
        let entry = m.lw(hub.at(8));
        let ended = call(m, STREAM_TICK, [entry.at(8), b.at(0xf40), 0, 0]);
        let b = block(m);
        let event = m.lw(b.at(0x189c));
        let id = lh(m, event);
        if m.lbu(EVENTS.wrapping_add(id << 3).at(6)) == 0 {
            progress_edf84(m);
        }
        ended
    } else if flag == 2 {
        2
    } else {
        0
    };
    m.pop_frame(0x20);
    v
}

fn slot_tick(m: &mut Machine, mode: u32, i: u32, q: u32, table: u32) {
    let b = block(m);
    let e = b.wrapping_add(i * 0xa0);
    let s = e.at(0x52c);
    match mode {
        1 => {
            let start = lh(m, s.at(2));
            let clock = m.lw(b.at(0x10));
            if lt(clock, start) {
                return;
            }
            let mid = lh(m, s.at(4));
            if lt(clock, mid) {
                let t = clock.wrapping_sub(start);
                let span = mid.wrapping_sub(start);
                let dx = lerp(m, s, 0x10, 8, t, span, 0x800e_f600);
                let dw = lerp(m, s, 0x14, 0xc, t, span, 0x800e_f64c);
                let x = u32::from(m.lhu(s.at(8)));
                let y = u32::from(m.lhu(s.at(0xa)));
                let w = u32::from(m.lhu(s.at(0xc)));
                line_quad(m, q, x.wrapping_add(dx), y, w.wrapping_add(dw));
            } else {
                let end = lh(m, s.at(6));
                let t = clock.wrapping_sub(mid);
                if lt(clock, end) {
                    let span = end.wrapping_sub(mid);
                    let dy = lerp(m, s, 0x12, 0xa, t, span, 0x800e_f6c0);
                    let dh = lerp(m, s, 0x16, 0xe, t, span, 0x800e_f70c);
                    let x = u32::from(m.lhu(s.at(0x10)));
                    let h = u32::from(m.lhu(s.at(0xe)));
                    let w = u32::from(m.lhu(s.at(0x14)));
                    let y = u32::from(m.lhu(s.at(0xa)));
                    quad_corners(m, q, x, y.wrapping_add(dy), w, h.wrapping_add(dh), true);
                } else {
                    quad_rect(m, q, s.at(0x10));
                    add_prim(m, table, q);
                    m.sh(s, 2);
                    return;
                }
            }
            add_prim(m, table, q);
        }
        2 => {
            let b = block(m);
            quad_rect(m, q, b.wrapping_add(i * 0xa0).at(0x52c + 0x10));
            add_prim(m, table, q);
        }
        3 => {
            let start = lh(m, s.at(2));
            let clock = m.lw(b.at(0x10));
            if lt(clock, start) {
                return;
            }
            let end = lh(m, s.at(6));
            let t = clock.wrapping_sub(start);
            if !lt(clock, end) {
                quad_rect(m, q, s.at(0x10));
                add_prim(m, table, q);
                m.sh(s, 2);
                return;
            }
            let span = end.wrapping_sub(start);
            let dx = lerp(m, s, 0x10, 8, t, span, 0x800e_f924);
            let dy = lerp(m, s, 0x12, 0xa, t, span, 0x800e_f970);
            let dw = lerp(m, s, 0x14, 0xc, t, span, 0x800e_f9bc);
            let dh = lerp(m, s, 0x16, 0xe, t, span, 0x800e_fa08);
            let x = u32::from(m.lhu(s.at(8)));
            let y = u32::from(m.lhu(s.at(0xa)));
            let w = u32::from(m.lhu(s.at(0xc)));
            let h = u32::from(m.lhu(s.at(0xe)));
            quad_corners(
                m,
                q,
                x.wrapping_add(dx),
                y.wrapping_add(dy),
                w.wrapping_add(dw),
                h.wrapping_add(dh),
                true,
            );
            add_prim(m, table, q);
        }
        4 => {
            let start = lh(m, s.at(2));
            let clock = m.lw(b.at(0x10));
            if lt(clock, start) {
                return;
            }
            let mid = lh(m, s.at(4));
            if lt(clock, mid) {
                let t = clock.wrapping_sub(start);
                let span = mid.wrapping_sub(start);
                let dy = lerp(m, s, 0x12, 0xa, t, span, 0x800e_fb94);
                let dh = lerp(m, s, 0x16, 0xe, t, span, 0x800e_fbe0);
                let x = u32::from(m.lhu(s.at(8)));
                let h = u32::from(m.lhu(s.at(0xe)));
                let w = u32::from(m.lhu(s.at(0xc)));
                let y = u32::from(m.lhu(s.at(0xa)));
                quad_corners(m, q, x, y.wrapping_add(dy), w, h.wrapping_add(dh), true);
            } else {
                let end = lh(m, s.at(6));
                let t = clock.wrapping_sub(mid);
                if !lt(clock, end) {
                    quad_rect(m, q, s.at(0x10));
                    add_prim(m, table, q);
                    m.sh(s, 0);
                    return;
                }
                let span = end.wrapping_sub(mid);
                let dx = lerp(m, s, 0x10, 8, t, span, 0x800e_fc88);
                let dw = lerp(m, s, 0x14, 0xc, t, span, 0x800e_fcd4);
                let x = u32::from(m.lhu(s.at(8)));
                let y = u32::from(m.lhu(s.at(0x12)));
                let w = u32::from(m.lhu(s.at(0xc)));
                line_quad(m, q, x.wrapping_add(dx), y, w.wrapping_add(dw));
            }
            add_prim(m, table, q);
        }
        _ => {
            let counter = lh(m, s.at(0x1e));
            let period = lh(m, s.at(0x1a));
            let next = u32::from(m.lhu(s.at(0x1e))).wrapping_add(1);
            if lt(counter, period) {
                m.sh(s.at(0x1e), next as u16);
            } else {
                let frame = m.lhu(s.at(0x1c)).wrapping_add(1);
                let count = lh(m, s.at(0x18));
                m.sh(s.at(0x1c), frame);
                let frame = lh(m, s.at(0x1c));
                m.sh(s.at(0x1e), 0);
                if !lt(frame, count) {
                    m.sh(s.at(0x1c), 0);
                }
            }
            let b = block(m);
            let s = b.wrapping_add(i * 0xa0).at(0x52c);
            let frame = lh(m, s.at(0x1c));
            let record = s.wrapping_add((frame << 4).wrapping_add(0x20));
            let tpage = m.lhu(record.at(8));
            m.sh(q.at(0x16), tpage);
            let clut = m.lhu(record.at(0xa));
            m.sh(q.at(0xe), clut);
            quad_rect(m, q, s.at(8));
            let u = m.lbu(record);
            m.sb(q.at(0xc), u);
            let v = m.lbu(record.at(2));
            m.sb(q.at(0xd), v);
            let (w, u) = (m.lbu(record.at(4)), m.lbu(record));
            m.sb(q.at(0x14), u.wrapping_add(w.wrapping_sub(1)));
            let v = m.lbu(record.at(2));
            m.sb(q.at(0x15), v);
            let u = m.lbu(record);
            m.sb(q.at(0x1c), u);
            let (h, v) = (m.lbu(record.at(6)), m.lbu(record.at(2)));
            m.sb(q.at(0x1d), v.wrapping_add(h.wrapping_sub(1)));
            let (w, u) = (m.lbu(record.at(4)), m.lbu(record));
            m.sb(q.at(0x24), u.wrapping_add(w.wrapping_sub(1)));
            let (h, v) = (m.lbu(record.at(6)), m.lbu(record.at(2)));
            m.sb(q.at(0x25), v.wrapping_add(h.wrapping_sub(1)));
            for (to, from) in [(4u32, 0xcu32), (5, 0xd), (6, 0xe)] {
                let c = m.lbu(record.at(from));
                m.sb(q.at(to), c);
            }
            add_prim(m, table, q);
        }
    }
}

/// A two-pixel high quad at `y` from `x` to `x + w` (800EFD00..800EFD2C:
/// the y corners first).
fn line_quad(m: &mut Machine, q: u32, x: u32, y: u32, w: u32) {
    quad_corners(m, q, x, y, w, 2, false);
}
