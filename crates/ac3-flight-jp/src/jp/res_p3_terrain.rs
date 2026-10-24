//! Patches of `terrain` routines whose JP instructions differ from PAL
//! (REVIEW.md §2, §3, §3b), part 3 of the resident wave, and the JP-only
//! fog 800943A8 and VRAM fill 800939D4 (claimed in [`dispatch`]). Each
//! patch is listed in [`REPLACES`](super::REPLACES); the functions keep the
//! generated names. Most changes are the 320x240 screen (y limits 0x78,
//! heights 0xF0).
use crate::common::{CONTEXT, GAMEPLAY};
use crate::machine::{Fault, Machine, SmallData};
use crate::{sdk, shared, terrain};
use ac3_gte::{ctrl, reg};

const SCRATCH: u32 = 0x1f80_0000;

// ---- register-level helpers ------------------------------------------------------

fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
fn sh(m: &mut Machine, address: u32, value: u32) {
    m.sh(address, value as u16);
}
fn sb(m: &mut Machine, address: u32, value: u32) {
    m.sb(address, value as u8);
}
/// `slt`: signed less-than.
fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `sra`/`srav` (shift amount masked to five bits like the hardware).
fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
/// `sllv`.
fn sllv(value: u32, shift: u32) -> u32 {
    value << (shift & 31)
}
/// `sll 16; sra 16`.
fn sext16(value: u32) -> u32 {
    value as u16 as i16 as i32 as u32
}
fn abs(value: u32) -> u32 {
    if lt(value, 0) {
        value.wrapping_neg()
    } else {
        value
    }
}
/// `lwc2`: GTE data register from memory.
fn lwc2(m: &mut Machine, register: usize, address: u32) {
    let value = m.lw(address);
    m.gte.mtc2(register, value);
}
/// `swc2`: GTE data register to memory (through `mfc2`, with its storage
/// normalization side effect).
fn swc2(m: &mut Machine, register: usize, address: u32) {
    let value = m.gte.mfc2(register);
    m.sw(address, value);
}
/// `lwl` (little-endian): merge the aligned word into the high bytes of `rt`.
fn lwl(m: &mut Machine, rt: u32, address: u32) -> u32 {
    let k = address & 3;
    let word = m.lw(address & !3);
    (rt & (0x00ff_ffff >> (k * 8))) | (word << ((3 - k) * 8))
}
/// `swl` (little-endian): store the high bytes of `rt` into the low bytes of
/// the aligned word.
fn swl(m: &mut Machine, rt: u32, address: u32) {
    let k = address & 3;
    let aligned = address & !3;
    let word = m.lw(aligned);
    let keep = if k == 3 { 0 } else { 0xffff_ff00 << (k * 8) };
    m.sw(aligned, (word & keep) | (rt >> ((3 - k) * 8)));
}
/// `div` at `pc` with the compiler's checks: `break 7` at `pc + 0xC` for a
/// zero divisor and `break 6` at `pc + 0x24` for `0x80000000 / -1`. Returns
/// (`lo`, `hi`).
fn div(m: &mut Machine, numerator: u32, divisor: u32, pc: u32) -> (u32, u32) {
    let (n, d) = (numerator as i32, divisor as i32);
    if d == 0 {
        m.raise(Fault::Trap {
            address: pc + 0xc,
            code: 7,
        });
        return (if n >= 0 { u32::MAX } else { 1 }, numerator);
    }
    if n == i32::MIN && d == -1 {
        m.raise(Fault::Trap {
            address: pc + 0x24,
            code: 6,
        });
        return (numerator, 0);
    }
    (n.wrapping_div(d) as u32, n.wrapping_rem(d) as u32)
}
/// Inline `SetRotMatrix` macro: same register effect as `sdk::set_rot_matrix`.
fn set_rotation(m: &mut Machine, matrix: u32) {
    sdk::set_rot_matrix(m, matrix);
}
/// Inline `ldtr 0,0,0`.
fn clear_translation(m: &mut Machine) {
    for register in [ctrl::TRX, ctrl::TRY, ctrl::TRZ] {
        m.gte.ctc2(register, 0);
    }
}
/// `ldfcdir r, g, b`.
fn far_color(m: &mut Machine, r: u32, g: u32, b: u32) {
    m.gte.ctc2(ctrl::RFC, r);
    m.gte.ctc2(ctrl::GFC, g);
    m.gte.ctc2(ctrl::BFC, b);
}

/// `mult` by a magic constant: the high word of the signed 64-bit product.
fn mulhi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}

// ---- register spills ---------------------------------------------------------------

/// Callee-saved registers of the frame body 800339E0 when it calls the
/// world passes (80033324 .. 8003346C): s0 the DR_OFFSET code 0xE5000000,
/// s1 and s2 its ordering-table tag masks; and the main program's s3 (1: set
/// by the start-up code before main 80025F68, never changed by the game
/// loop). 8003346C and 80085204 run with them, and the prologues of their
/// callees spill them into the guest frames.
pub const FRAME_BODY_S0: u32 = 0xe500_0000;
pub const FRAME_BODY_S1: u32 = 0x0200_0000;
pub const FRAME_BODY_S2: u32 = 0xff00_0000;
pub const MAIN_S3: u32 = 1;

/// The prologue spill of a callee whose `frame`-byte frame starts below the
/// current `sp`: its saved registers `slots` (offset, value), written before
/// the call as the callee's first stores. Natives keep registers in Rust,
/// but later routines read these stale stack words: the pad halfwords of
/// the HUD's SVECTOR locals reach the GTE VZ registers through `lwc2` (the
/// pad of 801FFE4C is the s3 that 800943A8 saves, the pad of 801FFF4C the
/// s1 that 80092850 saves), as for the scratchpad spills of `render.rs`.
/// Only registers whose values are known here are written.
pub fn spill(m: &mut Machine, frame: u32, slots: &[(u32, u32)]) {
    let base = m.sp.wrapping_sub(frame);
    for &(offset, value) in slots {
        m.sw(base.wrapping_add(offset), value);
    }
}

/// 8003346C (JP; PAL 800331D4, exact, calls differ, REVIEW.md §3b): terrain (one
/// or two views), the depth masks and animated palette when enabled, the
/// depth fog through the JP fog routine 800943A8 (PAL 80092BCC has no JP
/// counterpart), and the sky of the mission's background type (gameplay
/// +0x244 +2, low three bits). Every stage advances the primitive pointer
/// at gameplay +0x14. Returns the original `v0` (the last stage's
/// primitive pointer, or 1/4 without a sky). Its frame and the frames of
/// its callees get the original register spills ([`spill`]: s0 the camera
/// block, s1..s3 those of 800339E0).
pub fn render_world(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x148);
    // the prologue: `ra` into 800339E0 and the caller's s0
    m.sw(sp + 0x144, 0x8003_3d0c);
    m.sw(sp + 0x140, FRAME_BODY_S0);
    let result = render_world_body(m, sp);
    m.pop_frame(0x148);
    result
}

fn render_world_body(m: &mut Machine, sp: u32) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28); // s0
    let angles = sp + 0x128;
    for (i, offset) in [0x14_u32, 0x16, 0x18].into_iter().enumerate() {
        let angle = lhu(m, camera + offset);
        sh(m, angles + i as u32 * 2, sra(angle << 16, 18));
    }
    let view = sp + 0x10;
    let [s0, s1, s2, s3] = [camera, FRAME_BODY_S1, FRAME_BODY_S2, MAIN_S3];
    if !m.lw(gameplay) & 0x100 != 0 {
        spill(m, 0x18, &[(0x10, s0), (0x14, 0x8003_34e8)]);
        terrain::build_view(m, camera + 4, angles, camera + 0x1c, view);
        terrain_pass(m, view, camera, 0x8003_354c);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay) & 8 != 0 {
        let position = sp + 0x130;
        let x = m.lw(camera + 4).wrapping_add(0x4000_0000);
        m.sw(position, x);
        let y = m.lw(camera + 8).wrapping_add(0xff40_0000);
        m.sw(position + 4, y);
        let z = m.lw(camera + 0xc);
        m.sw(position + 8, z);
        spill(m, 0x18, &[(0x10, s0), (0x14, 0x8003_35ac)]);
        terrain::build_view(m, position, angles, camera + 0x1c, view);
        terrain_pass(m, view, camera, 0x8003_3610);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay) & 4 != 0 {
        let ot = m.lw(gameplay + 0x10).wrapping_add(4);
        let packets = m.lw(gameplay + 0x14);
        let next = terrain::depth_masks(m, ot, packets);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, next);
    }
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244);
    let count = m.lw(params + 0x74);
    if count != 0 {
        let ot = m.lw(gameplay + 0x10).wrapping_add(4);
        let packets = m.lw(gameplay + 0x14);
        spill(m, 0x28, &[(0xc, s3), (8, s2), (4, s1), (0, s0)]);
        let next = terrain::animate_palette(m, ot, packets, params + 0x78, count);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, next);
    }
    let gameplay = m.gw(GAMEPLAY);
    if !m.lw(gameplay) & 0x200 != 0 {
        let context = m.gw(CONTEXT);
        let packets = m.lw(gameplay + 0x14);
        let buffer = m.lw(context + 4);
        let ot = m.lw(gameplay + 0x10);
        let index = ((((buffer << 8).wrapping_add(buffer)) << 2).wrapping_add(buffer)) << 2;
        let environments = m.gw(0x2e4);
        let rect = m.lw(environments.wrapping_add(index));
        spill(m, 0x10, &[(4, s1), (8, s2), (0xc, s3), (0, s0)]);
        let next = fog_943a8(m, ot.wrapping_add(4), packets, rect, gameplay + 0x298);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, next);
    }
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244);
    let ot = m.lw(gameplay + 0x10).wrapping_add(0x3ffc);
    let kind = lhu(m, params + 2) & 7;
    let next = match kind {
        1 => {
            let packets = m.lw(gameplay + 0x14);
            let slots = [
                (0x18, s2),
                (0x1c, s3),
                (0x10, s0),
                (0x14, s1),
                (0x20, 0x8003_3760),
            ];
            spill(m, 0x28, &slots);
            draw_sky(m, ot, packets)
        }
        2 => {
            let packets = m.lw(gameplay + 0x14);
            spill(m, 0x18, &[(0x10, s0), (0x14, 0x8003_3778)]);
            terrain::draw_cloud_sky(m, sp + 0x10, ot, packets)
        }
        3 => {
            let packets = m.lw(gameplay + 0x14);
            let slots = [
                (0x1c, s3),
                (0x18, s2),
                (0x14, s1),
                (0x10, s0),
                (0x20, 0x8003_378c),
            ];
            spill(m, 0x28, &slots);
            draw_dome_sky(m, ot, packets)
        }
        4 => {
            let packets = m.lw(gameplay + 0x14);
            let slots = [
                (0x28, 0x8003_37a0),
                (0x1c, s3),
                (0x18, s2),
                (0x14, s1),
                (0x10, s0),
            ];
            spill(m, 0x30, &slots);
            draw_night_sky(m, ot, packets)
        }
        0 => return 1,
        _ => return 4,
    };
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
    next
}

/// 80033538..80033554: load the camera matrix (inline SetRotMatrix and
/// SetTransMatrix of gameplay +0x4C) and draw the terrain of `view`
/// (the spill of 8005E6DC: s0 `camera`, s1..s3 of 800339E0, `ra`).
fn terrain_pass(m: &mut Machine, view: u32, camera: u32, ra: u32) {
    let gameplay = m.gw(GAMEPLAY);
    set_rotation(m, gameplay + 0x4c);
    sdk::set_trans_matrix(m, gameplay + 0x4c);
    let ot = m.lw(gameplay + 0x10).wrapping_add(4);
    let packets = m.lw(gameplay + 0x14);
    let slots = [
        (0x118, FRAME_BODY_S2),
        (0x114, FRAME_BODY_S1),
        (0x120, ra),
        (0x11c, MAIN_S3),
        (0x110, camera),
    ];
    spill(m, 0x128, &slots);
    let next = terrain::draw_terrain(m, gameplay + 0x244, view, ot, packets);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
}

/// 80061FA8 (JP; PAL 800607DC, near, `imm:2`): one level of the recursive
/// near-face subdivision, as PAL with the 240-line screen: a projected y
/// is inside for -0x78..0x78 (PAL -0x80..0x80). `quad` holds four 12-byte
/// vertices, +0x30.. their projections, +0x40.. their SZ. Emits a POLY_GT4
/// when small enough, otherwise recurses into four children at `quad +
/// 0x48` (`bits` selects the split diagonal). Returns 1 when drawn or
/// subdivided, 0 when rejected.
pub fn subdivide_level(m: &mut Machine, quad: u32, work: u32, level: u32, bits: u32) -> u32 {
    m.push_frame(0x38);
    let result = subdivide_level_body(m, quad, work, level, bits);
    m.pop_frame(0x38);
    result
}

fn subdivide_level_body(m: &mut Machine, s0: u32, s6: u32, level: u32, s8: u32) -> u32 {
    let mut s2 = level;
    lwc2(m, reg::VXY0, s0);
    lwc2(m, reg::VZ0, s0 + 4);
    lwc2(m, reg::VXY1, s0 + 0xc);
    lwc2(m, reg::VZ1, s0 + 0x10);
    lwc2(m, reg::VXY2, s0 + 0x18);
    lwc2(m, reg::VZ2, s0 + 0x1c);
    let mut outside = 0_u32; // a3
    let s1 = s0 + 0x48;
    m.gte.rtpt(true, false);
    let packet = m.lw(s6 + 4); // t1
    let clut = lhu(m, s6 + 0x268);
    let depth = m.lw(s6 + 0x264); // a0
    sh(m, packet + 0x1a, clut);
    let tpage = lhu(m, s6 + 0x26a);
    sh(m, packet + 0xe, tpage);
    swc2(m, reg::SXY0, s0 + 0x30);
    swc2(m, reg::SXY1, s0 + 0x34);
    swc2(m, reg::SXY2, s0 + 0x38);
    let mut inside_found = false;
    for corner in 0..4_u32 {
        let t0 = s0 + corner * 4;
        let mut inside = 0_u32;
        let x = lh(m, t0 + 0x30);
        if lt(x, 0xffff_ff60) {
            outside |= 1 << corner;
        } else if !lt(x, 0xa1) {
            outside |= 0x10 << corner;
        } else {
            inside = 1;
        }
        let y = lh(m, t0 + 0x32);
        if lt(y, 0xffff_ff88) {
            outside |= 0x100 << corner;
        } else if !lt(y, 0x79) {
            outside |= 0x1000 << corner;
        } else {
            inside += 1;
        }
        if !lt(inside, 2) {
            inside_found = true;
            break;
        }
    }
    if !inside_found {
        for shift in [0, 4, 8, 12] {
            if sra(outside, shift) & 0xf == 0xf {
                return 0;
            }
        }
    }
    let z1 = m.gte.mfc2(reg::SZ1);
    sh(m, s0 + 0x40, z1);
    let z2 = m.gte.mfc2(reg::SZ2);
    sh(m, s0 + 0x42, z2);
    let z3 = m.gte.mfc2(reg::SZ3);
    sh(m, s0 + 0x44, z3);
    m.gte.nclip();
    let mut a2 = lhu(m, s0 + 0x42);
    if lt(lh(m, s0 + 0x42), lh(m, s0 + 0x40)) {
        a2 = lhu(m, s0 + 0x40);
    }
    let mut a1 = lh(m, s0 + 0x46);
    if lt(a1, lh(m, s0 + 0x44)) {
        a1 = lhu(m, s0 + 0x44);
    }
    if lt(a1 << 16, a2 << 16) {
        a1 = a2;
    }
    let far = sext16(a1);
    if lt(far, 0x1f) {
        return 0;
    }
    let mut area = m.gte.mfc2(reg::MAC0);
    lwc2(m, reg::SXYP, s0 + 0x3c);
    area = abs(area);
    m.gte.nclip();
    let second = abs(m.gte.mfc2(reg::MAC0));
    let area = area.wrapping_add(second);
    if !lt(0x13ffe, area) && !lt(s2, 4) {
        s2 = 3;
    }
    if lt(area, 0x703f) && !lt(s2, 3) {
        s2 = 2;
    }
    if lt(area, 0x307f) && !lt(s2, 2) {
        s2 = 1;
    }
    if lt(area, 0x14ff) {
        s2 = if lt(0, s2) {
            u32::MAX
        } else {
            s2.wrapping_sub(1)
        };
    } else {
        s2 = s2.wrapping_sub(1);
    }
    if lt(s2, 0) {
        // Small enough: one POLY_GT4.
        let (t7, t6, t5, t4, t3, t2, t0, a3, a2, a1, v1, v0) = if s8 & 1 != 0 {
            (
                m.lw(s0 + 0x14),
                m.lw(s0 + 0x2c),
                m.lw(s0 + 8),
                m.lw(s0 + 0x20),
                lhu(m, s0 + 0x12),
                lhu(m, s0 + 0x2a),
                lhu(m, s0 + 6),
                lhu(m, s0 + 0x1e),
                m.lw(s0 + 0x34),
                m.lw(s0 + 0x3c),
                m.lw(s0 + 0x30),
                m.lw(s0 + 0x38),
            )
        } else {
            (
                m.lw(s0 + 8),
                m.lw(s0 + 0x14),
                m.lw(s0 + 0x20),
                m.lw(s0 + 0x2c),
                lhu(m, s0 + 6),
                lhu(m, s0 + 0x12),
                lhu(m, s0 + 0x1e),
                lhu(m, s0 + 0x2a),
                m.lw(s0 + 0x30),
                m.lw(s0 + 0x34),
                m.lw(s0 + 0x38),
                m.lw(s0 + 0x3c),
            )
        };
        m.sw(packet + 4, t7);
        m.sw(packet + 0x10, t6);
        m.sw(packet + 0x1c, t5);
        m.sw(packet + 0x28, t4);
        sh(m, packet + 0xc, t3);
        sh(m, packet + 0x18, t2);
        sh(m, packet + 0x24, t0);
        sh(m, packet + 0x30, a3);
        m.sw(packet + 8, a2);
        m.sw(packet + 0x14, a1);
        m.sw(packet + 0x20, v1);
        m.sw(packet + 0x2c, v0);
        let ot = m.lw(s6);
        let bucket = (depth << 2).wrapping_add(ot);
        let head = m.lw(bucket) | 0x0c00_0000;
        m.sw(packet, head);
        let ot = m.lw(s6);
        let bucket = (depth << 2).wrapping_add(ot);
        m.sw(bucket, packet & 0x00ff_ffff);
        let next = m.lw(s6 + 4).wrapping_add(0x34);
        m.sw(s6 + 4, next);
        return 1;
    }
    // Split into four children at s1.
    if s8 & 1 == 0 {
        terrain::midpoint(m, s1, s0 + 0xc, s0 + 0x18);
    } else {
        terrain::midpoint(m, s1, s0, s0 + 0x24);
    }
    let s7 = s0 + 0x18;
    terrain::midpoint(m, s1 + 0xc, s0, s7);
    let s3 = s1 + 0x18;
    let s4 = s0 + 0xc;
    terrain::midpoint(m, s3, s0, s4);
    let child = |m: &mut Machine, vertex: u32, projection: u32, z: u32, bits: u32| -> u32 {
        for offset in [0, 4, 8] {
            let value = m.lw(vertex + offset);
            m.sw(s1 + 0x24 + offset, value);
        }
        let value = m.lw(projection);
        m.sw(s1 + 0x3c, value);
        let value = lhu(m, z);
        sh(m, s1 + 0x46, value);
        subdivide_level(m, s1, s6, s2, bits)
    };
    let shift = |m: &mut Machine| {
        for offset in [0, 4, 8] {
            let value = m.lw(s1 + 0x18 + offset);
            m.sw(s1 + 0xc + offset, value);
        }
    };
    if child(m, s0, s0 + 0x30, s0 + 0x40, s8.wrapping_add(2)) != 0 {
        terrain::emit_triangle(m, s0, s4, s3, s6);
    }
    let s5 = s0 + 0x24;
    shift(m);
    terrain::midpoint(m, s3, s4, s5);
    if child(m, s4, s0 + 0x34, s0 + 0x42, s8.wrapping_add(3)) != 0 {
        terrain::emit_triangle(m, s4, s5, s3, s6);
    }
    shift(m);
    terrain::midpoint(m, s3, s7, s5);
    if child(m, s5, s0 + 0x3c, s0 + 0x46, s8) != 0 {
        terrain::emit_triangle(m, s5, s7, s3, s6);
    }
    shift(m);
    terrain::midpoint(m, s3, s0, s7);
    if child(m, s7, s0 + 0x38, s0 + 0x44, s8.wrapping_add(1)) != 0 {
        terrain::emit_triangle(m, s7, s0, s3, s6);
    }
    1
}

/// 800957A4 (JP; PAL 80093E2C, near, `imm:1`): horizon/sky background, as
/// PAL with the mission halfword at context +0x38C (PAL +0x390).
/// Scratchpad 0x1F800000 holds the OT pointer and primitive pointer,
/// 0x1F80001C the sky work block. Ends with a mask-mode packet linked to
/// `ot`. Returns the primitive pointer.
pub fn draw_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x28);
    let base = SCRATCH; // s1
    let sky = SCRATCH + 0x1c; // s0
    m.sw(base, ot);
    m.sw(base + 4, packets + 8);
    sky_camera(m, base, sky);
    let context = m.gw(CONTEXT);
    let mode = lh(m, context + 0x38c);
    m.sw(sky + 0x170, u32::from(mode == 6));
    sky_colors(m, base, sky);
    let bands = sky_bands(m, base, sky);
    project_bands(m, sky, bands);
    terrain::classify_corners(m, sky);
    draw_bands(m, base, sky);
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244);
    if lh(m, params + 2) & 0x200 != 0 {
        draw_sun(m, base, sky);
    }
    let mask = u32::from(m.lw(sky + 0x168) & 4 != 0);
    let next = m.lw(base + 4);
    let mut a1 = packets;
    if next == packets + 8 {
        m.sw(a1, 0x0100_0000);
        m.sw(a1 + 4, 0xe600_0000 | mask);
        let link = lwl(m, packets + 8, ot + 2);
        swl(m, link, a1 + 2);
        swl(m, a1 << 8, ot + 2);
    } else {
        m.sw(a1, ((packets + 8) & 0x00ff_ffff) | 0x0100_0000);
        m.sw(a1 + 4, 0xe600_0001);
        a1 = m.lw(base + 4);
        m.sw(a1, 0x0100_0000);
        m.sw(a1 + 4, 0xe600_0000 | mask);
        let link = lwl(m, 0x0100_0000, ot + 2);
        swl(m, link, a1 + 2);
        swl(m, packets << 8, ot + 2);
    }
    let flags = m.gw(0x41c);
    m.sw(base + 4, a1 + 8);
    m.sw(flags + 0x5c8, 0);
    let result = m.lw(base + 4);
    m.pop_frame(0x28);
    result
}

/// 80094718 (JP; PAL 80092E50, changed, `del:1 imm:3 ins:19 op:8 reg:6`):
/// camera position (>> 12), angles and rotation for the sky. JP copies the
/// camera's angle halfwords one by one (the same bytes as PAL's word and
/// halfword copies) and centres the screen corners on the 240-line display
/// (y -0x78/0x78, PAL -0x80/0x80).
pub fn sky_camera(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x20);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    for (to, from) in [(8, 4), (0xc, 8), (0x10, 0xc)] {
        let value = m.lw(camera + from);
        m.sw(base + to, sra(value, 12));
    }
    for (to, from) in [(0x14, 0x14), (0x16, 0x16), (0x18, 0x18), (0x1a, 0x24)] {
        let value = lhu(m, camera + from);
        sh(m, base + to, value);
    }
    let matrix = sky + 8;
    let pitch = lhu(m, base + 0x14);
    m.sw(sky, pitch);
    let roll = lhu(m, base + 0x18);
    sh(m, sky + 4, roll);
    shared::f_800306ac(m, [sky, matrix, 0, 0]);
    set_rotation(m, matrix);
    clear_translation(m);
    m.sw(sky + 0x28, 0);
    let flags = m.gw(0x41c);
    sh(m, sky + 0xf4, 0xff60);
    sh(m, sky + 0xe8, 0xff60);
    sh(m, sky + 0xf0, 0xa0);
    sh(m, sky + 0xec, 0xa0);
    sh(m, sky + 0xee, 0xff88);
    sh(m, sky + 0xea, 0xff88);
    sh(m, sky + 0xf6, 0x78);
    sh(m, sky + 0xf2, 0x78);
    let flags = m.lw(flags + 0x5c8);
    m.sw(sky + 0x168, flags);
    let clouds = if flags & 1 != 0 {
        true
    } else {
        let gameplay = m.gw(GAMEPLAY);
        let params = m.lw(gameplay + 0x244);
        lhu(m, params + 2) & 0x400 != 0
    };
    m.sw(sky + 0x16c, u32::from(clouds));
    m.pop_frame(0x20);
}

/// 80094D10 (JP; PAL 800933CC, near, `del:4 imm:2 ins:17 op:2 ref:1 reg:6`):
/// band colours (+0xF8..+0x10C) from mission parameters, blended by camera
/// altitude and pitch, then the optional flash tint, as PAL; JP reloads
/// the mission parameter block `[[gp+0x2E0]+0x244]` for each read (PAL
/// keeps it in s6).
pub fn sky_colors(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x30);
    let params = sky_params(m);
    let color = m.lw(params + 0x1c) | 0x0003_0303;
    m.sw(sky + 0xf8, color);
    let altitude = m.lw(base + 0xc);
    if lt(altitude, 0xffff_7f18) {
        terrain::sky_altitude_color(m, sky + 0xf8, altitude);
    }
    let params = sky_params(m);
    let mut s0 = lh(m, params + 0x3e).wrapping_shl(1);
    let pitch = sra(lhu(m, base + 0x16) << 16, 17);
    s0 = s0.wrapping_sub(pitch) & 0x1fff;
    if !lt(s0, 0x1001) {
        s0 = 0x2000 - s0;
    }
    for (target, source) in [(0xfc, 0x14), (0x100, 0x24), (0x104, 0x18), (0x108, 0x28)] {
        let params = sky_params(m);
        let value = m.lw(params + source);
        m.sw(sky + target, value);
    }
    if !lt(s0, 0x1000) {
        let a = m.lw(sky + 0x104);
        let b = m.lw(sky + 0x108);
        m.sw(sky + 0xfc, a);
        m.sw(sky + 0x100, b);
    } else if s0 != 0 {
        for band in 1..3_u32 {
            let current = m.lw(sky + band * 4 + 0xf8);
            let target = m.lw(sky + band * 4 + 0x100);
            if current != target {
                terrain::blend_color(m, sky + band * 4 + 0xf8, target, s0);
            }
        }
    }
    let params = sky_params(m);
    let value = m.lw(params + 8);
    m.sw(sky + 0x108, value);
    m.sw(sky + 0x104, value);
    let flags = m.lw(sky + 0x168);
    let params = sky_params(m);
    let value = m.lw(params + 4);
    m.sw(sky + 0x10c, value);
    if flags & 2 != 0 {
        let flash = m.gw(0x41c);
        let strength = m.lw(flash + 0x5cc);
        if strength != 0 {
            m.gte.mtc2(reg::RGBC, 0);
            m.gte.mtc2(reg::IR0, strength);
            for band in 0..3_u32 {
                let color = m.lw(sky + band * 4 + 0xf8);
                for register in [reg::IR1, reg::IR2, reg::IR3] {
                    m.gte.mtc2(register, 0x5b0);
                }
                m.gte.mtc2(reg::MAC1, (color & 0xff) << 4);
                let shifted = sra(color, 8);
                m.gte.mtc2(reg::MAC2, (shifted & 0xff) << 4);
                m.gte.mtc2(reg::MAC3, sra(shifted, 4) & 0xff0);
                m.gte.gpl(true, false);
                let color = m.gte.mfc2(reg::RGB2);
                m.sw(sky + band * 4 + 0xf8, color);
            }
        }
    }
    m.pop_frame(0x30);
}

/// The mission parameter block `[[gp+0x2E0]+0x244]`.
fn sky_params(m: &mut Machine) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    m.lw(gameplay + 0x244)
}

/// 80094AC8 (JP; PAL 800931B8, changed, `del:9 imm:1 ins:22 op:18 reg:18`):
/// band edge points (+0x2C.., five x pairs at the band heights) and limits
/// for the camera altitude, as PAL in the JP store order with the mission
/// parameter block reloaded per read; the division by the projection
/// distance traps at 80094BA8/80094BC0. Returns the band count (4 or 5).
pub fn sky_bands(m: &mut Machine, base: u32, sky: u32) -> u32 {
    m.push_frame(0x20);
    let gameplay = m.gw(GAMEPLAY); // t1
    let height = m.lw(base + 0xc).wrapping_neg(); // t0
    let shift = if lt(0x8ffe, height) { 3 } else { 2 }; // s2
    let params = m.lw(gameplay + 0x244);
    let high = lh(m, params + 0x22);
    sh(m, sky + 0x2e, sra(height.wrapping_add(high << 1), shift));
    let params = m.lw(gameplay + 0x244);
    let middle = lh(m, params + 0x20);
    sh(m, sky + 0x36, sra(height.wrapping_add(middle << 1), shift));
    let params = m.lw(gameplay + 0x244);
    let second = lh(m, params + 0x10) << 1;
    sh(m, sky + 0x3e, sra(height.wrapping_add(second), shift));
    let params = m.lw(gameplay + 0x244);
    let half = sra(lhu(m, params + 0x10) << 16, 17);
    sh(m, sky + 0x4e, sra(height, shift));
    let last = sra(height.wrapping_add(half), shift);
    sh(m, sky + 0x46, last);
    // Widescreen: the band edges as far out as the wider view (H * k).
    let distance = lh(m, base + 0x1a);
    let distance = wide_screen_distance(m, distance);
    let (mut width, _) = div(m, 0x003b_ff88, distance, 0x8009_4b9c);
    let mut edge = sky;
    for band in 0..5_u32 {
        let y = lhu(m, edge + 0x2e);
        sh(m, edge + 0x2c, width.wrapping_neg());
        sh(m, edge + 0x30, width);
        sh(m, edge + 0x32, y);
        if band == 2 {
            width = sra(width, 1);
        }
        edge += 8;
    }
    let limit = sra(0x7fff, shift - 1);
    m.sw(sky + 0x80, limit);
    m.sw(sky + 0x7c, limit);
    m.sw(sky + 0x84, sra(0x7800, shift - 1));
    let indoor = m.lw(sky + 0x170);
    m.sw(sky + 0x88, sra(0x7800, shift));
    if indoor != 0 {
        let y = lhu(m, sky + 0x4e);
        m.sw(sky + 0x110, 0x6800);
        let y = y.wrapping_add(0xc00);
        sh(m, sky + 0x52, y);
        sh(m, sky + 0x4e, y);
    } else {
        let clouds = m.lw(sky + 0x16c);
        let camera = m.lw(gameplay + 0x28);
        if clouds != 0 {
            let a = lh(m, camera + 0x28);
            let b = lh(m, camera + 0x2a);
            let ceiling = sra(a.wrapping_add(b), 1);
            m.sw(sky + 0x110, ceiling);
            if lt(ceiling, 0x3400) {
                let x0 = lhu(m, sky + 0x4c);
                let x1 = lhu(m, sky + 0x50);
                sh(m, sky + 0x4c, sra(x0 << 16, 17));
                sh(m, sky + 0x50, sra(x1 << 16, 17));
            }
        } else {
            m.sw(sky + 0x110, 0x6800);
        }
    }
    let mut bands = 4_u32;
    if lt(height, m.lw(sky + 0x110)) {
        let ceiling = m.lw(sky + 0x110);
        m.gte.mtc2(reg::IR1, ceiling);
        m.gte.mtc2(reg::IR2, height);
        bands = 5;
        m.gte.sqr(false, true);
        let a = m.gte.mfc2(reg::MAC1);
        let b = m.gte.mfc2(reg::MAC2);
        let root = shared::f_80082740(m, [a.wrapping_sub(b), sky + 0x28, 5, height]);
        m.sw(sky + 0x8c, sra(root, shift));
    }
    m.pop_frame(0x20);
    bands
}

/// 800948A0 (JP; PAL 80092F90, near, `imm:2`): project the band edges (RTPT
/// of the two edge points per band) and clip each consecutive pair to the
/// screen (8005F2B4) of 320x240 (y limit 0x78, PAL 0x80). Sets the
/// visible-band bits at +0x28.
pub fn project_bands(m: &mut Machine, sky: u32, bands: u32) {
    m.push_frame(0x30);
    m.gte.mtc2(reg::VXY2, 0);
    m.gte.mtc2(reg::VZ2, 0);
    let mut s1 = sky + 0x2c;
    let mut s0 = sky + 0x54;
    let mut s3 = sky + 0x7c;
    let mut pending = 1_u32; // a0
    let mut band = 0_u32;
    while lt(band, bands) {
        let depth = m.lw(s3);
        lwc2(m, reg::VXY0, s1);
        lwc2(m, reg::VXY1, s1 + 4);
        m.gte.mtc2(reg::VZ0, depth);
        m.gte.mtc2(reg::VZ1, depth);
        m.gte.rtpt(true, false);
        if pending == 0 {
            terrain::clip_segment(m, s0 - 8, s0 - 4, 0xa0, 0x78);
        }
        pending = m.gte.cfc2(ctrl::FLAG) & 0x6000;
        if pending == 0 {
            swc2(m, reg::SXY0, s1);
            swc2(m, reg::SXY1, s1 + 4);
            swc2(m, reg::SXY0, s0);
            swc2(m, reg::SXY1, s0 + 4);
            let visible = lhu(m, sky + 0x28) | sllv(1, band);
            sh(m, sky + 0x28, visible);
        }
        band = band.wrapping_add(1);
        s1 += 8;
        s0 += 8;
        s3 += 4;
    }
    if pending == 0 {
        terrain::clip_segment(m, s0 - 8, s0 - 4, 0xa0, 0x78);
    }
    m.pop_frame(0x30);
}

/// 80095074 (JP; PAL 800936FC, near, `imm:1`): draw the sky bands (flat and
/// gradient), stars and cloud band, or a single 320x240 background tile
/// (PAL 320x256) when no band edge is on screen.
pub fn draw_bands(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x28);
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244);
    let mut stars = if lhu(m, params + 2) & 0x100 != 0 {
        let sun = m.lw(gameplay + 0x268);
        lbu(m, sun + 0x1c).wrapping_add(1)
    } else {
        0
    };
    let visible = lh(m, sky + 0x28); // s2
    if visible != 0 {
        if visible & 1 != 0 {
            terrain::flat_band(m, base, sky, 0);
        }
        if visible & 3 != 0 {
            terrain::gradient_band(m, base, sky, 1);
        }
        if visible & 6 != 0 {
            terrain::gradient_band(m, base, sky, 2);
        }
        if stars != 0 {
            draw_stars(m, base, sky, stars);
        }
        if visible & 0xc != 0 {
            terrain::gradient_band(m, base, sky, 3);
        }
        if visible & 8 != 0 {
            if m.lw(sky + 0x170) != 0 {
                let color = m.lw(sky + 0x10c);
                m.sw(sky + 0x108, color);
                terrain::gradient_band(m, base, sky, 4);
            } else {
                terrain::flat_band(m, base, sky, 4);
            }
        }
        if visible & 0x10 != 0 && m.lw(sky + 0x16c) != 0 {
            terrain::flat_band(m, base, sky, 5);
        }
        m.pop_frame(0x28);
        return;
    }
    let altitude = m.lw(base + 0xc);
    let angle = sdk::ratan2(m, altitude, 0x7fff);
    let color = if lt(angle << 2, lh(m, base + 0x14)) {
        m.lw(sky + 0xf8)
    } else {
        stars = 0;
        let altitude = m.lw(base + 0xc).wrapping_neg();
        if !lt(altitude, m.lw(sky + 0x110)) {
            m.lw(sky + 0x108)
        } else if m.lw(sky + 0x16c) != 0 {
            m.lw(sky + 0x10c)
        } else {
            u32::MAX
        }
    };
    if !lt(color, 0) {
        let packet = m.lw(base + 4);
        let next = packet + 0x10;
        m.sw(packet, (next & 0x00ff_ffff) | 0x0300_0000);
        m.sw(packet + 4, color | 0x6000_0000);
        let corner = m.lw(sky + 0xe8);
        m.sw(packet + 0xc, 0x00f0_0140);
        m.sw(packet + 8, corner);
        m.sw(base + 4, next);
    }
    if stars != 0 {
        draw_stars(m, base, sky, stars);
    }
    m.pop_frame(0x28);
}

/// 80095930 (JP; PAL 80093FB8, changed, `del:1 imm:6 ins:17 op:6 reg:38`):
/// the sun/moon strip of semi-transparent POLY_FT4 packets along the
/// horizon direction from the gameplay +0x268 block, as PAL with the JP
/// screen (the strip angle `ratan2(0xC8, H)`, PAL 0xCD; a visible y sum
/// below 0x119, PAL 0x129) and the clut row 0x59 or 0x39 by the colour
/// word's top byte, the 0x59 strip ended by a DR_TPAGE (0xE1000220).
pub fn draw_sun(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x28);
    draw_sun_body(m, base, sky);
    m.pop_frame(0x28);
}

fn draw_sun_body(m: &mut Machine, s2: u32, s1: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let sun = m.lw(gameplay + 0x268);
    let word = m.lw(sun + 4);
    let mut color = m.lw(sun + 0x18); // t2
    let altitude = m.lw(s2 + 0xc);
    let wide = word & 1; // s4
    let s0 = sra(word, 16).wrapping_sub(altitude);
    let depth = sra(s0.wrapping_sub(0xfa0), 3);
    if !lt(depth, 0x1000) {
        return;
    }
    set_rotation(m, gameplay + 0x4c);
    // JP: the clut row 0x59 (with a DR_TPAGE after the strip) when the
    // colour word's top byte is set, else 0x39 (PAL: always 0x39)
    let row: u32 = if sra(color, 24) != 0 { 0x59 } else { 0x39 }; // s3
    clear_translation(m);
    m.gte.mtc2(reg::IR0, depth);
    far_color(m, 0, 0, 0);
    color = (color & 0x00ff_ffff) | 0x2e00_0000;
    m.gte.mtc2(reg::RGBC, color);
    m.gte.dpcs(true, false);
    // Widescreen: the glow band over the wider view (H * k).
    let distance = lh(m, s2 + 0x1a);
    let distance = wide_screen_distance(m, distance);
    let elevation = sdk::ratan2(m, 0xc8, distance); // a2
    let heading = sra(lhu(m, s2 + 0x16) << 16, 18);
    let mut t0 = heading.wrapping_sub(elevation) & 0xf80;
    color = m.gte.mfc2(reg::RGB2);
    let spread = if wide != 0 { 0xffff_e800 } else { 0xffff_f400 };
    sh(m, s1 + 0x116, sra(s0.wrapping_add(spread), 2));
    sh(m, s1 + 0x11a, sra(s0.wrapping_sub(spread), 2));
    let table = 0x800b_7104_u32; // rcossin_tbl
    let load_pair = |m: &mut Machine, angle: u32| {
        let entry = (angle << 2).wrapping_add(table);
        let sine = lhu(m, entry) << 2;
        sh(m, s1 + 0x118, sine);
        sh(m, s1 + 0x114, sine);
        let cosine = lh(m, entry + 2) << 2;
        lwc2(m, reg::VXY0, s1 + 0x114);
        lwc2(m, reg::VXY1, s1 + 0x118);
        m.gte.mtc2(reg::VZ0, cosine);
        m.gte.mtc2(reg::VZ1, cosine);
    };
    load_pair(m, t0);
    m.gte.mtc2(reg::VXY2, 0);
    m.gte.mtc2(reg::VZ2, 0);
    let mut visible_run = 0_u32; // t1
    let mut packet = m.lw(s2 + 4); // a1
    m.gte.rtpt(true, false);
    let mut remaining = sra(elevation, 6).wrapping_add(2); // a3
    let mut step = sra(t0, 7).wrapping_sub(1); // a2
    swc2(m, reg::SXY0, s1 + 0x11c);
    swc2(m, reg::SXY1, s1 + 0x120);
    if lt(0, remaining) {
        loop {
            t0 = t0.wrapping_add(0x80) & 0xfff;
            load_pair(m, t0);
            let next_step = step.wrapping_add(1);
            step = next_step & 0xf;
            m.gte.rtpt(true, false);
            let previous_low = m.lw(s1 + 0x11c);
            let v = (next_step & 3) << 6;
            m.sw(packet + 8, previous_low);
            let previous_high = m.lw(s1 + 0x120);
            sb(m, packet + 0x1c, v);
            sb(m, packet + 0xc, v);
            sb(m, packet + 0x24, v + 0x3f);
            sb(m, packet + 0x14, v + 0x3f);
            m.sw(packet + 0x18, previous_high);
            let (low_u, high_u) = if wide != 0 {
                let u = (step << 4) & 0x40;
                (u.wrapping_sub(0x80), u.wrapping_sub(0x41))
            } else {
                let u = (step << 3) & 0x60;
                (u.wrapping_sub(0x80), u.wrapping_sub(0x61))
            };
            sb(m, packet + 0x15, low_u);
            sb(m, packet + 0xd, low_u);
            sb(m, packet + 0x25, high_u);
            sb(m, packet + 0x1d, high_u);
            swc2(m, reg::SXY0, s1 + 0x11c);
            swc2(m, reg::SXY1, s1 + 0x120);
            let on_screen = lt(
                abs(lh(m, s1 + 0x11c)).wrapping_add(abs(lh(m, s1 + 0x120))),
                0x169,
            ) && lt(
                abs(lh(m, s1 + 0x11e)).wrapping_add(abs(lh(m, s1 + 0x122))),
                0x119,
            );
            let emit = if on_screen {
                visible_run = 1;
                true
            } else if visible_run != 0 {
                visible_run = 0;
                true
            } else {
                if packet != m.lw(s2 + 4) {
                    break;
                }
                false
            };
            if emit {
                let next = packet + 0x28;
                m.sw(packet, (next & 0x00ff_ffff) | 0x0900_0000);
                sh(m, packet + 0xe, 0x4ed6);
                m.sw(packet + 4, color);
                sh(m, packet + 0x16, row);
                let low = m.lw(s1 + 0x11c);
                m.sw(packet + 0x10, low);
                let high = m.lw(s1 + 0x120);
                m.sw(packet + 0x20, high);
                packet = next;
            }
            remaining = remaining.wrapping_sub(1);
            if !lt(0, remaining) {
                break;
            }
        }
    }
    if packet != m.lw(s2 + 4) {
        let mut packet = packet;
        if row == 0x59 {
            let tpage = packet;
            packet = packet.wrapping_add(8);
            m.sw(tpage, ((packet << 8) >> 8) | 0x0100_0000);
            m.sw(tpage + 4, 0xe100_0220);
        }
        m.sw(s2 + 4, packet);
    }
}

/// 80095CB8 (JP; PAL 80094300, near, `imm:1`): up to 256 stars (TILE_1 runs
/// of seven packed into 0x0E-word packets) from the star table at
/// `[gp+0x41C]`, drifting with the camera; a star is on screen for |y| <
/// 0x79 (PAL 0x81).
pub fn draw_stars(m: &mut Machine, base: u32, sky: u32, count: u32) {
    if !lt(0, count) {
        return;
    }
    let count = if lt(count, 0x101) { count } else { 0x100 };
    let gameplay = m.gw(GAMEPLAY);
    set_rotation(m, gameplay + 0x2c);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0x2000);
    let start = m.lw(base + 4); // t8
    let mut t3 = start + 8;
    m.gte.mvmva(true, false, 0, 0, 3);
    m.sw(start, (t3 & 0x00ff_ffff) | 0x0100_0000);
    m.sw(start + 4, 0xe100_0220);
    let sun = m.lw(gameplay + 0x268);
    let mut table = m.gw(0x41c); // t7
    let brightness = (0x80_u32.wrapping_sub(lbu(m, sun + 0x13))) << 5;
    let a3 = m.gte.mfc2(reg::MAC1);
    let v1 = m.gte.mfc2(reg::MAC2);
    let a0 = m.gte.mfc2(reg::MAC3);
    far_color(m, 0x60, 0x70, 0x80);
    m.gte.mtc2(reg::RGBC, 0x6a80_7060);
    m.gte.mtc2(reg::IR0, brightness);
    let dx = a3.wrapping_neg();
    let dy = v1.wrapping_neg();
    m.sw(sky + 0x158, dx);
    m.sw(sky + 0x15c, dy);
    m.gte.dpcs(true, false);
    let dz = a0.wrapping_neg();
    m.sw(sky + 0x160, dz);
    sh(m, sky, (dx & 0x1fff).wrapping_sub(0x1000));
    sh(m, sky + 2, (dy & 0x1fff).wrapping_sub(0x1000));
    sh(m, sky + 4, (dz & 0x1fff).wrapping_sub(0x1000));
    let base_color = m.gte.mfc2(reg::RGB2);
    m.gte.mtc2(reg::RGBC, base_color);
    let twinkle = base_color >> 8; // t9
    set_rotation(m, gameplay + 0x4c);
    m.gte.ctc2(ctrl::TRX, 0);
    m.gte.ctc2(ctrl::TRY, 0);
    m.gte.ctc2(ctrl::TRZ, 0x2000);
    let mut run = 0_u32; // t0
    for star in 0..count {
        lwc2(m, reg::VXY0, sky);
        lwc2(m, reg::VZ0, sky + 4);
        let mut offsets = m.lw(table);
        table = table.wrapping_add(4);
        m.gte.rtps(true, false);
        let x = lhu(m, sky + 0x158).wrapping_add(offsets);
        offsets = sra(offsets, 8);
        let y = lhu(m, sky + 0x15c).wrapping_add(offsets);
        offsets = sra(offsets, 8);
        sh(m, sky, (x & 0x1fff).wrapping_sub(0x1000));
        let z = lhu(m, sky + 0x160);
        sh(m, sky + 2, (y & 0x1fff).wrapping_sub(0x1000));
        sh(
            m,
            sky + 4,
            (z.wrapping_add(offsets) & 0x1fff).wrapping_sub(0x1000),
        );
        let screen = m.gte.mfc2(reg::SXY2);
        let depth = m.gte.mfc2(reg::SZ3);
        if !lt(abs(sra(screen, 16)), 0x79) || !lt(abs(sext16(screen)), 0xa1) {
            continue;
        }
        let distance = abs(depth.wrapping_sub(0x2000));
        if !lt(distance, 0x1001) {
            continue;
        }
        m.gte.mtc2(reg::IR0, distance);
        let entry = t3.wrapping_add((run << 3) + 4);
        m.gte.dpcs(true, false);
        run += 1;
        m.sw(entry + 4, screen);
        if !lt(run, 7) {
            let next = t3 + 0x3c;
            m.sw(t3, (next & 0x00ff_ffff) | 0x0e00_0000);
            t3 = next;
            run = 0;
        }
        swc2(m, reg::RGB2, entry);
        if star & 0xf == 0 {
            sb(m, entry, twinkle);
        }
    }
    if run != 0 {
        let end = t3.wrapping_add((run << 3) + 4);
        m.sw(base + 4, end);
        m.sw(t3, (run << 25) | (end & 0x00ff_ffff));
    } else if start + 8 != t3 {
        m.sw(base + 4, t3);
    }
}

/// 80096184 (JP; PAL 800947CC, near, `imm:1`): the cloud-sky bands, as PAL
/// with a 320x240 background tile (PAL 320x256).
pub fn draw_cloud_bands(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x28);
    let s4 = lhu(m, sky + 0x28) << 16;
    let visible = sra(s4, 16);
    if visible != 0 {
        if m.lb(sky + 0x10b) == 0 {
            let first = visible & 1;
            if first != 0 {
                terrain::flat_band(m, base, sky, 0);
            }
            if visible & 8 != 0 {
                terrain::flat_band(m, base, sky, 4);
            }
            if first != 0 {
                terrain::gradient_band(m, base, sky, 1);
            }
            if sra(s4, 18) & 1 != 0 {
                terrain::gradient_band(m, base, sky, 3);
            }
        }
        if visible & 6 != 0 {
            terrain::flat_band(m, base, sky, 2);
        }
    } else if m.lb(sky + 0x10b) != 0 {
        let packet = m.lw(base + 4);
        let next = packet + 0x10;
        m.sw(packet, (next & 0x00ff_ffff) | 0x0300_0000);
        let color = m.lw(sky + 0xfc) | 0x6000_0000;
        m.sw(packet + 4, color);
        let corner = m.lw(sky + 0xe8);
        m.sw(packet + 0xc, 0x00f0_0140);
        m.sw(packet + 8, corner);
        m.sw(base + 4, next);
    }
    m.pop_frame(0x28);
}

/// 800965D4 (JP; PAL 80094C1C, near, `imm:2`): the outcode of a screen point
/// (1/2 left/right of x -0xA0..0xA0, 4/8 above/below y -0x78..0x78; PAL
/// -0x80..0x80).
pub fn screen_outcode(m: &mut Machine, point: u32) -> u32 {
    let x = lh(m, point);
    let mut code = if lt(x, 0xffff_ff60) {
        1
    } else if !lt(x, 0xa1) {
        2
    } else {
        0
    };
    let y = lh(m, point + 2);
    if lt(y, 0xffff_ff88) {
        code |= 4;
    } else if !lt(y, 0x79) {
        code |= 8;
    }
    code
}

/// 80097EB4 (JP; PAL 800964FC, near, `imm:2`): the dome sky, as PAL
/// with the 320x240 screen: its background tile from (-0xA0, -0x78)
/// (PAL -0x80) of 320x240 (PAL 320x256).
pub fn draw_dome_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x28);
    let base = SCRATCH; // s1
    let s0 = packets + 8;
    let tile = packets + 0x18;
    m.sw(base, ot);
    m.sw(base + 4, s0);
    m.sw(packets + 8, (tile & 0x00ff_ffff) | 0x0300_0000);
    m.sw(s0 + 8, 0xff88_ff60);
    m.sw(s0 + 0xc, 0x00f0_0140);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(s0 + 4, 0x6000_0000);
    m.sw(base + 4, tile);
    for (target, source) in [(0x14, 0x14), (0x16, 0x16), (0x18, 0x18), (0x1a, 0x24)] {
        let camera = m.lw(gameplay + 0x28);
        let value = lhu(m, camera + source);
        sh(m, base + target, value);
    }
    draw_stars(m, base, base + 0x1c, 0x100);
    terrain::draw_dome(m);
    let flags = m.gw(0x41c);
    let mask = u32::from(m.lw(flags + 0x5c8) & 4 != 0);
    let mut a0 = packets;
    if m.lw(base + 4) == s0 {
        m.sw(packets, 0x0100_0000);
        m.sw(packets + 4, mask | 0xe600_0000);
        let link = lwl(m, mask, ot + 2);
        swl(m, link, a0 + 2);
        swl(m, packets << 8, ot + 2);
    } else {
        m.sw(packets, (s0 & 0x00ff_ffff) | 0x0100_0000);
        m.sw(packets + 4, 0xe600_0001);
        a0 = m.lw(base + 4);
        m.sw(a0, 0x0100_0000);
        m.sw(a0 + 4, mask | 0xe600_0000);
        let link = lwl(m, 0x0100_0000, ot + 2);
        swl(m, link, a0 + 2);
        swl(m, packets << 8, ot + 2);
    }
    let flags = m.gw(0x41c);
    m.sw(flags + 0x5c8, 0);
    m.pop_frame(0x28);
    a0 + 8
}

/// 80098044 (JP; PAL 8009668C, near, `imm:2`): the night sky, as PAL
/// with the 320x240 screen: its background tile from (-0xA0, -0x78)
/// (PAL -0x80) of 320x240 (PAL 320x256).
pub fn draw_night_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x30);
    let s2 = SCRATCH;
    let context = m.gw(CONTEXT);
    let clock = m.lw(context + 0x28); // s4
    let scaled = ((clock << 4).wrapping_sub(clock)) << 9;
    let quotient =
        sra(mulhi(scaled, 0x91a2_b3c5).wrapping_add(scaled), 8).wrapping_sub(sra(scaled, 31));
    let cycles = sra(mulhi(quotient, 0x8888_8889).wrapping_add(quotient), 12)
        .wrapping_sub(sra(quotient, 31));
    let mut phase = quotient.wrapping_sub(((cycles << 4).wrapping_sub(cycles)) << 9);
    if !lt(phase, 0x801) {
        phase = 0x800;
    }
    let mut level = lh(m, ((phase & 0xfff) << 2).wrapping_add(0x800b_7106));
    level = abs(level);
    level = 0x12aa_u32.wrapping_sub(level);
    if lt(level, 0x72e) {
        level = 0x72e;
    }
    let s0 = fade_overlay(m, ot.wrapping_sub(0x3ffc), packets, 0, level);
    let s1 = s0 + 8;
    let gameplay = m.gw(GAMEPLAY);
    m.sw(s2, ot);
    m.sw(s2 + 4, s1);
    for (target, source) in [(8, 4), (0xc, 8), (0x10, 0xc)] {
        let camera = m.lw(gameplay + 0x28);
        let value = m.lw(camera + source);
        m.sw(s2 + target, value);
    }
    let camera = m.lw(gameplay + 0x28);
    let distance = lhu(m, camera + 0x24);
    sh(m, s2 + 0x1a, distance);
    let tile = s0 + 0x10;
    let after = s0 + 0x20;
    m.sw(s0 + 8, (tile & 0x00ff_ffff) | 0x0100_0000);
    m.sw(s1 + 4, 0xe100_0200);
    m.sw(s0 + 0x10, (after & 0x00ff_ffff) | 0x0400_0000);
    m.sw(tile + 4, 0x6000_0000);
    m.sw(tile + 8, 0xff88_ff60);
    m.sw(tile + 0xc, 0x00f0_0140);
    m.sw(s2 + 4, after);
    set_rotation(m, gameplay + 0x4c);
    sdk::set_trans_matrix(m, gameplay + 0x4c);
    terrain::night_dome(m, s2, clock);
    terrain::night_ground(m, s2);
    let flags = m.gw(0x41c);
    let mask = u32::from(m.lw(flags + 0x5c8) & 4 != 0);
    let result = if m.lw(s2 + 4) == s1 {
        m.sw(s0, 0x0100_0000);
        m.sw(s0 + 4, mask | 0xe600_0000);
        let link = lwl(m, s1, ot + 2);
        swl(m, link, s0 + 2);
        swl(m, s0 << 8, ot + 2);
        s0 + 8
    } else {
        m.sw(s0, (s1 & 0x00ff_ffff) | 0x0100_0000);
        m.sw(s0 + 4, 0xe600_0001);
        let last = m.lw(s2 + 4);
        let end = last + 8;
        m.sw(last, (end & 0x00ff_ffff) | 0x0100_0000);
        m.sw(last + 4, 0xe100_0220);
        m.sw(last + 8, 0x0100_0000);
        m.sw(end + 4, mask | 0xe600_0000);
        let link = lwl(m, last, ot + 2);
        swl(m, link, end + 2);
        swl(m, s0 << 8, ot + 2);
        end + 8
    };
    let flags = m.gw(0x41c);
    m.sw(flags + 0x5c8, 0);
    terrain::compact_ot_runs(m, ot.wrapping_sub(0x3ff8), 0xfff);
    m.pop_frame(0x30);
    result
}

/// 8008FAA8 (JP; PAL 8008E238, near, `imm:2`): the fade overlay, as PAL with
/// tiles 240 lines high (PAL 256).
pub fn fade_overlay(
    m: &mut Machine,
    ot: u32,
    packets: u32,
    environment: u32,
    brightness: u32,
) -> u32 {
    m.push_frame(8);
    let mut fade = 0x1000_u32.wrapping_sub(brightness);
    if lt(fade, 0) {
        fade = 0;
    }
    far_color(m, 0, 0, 0);
    m.gte.mtc2(reg::RGBC, 0x007f_7f7f);
    m.gte.mtc2(reg::IR0, fade);
    let context = m.gw(CONTEXT);
    let page = u32::from(m.lw(context + 4) < 1) << 8; // t1
    m.gte.dpcs(true, false);
    let t2 = packets + 8;
    m.sw(packets, (t2 & 0x00ff_ffff) | 0x0100_0000);
    m.sw(packets + 4, 0xe600_0001);
    let mut t0;
    if environment != 0 {
        t0 = packets + 0x14;
        let bit = page.wrapping_add(0x100) & 0x100;
        m.sw(packets + 8, (t0 & 0x00ff_ffff) | 0x0200_0000);
        m.sw(t2 + 4, (bit << 11) | 0xe500_0000);
        m.sw(t2 + 8, 0);
    } else {
        t0 = t2;
    }
    let color = m.gte.mfc2(reg::RGB2);
    let t6 = sra(page & 0x100, 4);
    let t5 = (page & 0x200) << 2;
    let tile = color | 0x6200_0000;
    let sprite = color | 0x6600_0000;
    let mut next = (t0 << 8).wrapping_add(0x2800); // t1
    let mut x = 0_u32; // a3
    while lt(x, 0x140) {
        let a0 = t0 + 0x10;
        let column = sra(x & 0x3ff, 6);
        m.sw(t0, (next >> 8) | 0x0900_0000);
        let first = column | 0x140 | t6 | t5 | 0xe100_0200;
        let second = column | 0x120 | t6 | t5 | 0xe100_0200;
        m.sw(a0.wrapping_sub(0xc), first);
        m.sw(a0 + 4, second);
        m.sw(a0.wrapping_sub(8), tile);
        m.sw(a0 + 8, sprite);
        m.sw(a0 + 0xc, x);
        m.sw(a0.wrapping_sub(4), x);
        sh(m, a0 + 0x10, 0);
        let size = if x != 0 { 0x00f0_0040 } else { 0x00f0_0100 };
        m.sw(a0 + 0x14, size);
        m.sw(a0, size);
        next = next.wrapping_add(0x2800);
        x = x.wrapping_add(0x100);
        t0 += 0x28;
    }
    let last;
    if environment != 0 {
        last = t0;
        let word = m.lw(environment);
        m.sw(t0, 0x0200_0000);
        m.sw(t0 + 8, 0);
        let y = (word << 5) >> 21;
        m.sw(t0 + 4, (word & 0x7ff) | (y << 11) | 0xe500_0000);
        t0 += 0xc;
    } else {
        last = t0 - 0x28;
    }
    let link = lwl(m, 0, ot + 2);
    swl(m, link, last + 2);
    swl(m, packets << 8, ot + 2);
    m.pop_frame(8);
    t0
}

/// 80098714 (JP; PAL 80096D5C, near, `imm:3`): one row of the night
/// ground, as PAL with the screen y limits -0x78..0x78 (PAL -0x80..0x80).
pub fn night_ground_row(m: &mut Machine, block: u32, out: u32, row: u32) {
    let row = row << 10;
    let cell_x = m.lw(block + 0x20);
    let cell_z = m.lw(block + 0x28);
    let cell_y = m.lw(block + 0x24);
    let z = cell_z.wrapping_add(0x2000).wrapping_sub(row);
    m.gte.mtc2(reg::VZ0, z);
    let distance = abs(z);
    let mut scaled = ((distance << 4).wrapping_sub(distance)) << 8;
    if lt(scaled, 0) {
        scaled = scaled.wrapping_add(0x1fff);
    }
    let width = 0x1000_u32.wrapping_sub(sra(scaled, 13)); // t0
    let mut y = cell_y.wrapping_mul(width);
    let x0 = cell_x.wrapping_sub(0x2000);
    if lt(y, 0) {
        y = y.wrapping_add(0xfff);
    }
    let y = sra(y, 12) << 16;
    let mut x = x0.wrapping_mul(width); // a2
    let mut record = out;
    for _ in 0..16 {
        let rounded = if lt(x, 0) { x.wrapping_add(0xfff) } else { x };
        let xy = (sra(rounded, 12) & 0xffff) | y;
        m.gte.mtc2(reg::VXY0, xy);
        x = x.wrapping_add(width << 10);
        m.gte.rtps(true, false);
        if lt(m.gte.cfc2(ctrl::FLAG), 0) {
            m.sw(record, u32::MAX);
        } else {
            let screen = m.gte.mfc2(reg::SXY2);
            let sy = sra(screen, 16);
            m.gte.dpcs(true, false);
            m.sw(record + 4, screen);
            let sx = sext16(screen);
            let mut code = if lt(sx, 0xffff_ff60) {
                1
            } else if !lt(sx, 0xa1) {
                2
            } else {
                0
            };
            if lt(sy, 0xffff_ff88) {
                code |= 4;
            } else if !lt(sy, 0x79) {
                code |= 8;
            }
            m.sw(record, code);
            swc2(m, reg::RGB2, record + 8);
        }
        record += 0xc;
    }
    m.sw(record, u32::MAX);
}

/// The horizontal projection distance of the terrain frustum: H, or H * k
/// (rounded down) while the world is drawn squeezed for widescreen.
fn wide_screen_distance(m: &Machine, h: u32) -> u32 {
    match crate::widescreen::x_scale(m) {
        Some((num, den)) => {
            (i64::from(h as i32) * i64::from(num)).div_euclid(i64::from(den)) as u32
        }
        None => h,
    }
}

// ---- JP-only routines ------------------------------------------------------------

/// Bucket table pointer of the fog strips: `[[gp+0x2E0]+0x288]`.
const FOG_BUCKETS: u32 = 0x288;
/// Bar-height ramp of 800939D4 (halfwords, indexed by `value / 4`).
const LETTERBOX_HEIGHTS: u32 = 0x800b_7104;

/// The low 24 bits (a packet link).
fn link(address: u32) -> u32 {
    address & 0x00ff_ffff
}

/// 800943A8 (JP-only, `new`; replaces PAL draw_fog 80092BCC in the JP world
/// pass): the depth fog as VRAM copies. Sixteen strips, one per halfword
/// bucket index of `[[gp+0x2E0]+0x288]`: an odd strip adds two 13-word
/// packets of a cache flush and two 16x16 copies each (sources from x
/// `(i & 6) << 5`, y `((i & 8) << 5) + 0xF0`, to (0x140, 0x100..0x130)),
/// inserted in front of the bucket; an even strip adds one 8-word packet,
/// a 16x64 copy from (`((i & 6) << 3) + 0x140`, `((i & 8) << 3) + 0x180`)
/// to (0x140, 0x100), and makes the bucket point to it. All set the mask
/// mode `0xE6000000 | (flags bit 2)` of `[[gp+0x41C]+0x5C8]`. Then the
/// seventeenth bucket gets a draw area (0x140,0x100)..(0x14F,0x13F) with
/// a TILE of the mission's colour (`[[gp+0x2E0]+0x244]+8`) and the restored
/// draw area of `rect` and drawing offset `[offset]`. Returns the next
/// packet address.
pub fn fog_943a8(m: &mut Machine, ot: u32, packets: u32, rect: u32, offset: u32) -> u32 {
    m.push_frame(0x10);
    let flags = m.gw(0x41c);
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(flags + 0x5c8);
    let mut buckets = m.lw(gameplay + FOG_BUCKETS); // t7
    let mask = if flags & 4 != 0 {
        0xe600_0001
    } else {
        0xe600_0000
    }; // t5
    let mut t4 = packets;
    for i in 0..16_u32 {
        let index = lh(m, buckets);
        let p = t4;
        if i & 1 != 0 {
            let second = p + 0x34;
            let bucket = (index << 2).wrapping_add(ot);
            m.sw(p, link(second) | 0x0c00_0000);
            m.sw(second, 0x0c00_0000);
            let old = lwl(m, 0, bucket + 2);
            swl(m, old, second + 2);
            swl(m, p << 8, bucket + 2);
            t4 = p + 0x68;
            let x = (i & 6) << 5;
            let y = (((i & 8) << 5) + 0xf0) << 16;
            m.sw(p + 0x10, x | y);
            m.sw(p + 0x24, (x + 0x10) | y);
            m.sw(p + 0x44, (x + 0x20) | y);
            m.sw(p + 0x38, 0xe600_0000);
            m.sw(p + 4, 0xe600_0000);
            m.sw(p + 0x64, mask);
            m.sw(p + 0x30, mask);
            for word in [0x50, 0x3c, 0x1c, 8] {
                m.sw(p + word, 0x0100_0000);
            }
            for word in [0x54, 0x40, 0x20, 0xc] {
                m.sw(p + word, 0x8000_0000);
            }
            m.sw(p + 0x58, (x + 0x30) | y);
            m.sw(p + 0x14, 0x0100_0140);
            m.sw(p + 0x28, 0x0110_0140);
            m.sw(p + 0x48, 0x0120_0140);
            m.sw(p + 0x5c, 0x0130_0140);
            for word in [0x60, 0x4c, 0x2c, 0x18] {
                m.sw(p + word, 0x0010_0010);
            }
        } else {
            let bucket = (index << 2).wrapping_add(ot);
            let x = ((i & 6) << 3) + 0x140;
            let old = m.lw(bucket);
            m.sw(p, old | 0x0700_0000);
            t4 = p + 0x20;
            m.sw(bucket, link(p));
            let y = (((i & 8) << 3) + 0x180) << 16;
            m.sw(p + 4, 0xe600_0000);
            m.sw(p + 0x1c, mask);
            m.sw(p + 8, 0x0100_0000);
            m.sw(p + 0xc, 0x8000_0000);
            m.sw(p + 0x10, x | y);
            m.sw(p + 0x14, 0x0100_0140);
            m.sw(p + 0x18, 0x0040_0010);
        }
        buckets = buckets.wrapping_add(2);
    }
    let p = t4;
    let tile = p + 0x14;
    let restore = p + 0x2c;
    m.sw(p + 0xc, 0xe508_0140);
    m.sw(p + 8, 0xe404_fd4f);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(p, link(tile) | 0x0400_0000);
    m.sw(p + 4, 0xe304_0140);
    m.sw(p + 0x10, 0);
    let params = m.lw(gameplay + 0x244);
    let color = m.lw(params + 8);
    m.sw(p + 0x14, link(restore) | 0x0500_0000);
    m.sw(tile + 4, 0xe600_0001);
    m.sw(tile + 0xc, 0);
    m.sw(tile + 0x10, 0x0040_0010);
    m.sw(tile + 0x14, mask);
    m.sw(tile + 8, link(color) | 0x6000_0000);
    let origin = m.lw(rect);
    m.sw(restore, 0x0400_0000);
    let x = sext16(origin);
    let y = sra(origin, 16);
    m.sw(restore + 4, (x & 0x3ff) | ((y & 0x3ff) << 10) | 0xe300_0000);
    let size = m.lw(rect + 4);
    let right = x.wrapping_add(sext16(size)).wrapping_sub(1);
    let bottom = y.wrapping_add(sra(size, 16)).wrapping_sub(1);
    let offset = m.lw(offset);
    m.sw(
        restore + 8,
        (right & 0x3ff) | ((bottom & 0x3ff) << 10) | 0xe400_0000,
    );
    let index = lh(m, buckets);
    m.sw(restore + 0x10, 0);
    m.sw(
        restore + 0xc,
        (offset & 0x7ff) | (sra(offset, 5) & 0x003f_f800) | 0xe500_0000,
    );
    let bucket = (index << 2).wrapping_add(ot);
    let tag = m.lw(restore);
    let old = m.lw(bucket);
    m.sw(restore, (tag & 0xff00_0000) | link(old));
    let old = m.lw(bucket);
    m.sw(bucket, (old & 0xff00_0000) | link(p));
    m.pop_frame(0x10);
    p + 0x40
}

/// 800939D4 (JP-only, `new`, no caller reached): letterbox bars. For
/// `value > 0` (clamped: 0x1000 at or above 0x1000, else the halfword at
/// 0x800B7104 + `value & !3`), adds in front of the last bucket
/// (`ot + 0x3FFC`) a draw area of the band between the bars of the
/// display buffer `[[gp+0x2EC]+4]` (y 0 or 0x100), and in front of
/// `ot + 4` the full-screen draw area and two black TILEs: the top bar
/// `value * 2 / 256` lines and the bottom `value * 3 / 256` lines high
/// from y `0xF0 -` its height. Returns the next packet address
/// (`packets` unchanged for `value <= 0`).
pub fn letterbox_939d4(m: &mut Machine, ot: u32, packets: u32, value: u32) -> u32 {
    if lt(0, value) {
        let value = if lt(value, 0x1000) {
            let entry = (sra(value, 2) << 2).wrapping_add(LETTERBOX_HEIGHTS);
            lh(m, entry)
        } else {
            0x1000
        };
        let top = sra(value << 5, 12); // a2
        let bottom = sra(((value << 1).wrapping_add(value)) << 4, 12); // a1
        let start = 0xf0_u32.wrapping_sub(bottom); // t0
        let context = m.gw(CONTEXT);
        let bottom_size = (bottom << 16) | 0x140;
        let buffer = m.lw(context + 4);
        m.sw(packets, 0x0200_0000);
        let base = u32::from(buffer != 0) << 8; // a0
        let first = base.wrapping_add(top);
        m.sw(packets + 4, ((first & 0x3ff) << 10) | 0xe300_0000);
        let last = first.wrapping_add(start.wrapping_sub(top)).wrapping_sub(1);
        m.sw(packets + 8, ((last & 0x3ff) << 10) | 0xe400_013f);
        let tiles = packets + 0x18;
        let top_size = (top << 16) | 0x140;
        let old = m.lw(ot + 0x3ffc);
        m.sw(packets, old | 0x0200_0000);
        m.sw(ot + 0x3ffc, link(packets));
        let area = packets + 0xc;
        m.sw(packets + 0xc, link(tiles) | 0x0200_0000);
        m.sw(area + 4, (base << 10) | 0xe300_0000);
        m.sw(area + 8, ((base + 0xef) << 10) | 0xe400_013f);
        m.sw(packets + 0x18, 0x0600_0000);
        m.sw(tiles + 4, 0x6000_0000);
        m.sw(tiles + 0x10, 0x6000_0000);
        sh(m, tiles + 0x14, 0);
        sh(m, tiles + 8, 0);
        sh(m, tiles + 0xa, 0);
        sh(m, tiles + 0x16, start);
        m.sw(tiles + 0xc, top_size);
        m.sw(tiles + 0x18, bottom_size);
        let old = lwl(m, 0, ot + 6);
        swl(m, old, tiles + 2);
        swl(m, area << 8, ot + 6);
        packets + 0x34
    } else {
        packets
    }
}

/// Dispatch claims of the JP-only terrain routines.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8009_39d4 => letterbox_939d4(m, a0, a1, a2),
        0x8009_43a8 => fog_943a8(m, a0, a1, a2, a3),
        _ => return None,
    })
}
