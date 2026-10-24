//! Terrain, fog and sky rendering: the 800331D4 tree (called from 80033748).
//!
//! Every routine is translated from the original instructions over the
//! address-preserving [`Machine`]: structure and pointer values keep their
//! original addresses, GTE work goes through the Redux-exact `ac3_gte`
//! register file, and primitives/ordering-table links are written to RAM
//! exactly as the original writes them.
//!
//! Stack frames whose addresses the original passes to callees (or stores,
//! like 8005CFB4's two vertex-row buffers kept in scratchpad 0x1F80000C and
//! 0x1F800010) live in machine RAM at their original offsets
//! (`Machine::push_frame`). Every routine with a frame that calls another
//! routine pushes its original frame size, so stack addresses stay exact.
//! Pure register spills stay in Rust. Those scratchpad words match the
//! original only when `Machine::sp` at 800331D4's entry is the original
//! `$sp` (0x801FFF98 in the captured flight frames), so the callers must push
//! their original frames too.
//!
//! Compiler-emitted division checks (`break 7` for a zero divisor, `break 6`
//! for `MIN / -1`) have no native trap handler: they raise [`Fault::Trap`]
//! with the `break` address and code and continue with the hardware
//! quotient.
//!
//! The libgte routines of the tree (NormalClip, rcos, MulMatrix0,
//! SetMulMatrix 800A4ED8) and printf are `sdk.rs`'s.
use crate::common::{CONTEXT, GAMEPLAY};
use crate::machine::{Fault, Machine, SmallData};
use crate::{sdk, shared};
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
/// `srlv`.
fn srlv(value: u32, shift: u32) -> u32 {
    value >> (shift & 31)
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

// ---- 800331D4: the world/background pass of the flight frame -------------------------

/// 800331D4: terrain (one or two views), the depth masks and animated
/// palette when enabled, the depth fog, and the sky of the mission's
/// background type (gameplay +0x244 +2, low three bits). Every stage
/// advances the primitive pointer at gameplay +0x14. Returns the original
/// `v0` (the last stage's primitive pointer, or 1/4 without a sky).
pub fn render_world(m: &mut Machine) -> u32 {
    let sp = m.push_frame(0x148);
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
    if !m.lw(gameplay) & 0x100 != 0 {
        build_view(m, camera + 4, angles, camera + 0x1c, view);
        terrain_pass(m, view);
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
        build_view(m, position, angles, camera + 0x1c, view);
        terrain_pass(m, view);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay) & 4 != 0 {
        let ot = m.lw(gameplay + 0x10).wrapping_add(4);
        let packets = m.lw(gameplay + 0x14);
        let next = depth_masks(m, ot, packets);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, next);
    }
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244);
    let count = m.lw(params + 0x74);
    if count != 0 {
        let ot = m.lw(gameplay + 0x10).wrapping_add(4);
        let packets = m.lw(gameplay + 0x14);
        let next = animate_palette(m, ot, packets, params + 0x78, count);
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
        let environments = m.gw(0x2d4);
        let rect = m.lw(environments.wrapping_add(index));
        let next = draw_fog(m, ot.wrapping_add(4), packets, rect, gameplay + 0x298);
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
            draw_sky(m, ot, packets)
        }
        2 => {
            let packets = m.lw(gameplay + 0x14);
            draw_cloud_sky(m, sp + 0x10, ot, packets)
        }
        3 => {
            let packets = m.lw(gameplay + 0x14);
            draw_dome_sky(m, ot, packets)
        }
        4 => {
            let packets = m.lw(gameplay + 0x14);
            draw_night_sky(m, ot, packets)
        }
        0 => return 1,
        _ => return 4,
    };
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
    next
}

/// 800332A0..800332BC: load the camera matrix (inline SetRotMatrix and
/// SetTransMatrix of gameplay +0x4C) and draw the terrain of `view`.
fn terrain_pass(m: &mut Machine, view: u32) {
    let gameplay = m.gw(GAMEPLAY);
    set_rotation(m, gameplay + 0x4c);
    sdk::set_trans_matrix(m, gameplay + 0x4c);
    let ot = m.lw(gameplay + 0x10).wrapping_add(4);
    let packets = m.lw(gameplay + 0x14);
    let next = draw_terrain(m, gameplay + 0x244, view, ot, packets);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
}

/// 8005D824: 16 depth-mask packets (the 24-byte template at 80024040)
/// prepended to the first 16 depth buckets of gameplay +0x288. Returns the
/// primitive pointer.
pub fn depth_masks(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    let template: [u32; 6] = std::array::from_fn(|i| m.lw(0x8002_4040 + i as u32 * 4));
    let gameplay = m.gw(GAMEPLAY);
    let mut buckets = m.lw(gameplay + 0x288);
    let mut packet = packets;
    for _ in 0..16 {
        let depth = lh(m, buckets);
        buckets = buckets.wrapping_add(2);
        m.sw(packet, template[0]);
        m.sw(packet + 4, template[1]);
        let entry = (depth << 2).wrapping_add(ot);
        m.sw(packet + 0xc, template[3]);
        m.sw(packet + 0x10, template[4]);
        m.sw(packet + 0x14, template[5]);
        m.sw(packet + 8, template[2]);
        let head = m.lw(packet) & 0xff00_0000;
        let next = m.lw(entry) & 0x00ff_ffff;
        m.sw(packet, head | next);
        let top = m.lw(entry) & 0xff00_0000;
        m.sw(entry, top | (packet & 0x00ff_ffff));
        packet = packet.wrapping_add(0x18);
    }
    packet
}

/// 8005D5FC: animated palette entries (mission parameters +0x74 count,
/// +0x78 12-byte entries): INTPL toward the entry colour by a wave from the
/// table at 80010498, then 16 depth-cued single-pixel VRAM writes (GP0 A0)
/// into the CLUT, one per depth bucket. Returns the primitive pointer.
pub fn animate_palette(m: &mut Machine, ot: u32, packets: u32, entries: u32, count: u32) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244);
    let fog = params + 8;
    let fog_r = lbu(m, params + 8) << 4; // s7
    let fog_g = lbu(m, fog + 1) << 4; // s6
    let context = m.gw(CONTEXT);
    let clock = m.lw(context + 0x28); // s4
    let fog_b = lbu(m, fog + 2) << 4; // s5
    let mut packet = packets; // a1
    if !lt(0, count) {
        return packet;
    }
    let mut entry = entries.wrapping_add(4); // a2
    let mut position = entries; // t9
    let mut index = 0_u32;
    while lt(index, count) {
        let irgb = lh(m, entry + 2);
        m.gte.mtc2(reg::IRGB, irgb);
        let r = lbu(m, entry + 4) << 4;
        let b = lbu(m, entry + 6) << 4;
        let g = lbu(m, entry + 5) << 4;
        far_color(m, r, g, b);
        let shift = lh(m, entry.wrapping_sub(2));
        let phase = lh(m, entry);
        let t1 = sllv(clock, shift).wrapping_add(phase);
        let wave = lhu(m, ((t1 & 0x3fff) << 1).wrapping_add(0x8001_0498));
        let t1 = sra(wave << 16, 19).wrapping_add(0x800);
        m.gte.mtc2(reg::IR0, t1);
        m.gte.intpl(true, false);
        let gameplay = m.gw(GAMEPLAY);
        let mut depth = 0_u32; // t1
        let mut buckets = m.lw(gameplay + 0x288); // t3
        let location = lhu(m, position);
        let flag = r & 0x8000; // t8 (a0 holds the red channel here)
        let x = (location & 0xf) + 0x140;
        let y = sra(location << 16, 20).wrapping_add(0x100);
        let color = m.gte.mfc2(reg::RGB2);
        far_color(m, fog_r, fog_g, fog_b);
        m.gte.mtc2(reg::RGBC, color);
        let xy = (y << 16) | x; // t6
        for _ in 0..16 {
            m.gte.mtc2(reg::IR0, depth);
            m.gte.dpcs(true, false);
            let bucket = (lh(m, buckets) << 2).wrapping_add(ot);
            let pixel = m.gte.mfc2(reg::ORGB) | flag;
            m.sw(packet, 0x0500_0000);
            m.sw(packet + 0x10, pixel);
            m.sw(packet + 4, 0xa000_0000);
            m.sw(packet + 8, xy);
            m.sw(packet + 0xc, 0x0001_0001);
            m.sw(packet + 0x14, 0x0100_0000);
            depth = depth.wrapping_add(0x100);
            buckets = buckets.wrapping_add(2);
            let head = m.lw(packet) & 0xff00_0000;
            let next = m.lw(bucket) & 0x00ff_ffff;
            m.sw(packet, head | next);
            let top = m.lw(bucket) & 0xff00_0000;
            m.sw(bucket, top | (packet & 0x00ff_ffff));
            packet = packet.wrapping_add(0x18);
        }
        index = index.wrapping_add(1);
        entry = entry.wrapping_add(0xc);
        position = position.wrapping_add(0xc);
    }
    packet
}

/// Native targets owned by this module. Every translated routine is claimed
/// by its original address with the original `a0..a3`; the result is the
/// original `v0` (0 for routines whose `v0` is not meaningful).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8003_31d4 => render_world(m),
        0x8005_cda4 => select_cells(m, a0, a1, a2),
        0x8005_cfb4 => draw_terrain(m, a0, a1, a2, a3),
        0x8005_d5fc => animate_palette(m, a0, a1, a2, a3),
        0x8005_d824 => depth_masks(m, a0, a1),
        0x8005_d958 => max_x_index(m, a0, a1),
        0x8005_d9a8 => min_x_index(m, a0, a1),
        0x8005_d9f8 => corners_same_side(m, a0, a1),
        0x8005_dae8 => clip_segment(m, a0, a1, a2, a3),
        0x8005_dd44 => plane_intersection(m, a0, a1, a2, a3),
        0x8005_df68 => unproject(m, a0, a1, a3),
        0x8005_e030 => project_vector(m, a0, a1),
        0x8005_e140 => {
            view_frustum(m, a0, a1, a2, a3);
            0
        }
        0x8005_f4ec => {
            make_grid(m, a0);
            0
        }
        0x8005_fd68 => {
            view_tiles(m, a0);
            0
        }
        0x8005_fe48 => {
            build_view(m, a0, a1, a2, a3);
            0
        }
        0x8005_ff9c => {
            draw_cells(m, a0, a1, a2);
            0
        }
        0x8006_0214 => {
            mesh_flat_quads(m, a0);
            0
        }
        0x8006_0470 => {
            mesh_gouraud_quads(m, a0);
            0
        }
        0x8006_0684 => {
            midpoint(m, a0, a1, a2);
            0
        }
        0x8006_0708 => {
            emit_triangle(m, a0, a1, a2, a3);
            0
        }
        0x8006_07dc => subdivide_level(m, a0, a1, a2, a3),
        0x8006_0dc0 => subdivide_quad(m, a0, a1),
        0x8009_2bcc => draw_fog(m, a0, a1, a2, a3),
        0x8009_2e50 => {
            sky_camera(m, a0, a1);
            0
        }
        0x8009_2f90 => {
            project_bands(m, a0, a1);
            0
        }
        0x8009_309c => {
            classify_corners(m, a0);
            0
        }
        0x8009_31b8 => sky_bands(m, a0, a1),
        0x8009_33cc => {
            sky_colors(m, a0, a1);
            0
        }
        0x8009_35d8 => {
            sky_altitude_color(m, a0, a1);
            0
        }
        0x8009_36a0 => {
            blend_color(m, a0, a1, a2);
            0
        }
        0x8009_36fc => {
            draw_bands(m, a0, a1);
            0
        }
        0x8009_3920 => {
            flat_band(m, a0, a1, a2);
            0
        }
        0x8009_3af4 => {
            gradient_band(m, a0, a1, a2);
            0
        }
        0x8009_3e2c => draw_sky(m, a0, a1),
        0x8009_3fb8 => {
            draw_sun(m, a0, a1);
            0
        }
        0x8009_4300 => {
            draw_stars(m, a0, a1, a2);
            0
        }
        0x8009_45e8 => {
            cloud_sky_bands(m, a0, a1);
            0
        }
        0x8009_47cc => {
            draw_cloud_bands(m, a0, a1);
            0
        }
        0x8009_4900 => draw_cloud_sky_bands(m, a0, a1),
        0x8009_4a40 => quad_too_large(a0, a1, a2, a3),
        0x8009_4b60 => {
            cloud_midpoint(m, a0, a1, a2);
            0
        }
        0x8009_4c1c => screen_outcode(m, a0),
        0x8009_4c78 => {
            subdivide_cloud_quad(m, a0, a1, a2, a3);
            0
        }
        0x8009_5090 => draw_cloud_layer(m, a0, a1, a2),
        0x8009_5b94 => draw_cloud_sky(m, a0, a1, a2),
        0x8009_5bc4 => {
            dome_ring(m, a0, a1, a2);
            0
        }
        0x8009_5d40 => {
            draw_dome(m);
            0
        }
        0x8009_64fc => draw_dome_sky(m, a0, a1),
        0x8009_668c => draw_night_sky(m, a0, a1),
        0x8009_6954 => {
            night_ring(m, a0, a1, a2);
            0
        }
        0x8009_6a5c => {
            night_dome(m, a0, a1);
            0
        }
        0x8009_6d5c => {
            night_ground_row(m, a0, a1, a2);
            0
        }
        0x8009_6e98 => {
            night_ground(m, a0);
            0
        }
        0x8008_3330 => {
            compact_ot_runs(m, a0, a1);
            0
        }
        0x8008_e238 => fade_overlay(m, a0, a1, a2, a3),
        _ => return None,
    })
}

// ---- terrain: 8005CFB4 and its mesh renderer ------------------------------------------

/// 8005CFB4: draw the visible terrain. `params` is the mission pointer array
/// at gameplay +0x244 (+4 grid, +8 geometry directory), `view` the 8005FE48
/// descriptor, `ot` the depth OT (+4), `packets` the primitive pointer.
/// Returns the advanced primitive pointer.
pub fn draw_terrain(m: &mut Machine, params: u32, view: u32, ot: u32, packets: u32) -> u32 {
    let sp = m.push_frame(0x128);
    let grid = m.lw(params + 4);
    let end = select_cells(m, grid, view, packets);
    let work = SCRATCH;
    m.sw(work + 4, end);
    m.sw(work + 0xc, sp + 0x10);
    let x = m.lw(0x800b_aef0);
    m.sw(work + 0x10, sp + 0x90);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(work + 0x14, x.wrapping_add(0x7c00));
    let z = m.lw(0x800b_aef4);
    m.sw(work, ot);
    m.sw(work + 0x1c, z.wrapping_add(0x7c00));
    let y = m.lw(gameplay + 0x64);
    m.sw(work + 0x18, y);
    sdk::apply_rot_matrix_lv(m, work + 0x14, work + 0x14);
    let directory = m.lw(params + 8);
    draw_cells(m, packets, directory, work);
    let packet = m.lw(work + 4);
    sb(m, packet + 3, 1);
    let packet = m.lw(work + 4);
    m.sw(packet + 4, 0xe100_0228);
    let next = m.lw(ot + 0x3ff8) | 0x0100_0000;
    let packet = m.lw(work + 4);
    m.sw(packet, next);
    let packet = m.lw(work + 4);
    m.sw(ot + 0x3ff8, packet & 0x00ff_ffff);
    let packet = m.lw(work + 4).wrapping_add(8);
    m.sw(work + 4, packet);
    m.pop_frame(0x128);
    packet
}

/// 8005CDA4: select the visible cells of `view` from the 256x256 signed
/// halfword grid into 12-byte records at `out`: x, -height and z halfwords
/// (the padding halfword is left untouched), then the descriptor word
/// (signed cell, column << 16, first-cell / previous-row stitch bits). A
/// record with descriptor 0x200000 ends the list; returns the address after
/// it.
pub fn select_cells(m: &mut Machine, grid: u32, view: u32, out: u32) -> u32 {
    let mut out = out;
    let mut previous = 0_u32; // s5
    let mut occupied = 0_u32; // t7
    let fraction_x = m.lw(view + 0x40).wrapping_sub(0x400); // s4
    let fraction_z = m.lw(view + 0x44).wrapping_sub(0x400); // s6
    let mut row_z = m.lw(view + 0x4c); // t9
    let height = m.lw(view + 4); // s8
    let tile_x = m.lw(view + 0x38);
    let rows = m.lw(view + 0x5c);
    let tile_z = m.lw(view + 0x3c);
    if lt(0, rows) {
        let mut row = 0_u32; // a0
        let mut masks = view; // s0
        loop {
            let z = (row_z << 11).wrapping_sub(fraction_z); // t6
            let mut first = u32::MAX; // t8
            let columns = m.lw(view + 0x58);
            let mut column_x = m.lw(view + 0x48); // t4
            let mut column = 0_u32; // t1
            if lt(0, columns) {
                let t5 = tile_z.wrapping_add(row_z);
                let s2 = sext16(t5);
                let s1 = t5 & 7;
                loop {
                    let mask = m.lw(masks + 0x60);
                    let bit = sllv(1, column);
                    if mask & bit != 0 {
                        let mut cz = t5;
                        let a3 = tile_x.wrapping_add(column_x);
                        let mut cx = a3;
                        if lt(a3 << 16, 0) {
                            cx = a3 & 7;
                        }
                        if lt(s2, 0) {
                            cz = s1;
                        }
                        if !lt(sext16(cx), 0x100) {
                            cx = (cx & 7) | 0xf8;
                        }
                        if !lt(sext16(cz), 0x100) {
                            cz = (cz & 7) | 0xf8;
                        }
                        let address =
                            (sext16(cz) << 9).wrapping_add((sext16(cx) << 1).wrapping_add(grid));
                        let cell = lh(m, address);
                        if cell & 0x1ff != 0 {
                            occupied |= bit;
                            let stitch = if lt(first, 0) {
                                first = column;
                                0x0040_0000
                            } else if srlv(previous, column.wrapping_sub(1)) & 3 == 3 {
                                0x0e00_0000
                            } else {
                                0x0800_0000
                            };
                            let descriptor = cell | (column << 16) | stitch;
                            sh(m, out, (column_x << 11).wrapping_sub(fraction_x));
                            sh(m, out + 2, height.wrapping_neg());
                            sh(m, out + 4, z);
                            m.sw(out + 8, descriptor);
                            out = out.wrapping_add(12);
                        }
                    }
                    column = column.wrapping_add(1);
                    column_x = column_x.wrapping_add(1);
                    let columns = m.lw(view + 0x58);
                    if !lt(column, columns) {
                        break;
                    }
                }
            }
            previous = occupied;
            occupied = 0;
            row_z = row_z.wrapping_add(1);
            row = row.wrapping_add(1);
            masks = masks.wrapping_add(4);
            let rows = m.lw(view + 0x5c);
            if !lt(row, rows) {
                break;
            }
        }
    }
    m.sw(out + 8, 0x0020_0000);
    out.wrapping_add(12)
}

/// 8005FF9C: transform and draw the meshes of the selected cells. `work` is
/// the scratchpad block (0x1F800000): +0 OT, +4 primitive pointer, +8 face
/// list, +0xC/+0x10 previous/current row seam buffers, +0x14 camera
/// translation, +0x20.. projected vertices (16 bytes each), +0x230 vertex
/// data.
pub fn draw_cells(m: &mut Machine, cells: u32, directory: u32, work: u32) {
    m.push_frame(0x28);
    far_color(m, 0x830, 0x830, 0x830);
    let mut cell = cells;
    loop {
        let descriptor = m.lw(cell + 8);
        if !descriptor & 0x0020_0000 == 0 {
            break;
        }
        let mesh = m.lw(((descriptor & 0x1ff) << 2).wrapping_add(directory));
        // SetTransVector(work + 0x14)
        let tx = m.lw(work + 0x14);
        let ty = m.lw(work + 0x18);
        let tz = m.lw(work + 0x1c);
        m.gte.ctc2(ctrl::TRX, tx);
        m.gte.ctc2(ctrl::TRY, ty);
        m.gte.ctc2(ctrl::TRZ, tz);
        lwc2(m, reg::VXY0, cell);
        lwc2(m, reg::VZ0, cell + 4);
        if descriptor & 0x0040_0000 != 0 {
            let previous = m.lw(work + 0xc);
            let current = m.lw(work + 0x10);
            m.sw(work + 0x10, previous);
            m.sw(work + 0xc, current);
        }
        m.gte.mvmva(true, false, 0, 0, 0);
        let mut t4 = work.wrapping_sub(0x10);
        let t3 = work + 0x20;
        let column = (descriptor >> 16) & 0x1f;
        let vertices = m.lw(mesh + 4);
        let mut t5 = m.lw(t3 + 0x30);
        let mut a1 = vertices.wrapping_add(4);
        let mut count = m.lw(vertices);
        let x = m.gte.mfc2(reg::MAC1);
        let y = m.gte.mfc2(reg::MAC2);
        m.gte.ctc2(ctrl::TRX, x);
        let z = m.gte.mfc2(reg::MAC3);
        m.gte.ctc2(ctrl::TRY, y);
        m.gte.ctc2(ctrl::TRZ, z);
        let mut v1 = vertices.wrapping_add(0x14);
        if !lt(z, 0x7001) {
            // Far cell: project only the centre vertex for the seam buffer.
            lwc2(m, reg::VXY0, a1 + 0x14);
            lwc2(m, reg::VZ0, a1 + 0x20);
            m.gte.rtps(true, false);
            swc2(m, reg::SXY2, t3 + 0x30);
            let current = m.lw(work + 0x10);
            let value = m.lw(t3 + 0x30);
            m.sw((column << 2).wrapping_add(current), value);
        } else {
            let mut t1 = m.lw(vertices.wrapping_add(4));
            let mut t0 = m.lw(a1 + 4);
            let mut a3 = m.lw(a1 + 8);
            let mut vz = m.lw(a1 + 0xc);
            let mut a2 = m.lw(a1 + 0x10);
            let mut a0 = vz >> 16;
            loop {
                m.gte.mtc2(reg::VXY0, t1);
                m.gte.mtc2(reg::VXY1, t0);
                m.gte.mtc2(reg::VXY2, a3);
                m.gte.mtc2(reg::VZ0, vz);
                m.gte.mtc2(reg::VZ1, a0);
                m.gte.mtc2(reg::VZ2, a2);
                v1 = v1.wrapping_add(0x14);
                a1 = a1.wrapping_add(0x14);
                m.gte.rtpt(true, false);
                count = count.wrapping_sub(3);
                t4 = t4.wrapping_add(0x30);
                t1 = m.lw(a1);
                t0 = m.lw(v1.wrapping_sub(0xc));
                a3 = m.lw(v1.wrapping_sub(8));
                vz = m.lw(v1.wrapping_sub(4));
                a2 = m.lw(v1);
                a0 = vz >> 16;
                swc2(m, reg::SXY0, t4);
                swc2(m, reg::SZ1, t4 + 4);
                swc2(m, reg::IR0, t4 + 8);
                swc2(m, reg::SXY1, t4 + 0x10);
                swc2(m, reg::SZ2, t4 + 0x14);
                swc2(m, reg::IR0, t4 + 0x18);
                swc2(m, reg::SXY2, t4 + 0x20);
                swc2(m, reg::SZ3, t4 + 0x24);
                swc2(m, reg::IR0, t4 + 0x28);
                if !lt(0, count) {
                    break;
                }
            }
            let mut flags = descriptor;
            flags ^= flags & (flags << 16);
            if flags & 0x0800_0000 != 0 {
                m.sw(t3 + 0x20, t5);
            }
            if flags & 0x0200_0000 != 0 {
                let previous = m.lw(work + 0xc);
                t5 = m.lw((column << 2).wrapping_add(previous).wrapping_sub(4));
                m.sw(t3, t5);
            }
            if flags & 0x0400_0000 != 0 {
                let previous = m.lw(work + 0xc);
                t5 = m.lw((column << 2).wrapping_add(previous));
                m.sw(t3 + 0x10, t5);
            }
            let current = m.lw(work + 0x10);
            let value = m.lw(t3 + 0x30);
            m.sw((column << 2).wrapping_add(current), value);
            let base = m.lw(mesh + 4).wrapping_add(4);
            m.sw(work + 0x230, base);
            let faces = m.lw(mesh + 0x10);
            m.sw(work + 8, faces);
            mesh_gouraud_quads(m, work);
            let faces = m.lw(mesh + 0xc);
            m.sw(work + 8, faces);
            mesh_flat_quads(m, work);
        }
        cell = cell.wrapping_add(0xc);
    }
    m.pop_frame(0x28);
}

/// Shared face test of 80060214/80060470: back-face cull with NCLIP over
/// (v0, v1, v2) and then (v3, v1, v2), and the depth limit on the largest
/// of the four SZ words. Returns `(sxy0, depth >> 3)` for a drawable face,
/// `None` for a culled one. `t2/v1/t1/a0` are the four vertex slots.
fn face_test(m: &mut Machine, slots: [u32; 4]) -> Option<(u32, u32)> {
    let [t2, v1, t1, a0] = slots;
    let sxy0 = m.lw(t2);
    lwc2(m, reg::SXY1, v1);
    lwc2(m, reg::SXY2, t1);
    m.gte.mtc2(reg::SXY0, sxy0);
    let sxy3 = m.lw(a0);
    m.gte.nclip();
    let mut depth = m.lw(v1 + 4);
    let z0 = m.lw(t2 + 4);
    if lt(depth, z0) {
        depth = z0;
    }
    let mut d = m.lw(a0 + 4);
    let z2 = m.lw(t1 + 4);
    if lt(d, z2) {
        d = z2;
    }
    if lt(d, depth) {
        d = depth;
    }
    let area = m.gte.mfc2(reg::MAC0);
    m.gte.mtc2(reg::SXY0, sxy3);
    if !lt(0, area) {
        m.gte.nclip();
        let area = m.gte.mfc2(reg::MAC0);
        if !lt(area, 0) {
            return None;
        }
    }
    if d.wrapping_sub(0x79) >= 0x777f {
        return None;
    }
    Some((sxy0, sra(d, 3)))
}

/// Decode the byte-packed vertex indices of a face into four 16-byte vertex
/// slots of the projected-vertex area (`work + 0x20`).
fn face_slots(base: u32, indices: u32) -> [u32; 4] {
    let a = indices & 0xff;
    let b = (indices >> 8) & 0xff;
    let c = (indices >> 16) & 0xff;
    let d = indices >> 24;
    [
        base + (a << 4),
        base + (b << 4),
        base + (c << 4),
        base + (d << 4),
    ]
}

/// 80060214: flat-shaded textured quads (POLY_FT4) of a mesh face list
/// (`work + 8`: count, then 0x24-byte faces). Near faces are subdivided
/// through 80060DC0 with a Gouraud record built at `work + 0x234`.
pub fn mesh_flat_quads(m: &mut Machine, work: u32) {
    m.push_frame(0x38);
    let s2 = work + 0x20;
    let list = m.lw(work + 8);
    let mut packet = m.lw(work + 4); // t0
    let ot = m.lw(work);
    let count = m.lw(list);
    let mut face = list.wrapping_add(4); // s3
    if lt(0, count) {
        let record = work + 0x234; // s1
        let mut s0 = list.wrapping_add(0x24);
        let mut index = 0_u32;
        loop {
            let indices = m.lw(s0.wrapping_sub(0x10));
            let slots = face_slots(s2, indices);
            if let Some((sxy0, depth)) = face_test(m, slots) {
                if !lt(depth, 0x38f) {
                    lwc2(m, reg::IR0, slots[0] + 8);
                    lwc2(m, reg::RGBC, face + 0x20);
                    let bucket = (depth << 2).wrapping_add(ot);
                    m.sw(packet + 8, sxy0);
                    let head = m.lw(bucket) | 0x0900_0000;
                    m.sw(packet, head);
                    m.sw(bucket, packet & 0x00ff_ffff);
                    m.gte.dpcs(true, false);
                    let uv2 = m.lw(s0.wrapping_sub(4));
                    let uv0 = m.lw(s0.wrapping_sub(0xc));
                    let uv1 = m.lw(s0.wrapping_sub(8));
                    m.sw(packet + 0x1c, uv2);
                    m.sw(packet + 0xc, uv0);
                    m.sw(packet + 0x14, uv1);
                    m.sw(packet + 0x24, uv2 >> 16);
                    swc2(m, reg::RGB2, packet + 4);
                    swc2(m, reg::SXY1, packet + 0x10);
                    swc2(m, reg::SXY2, packet + 0x18);
                    swc2(m, reg::SXY0, packet + 0x20);
                    packet = packet.wrapping_add(0x28);
                } else {
                    m.sw(work + 0x264, depth);
                    let first = m.lw(face);
                    m.sw(work + 0x234, first);
                    let a1 = m.lw(s0.wrapping_sub(0x18));
                    let a2 = m.lw(s0.wrapping_sub(0x14));
                    m.sw(record + 8, a1);
                    m.sw(record + 0xc, a2);
                    let a1 = m.lw(s0.wrapping_sub(0xc));
                    let a2 = m.lw(s0.wrapping_sub(8));
                    let a0 = m.lw(s0.wrapping_sub(4));
                    m.sw(record + 0x1c, a0);
                    m.sw(record + 0x14, a1);
                    m.sw(record + 0x18, a2);
                    let color = (m.lw(s0) & 0x00ff_ffff) | 0x3c00_0000;
                    for offset in [0x20, 0x24, 0x28, 0x2c] {
                        m.sw(record + offset, color);
                    }
                    m.sw(work + 4, packet);
                    packet = subdivide_quad(m, record, work);
                }
            }
            index = index.wrapping_add(1);
            s0 = s0.wrapping_add(0x24);
            face = face.wrapping_add(0x24);
            if !lt(index, count) {
                break;
            }
        }
    }
    m.sw(work + 4, packet);
    m.pop_frame(0x38);
}

/// 80060470: Gouraud-shaded textured quads (POLY_GT4) of a mesh face list
/// (`work + 8`: count, then 0x30-byte faces); near faces go to 80060DC0.
pub fn mesh_gouraud_quads(m: &mut Machine, work: u32) {
    m.push_frame(0x30);
    let s1 = work + 0x20;
    let list = m.lw(work + 8);
    let mut packet = m.lw(work + 4); // t0
    let ot = m.lw(work);
    let count = m.lw(list);
    let mut face = list.wrapping_add(4); // s2
    if lt(0, count) {
        let mut s0 = list.wrapping_add(0x1c);
        let mut index = 0_u32;
        loop {
            let indices = m.lw(s0.wrapping_sub(8));
            let slots = face_slots(s1, indices);
            if let Some((sxy0, depth)) = face_test(m, slots) {
                if !lt(depth, 0x38f) {
                    lwc2(m, reg::IR0, slots[0] + 8);
                    lwc2(m, reg::RGBC, face + 0x20);
                    lwc2(m, reg::RGB0, face + 0x24);
                    lwc2(m, reg::RGB1, face + 0x28);
                    lwc2(m, reg::RGB2, face + 0x2c);
                    m.sw(packet + 8, sxy0);
                    m.gte.dpct(true, false);
                    let bucket = (depth << 2).wrapping_add(ot);
                    let head = m.lw(bucket) | 0x0c00_0000;
                    m.sw(packet, head);
                    m.sw(bucket, packet & 0x00ff_ffff);
                    let uv23 = m.lw(s0 + 4);
                    let uv0 = m.lw(s0.wrapping_sub(4));
                    let uv1 = m.lw(s0);
                    swc2(m, reg::RGB0, packet + 0x10);
                    swc2(m, reg::RGB1, packet + 0x1c);
                    swc2(m, reg::RGB2, packet + 0x28);
                    m.sw(packet + 0xc, uv0);
                    m.sw(packet + 0x18, uv1);
                    m.gte.dpcs(true, false);
                    m.sw(packet + 0x24, uv23);
                    m.sw(packet + 0x30, uv23 >> 16);
                    swc2(m, reg::RGB2, packet + 4);
                    swc2(m, reg::SXY1, packet + 0x14);
                    swc2(m, reg::SXY2, packet + 0x20);
                    swc2(m, reg::SXY0, packet + 0x2c);
                    packet = packet.wrapping_add(0x34);
                } else {
                    m.sw(work + 0x264, depth);
                    m.sw(work + 4, packet);
                    packet = subdivide_quad(m, face, work);
                }
            }
            index = index.wrapping_add(1);
            s0 = s0.wrapping_add(0x30);
            face = face.wrapping_add(0x30);
            if !lt(index, count) {
                break;
            }
        }
    }
    m.sw(work + 4, packet);
    m.pop_frame(0x30);
}

/// 80060DC0: set up the four-vertex buffer at `work + 0x270` (12-byte
/// vertices: xy, z, uv, colour) for a near Gouraud face `record`, depth-cue
/// its colours and subdivide it (800607DC). Returns the primitive pointer.
pub fn subdivide_quad(m: &mut Machine, record: u32, work: u32) -> u32 {
    m.push_frame(0x18);
    let clut = lhu(m, record + 0x1a);
    sh(m, work + 0x268, clut);
    let tpage = lhu(m, record + 0x16);
    let a0 = work + 0x270;
    sh(m, work + 0x26a, tpage);
    lwc2(m, reg::RGBC, record + 0x20);
    lwc2(m, reg::RGB0, record + 0x24);
    lwc2(m, reg::RGB1, record + 0x28);
    lwc2(m, reg::RGB2, record + 0x2c);
    let mut indices = m.lw(record + 0xc);
    let base = m.lw(work + 0x230);
    let xy2 = base.wrapping_add(indices & 0xff);
    indices >>= 8;
    let z2 = base.wrapping_add(indices & 0xff);
    indices >>= 8;
    let i3 = indices & 0xff;
    let xy2 = m.lw(xy2);
    let z2 = lhu(m, z2);
    indices >>= 8;
    sh(m, a0 + 0x1c, z2);
    m.sw(a0 + 0x18, xy2);
    let xy3 = m.lw(base.wrapping_add(i3));
    let z3 = lhu(m, base.wrapping_add(indices));
    let mut indices = m.lw(record + 8);
    m.gte.mtc2(reg::VXY0, xy3);
    m.gte.mtc2(reg::VZ0, z3);
    m.sw(a0 + 0x24, xy3);
    sh(m, a0 + 0x28, z3);
    m.gte.rtps(true, false);
    let xy0 = m.lw(base.wrapping_add(indices & 0xff));
    indices >>= 8;
    let z0 = lhu(m, base.wrapping_add(indices & 0xff));
    indices >>= 8;
    m.sw(work + 0x270, xy0);
    sh(m, a0 + 4, z0);
    m.gte.dpct(true, false);
    let xy1 = m.lw(base.wrapping_add(indices & 0xff));
    indices >>= 8;
    let z1 = lhu(m, base.wrapping_add(indices));
    swc2(m, reg::RGB0, a0 + 0x14);
    swc2(m, reg::RGB1, a0 + 0x20);
    swc2(m, reg::RGB2, a0 + 0x2c);
    m.sw(a0 + 0xc, xy1);
    sh(m, a0 + 0x10, z1);
    m.gte.dpcs(true, false);
    let uv23 = m.lw(record + 0x1c);
    let uv0 = lhu(m, record + 0x14);
    let uv1 = lhu(m, record + 0x18);
    sh(m, a0 + 0x1e, uv23);
    sh(m, a0 + 6, uv0);
    sh(m, a0 + 0x12, uv1);
    sh(m, a0 + 0x2a, uv23 >> 16);
    swc2(m, reg::RGB2, a0 + 8);
    swc2(m, reg::SXY2, work + 0x2ac);
    let z = m.gte.mfc2(reg::SZ3);
    sh(m, work + 0x2b6, z);
    subdivide_level(m, a0, work, 4, 0);
    let packet = m.lw(work + 4);
    m.pop_frame(0x18);
    packet
}

/// 80060684: midpoint vertex of `a` and `b` into `out` (positions averaged,
/// colour and uv averaged with the 0xFEFEFEFF carry mask).
pub fn midpoint(m: &mut Machine, out: u32, a: u32, b: u32) {
    for offset in [0, 2, 4] {
        let value = sra(lh(m, a + offset).wrapping_add(lh(m, b + offset)), 1);
        sh(m, out + offset, value);
    }
    let mask = 0xfefe_feff;
    let color = sra((m.lw(a + 8) & mask).wrapping_add(m.lw(b + 8) & mask), 1);
    m.sw(out + 8, color);
    let uv = sra((lh(m, a + 6) & mask).wrapping_add(lh(m, b + 6) & mask), 1);
    sh(m, out + 6, uv);
}

/// 80060708: emit one POLY_GT3 of three subdivision vertices unless RTPT
/// reports an error flag.
pub fn emit_triangle(m: &mut Machine, a: u32, b: u32, c: u32, work: u32) {
    lwc2(m, reg::VXY0, a);
    lwc2(m, reg::VZ0, a + 4);
    lwc2(m, reg::VXY1, b);
    lwc2(m, reg::VZ1, b + 4);
    lwc2(m, reg::VXY2, c);
    lwc2(m, reg::VZ2, c + 4);
    let color = m.lw(a + 8);
    let packet = m.lw(work + 4);
    m.gte.rtpt(true, false);
    let color_b = m.lw(b + 8);
    m.sw(packet + 4, color);
    m.sw(packet + 0x10, color_b);
    let uv_c = lhu(m, c + 6);
    let color_c = m.lw(c + 8);
    sh(m, packet + 0x24, uv_c);
    m.sw(packet + 0x1c, color_c);
    let uv_a = lhu(m, a + 6);
    let uv_b = lhu(m, b + 6);
    sh(m, packet + 0xc, uv_a);
    sh(m, packet + 0x18, uv_b);
    let tpage = lh(m, work + 0x26a);
    let clut = lh(m, work + 0x268);
    sb(m, packet + 7, 0x34);
    sh(m, packet + 0xe, tpage);
    sh(m, packet + 0x1a, clut);
    let depth = m.lw(work + 0x264);
    let flag = m.gte.cfc2(ctrl::FLAG);
    if lt(flag, 0) {
        return;
    }
    let next = m.lw(work + 4).wrapping_add(0x28);
    m.sw(work + 4, next);
    let ot = m.lw(work);
    let bucket = (depth << 2).wrapping_add(ot);
    let head = m.lw(bucket) | 0x0900_0000;
    m.sw(packet, head);
    let ot = m.lw(work);
    let bucket = (depth << 2).wrapping_add(ot);
    m.sw(bucket, packet & 0x00ff_ffff);
    swc2(m, reg::SXY0, packet + 8);
    swc2(m, reg::SXY1, packet + 0x14);
    swc2(m, reg::SXY2, packet + 0x20);
}

/// 800607DC: one level of the recursive near-face subdivision. `quad` holds
/// four 12-byte vertices, +0x30.. their projections, +0x40.. their SZ.
/// Emits a POLY_GT4 when small enough, otherwise recurses into four
/// children at `quad + 0x48` (`bits` selects the split diagonal). Returns 1
/// when drawn or subdivided, 0 when rejected.
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
        if lt(y, 0xffff_ff80) {
            outside |= 0x100 << corner;
        } else if !lt(y, 0x81) {
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
        midpoint(m, s1, s0 + 0xc, s0 + 0x18);
    } else {
        midpoint(m, s1, s0, s0 + 0x24);
    }
    let s7 = s0 + 0x18;
    midpoint(m, s1 + 0xc, s0, s7);
    let s3 = s1 + 0x18;
    let s4 = s0 + 0xc;
    midpoint(m, s3, s0, s4);
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
        emit_triangle(m, s0, s4, s3, s6);
    }
    let s5 = s0 + 0x24;
    shift(m);
    midpoint(m, s3, s4, s5);
    if child(m, s4, s0 + 0x34, s0 + 0x42, s8.wrapping_add(3)) != 0 {
        emit_triangle(m, s4, s5, s3, s6);
    }
    shift(m);
    midpoint(m, s3, s7, s5);
    if child(m, s5, s0 + 0x3c, s0 + 0x46, s8) != 0 {
        emit_triangle(m, s5, s7, s3, s6);
    }
    shift(m);
    midpoint(m, s3, s0, s7);
    if child(m, s7, s0 + 0x38, s0 + 0x44, s8.wrapping_add(1)) != 0 {
        emit_triangle(m, s7, s0, s3, s6);
    }
    1
}

// ---- visibility: 8005FE48 and the frustum/grid routines -------------------------------

/// 8005FE48: build the terrain view descriptor (0x118 bytes at `view`) for
/// the camera at `position` (VECTOR, 20.12) with `angles` (SVECTOR) and
/// screen block `screen` (+4/+6 halfword width/height).
pub fn build_view(m: &mut Machine, position: u32, angles: u32, screen: u32, view: u32) {
    m.push_frame(0x18);
    view_frustum(m, screen, position, angles, view);
    if lt(0, m.lw(view + 0x18)) {
        let mut point = view;
        let mut entry = view;
        let mut index = 0_u32;
        loop {
            let x = lhu(m, point + 0x1c);
            sh(m, entry + 0xe0, x);
            let height = lhu(m, view + 4);
            index = index.wrapping_add(1);
            sh(m, entry + 0xe2, height.wrapping_neg());
            let z = lhu(m, point + 0x1e);
            point = point.wrapping_add(4);
            sh(m, entry + 0xe4, z);
            let count = m.lw(view + 0x18);
            entry = entry.wrapping_add(8);
            if !lt(index, count) {
                break;
            }
        }
    }
    view_tiles(m, view);
    make_grid(m, view);
    m.pop_frame(0x18);
}

/// 8005D958: index of the record with the largest first word among `count`
/// 16-byte records (first maximum wins).
pub fn max_x_index(m: &mut Machine, records: u32, count: u32) -> u32 {
    let mut best = 0_u32;
    let mut index = 1_u32;
    while lt(index, count) {
        let current = m.lw((best << 4).wrapping_add(records));
        let candidate = m.lw(records.wrapping_add(index << 4));
        if lt(current, candidate) {
            best = index;
        }
        index = index.wrapping_add(1);
    }
    best
}

/// 8005D9A8: index of the record with the smallest first word (first
/// minimum wins).
pub fn min_x_index(m: &mut Machine, records: u32, count: u32) -> u32 {
    let mut best = 0_u32;
    let mut index = 1_u32;
    while lt(index, count) {
        let current = m.lw((best << 4).wrapping_add(records));
        let candidate = m.lw(records.wrapping_add(index << 4));
        if lt(candidate, current) {
            best = index;
        }
        index = index.wrapping_add(1);
    }
    best
}

/// 8005D9F8: 1 when every edge of the `count` corner directions (VECTOR
/// records; x and z halved) turns the same way around the origin
/// (NormalClip all positive or all non-positive), else 0.
pub fn corners_same_side(m: &mut Machine, records: u32, count: u32) -> u32 {
    m.push_frame(0x38);
    let mut current = records;
    let mut positive = 0_u32;
    // Local SXY triple at sp+0x10: origin, previous corner, current corner.
    let origin = 0_u32;
    let last = (count << 4).wrapping_add(records);
    let x = sra(m.lw(last.wrapping_sub(0x10)), 1) & 0xffff;
    let z = sra(m.lw(last.wrapping_sub(8)), 1) & 0xffff;
    let mut corner = x | (z << 16);
    let mut index = 0_u32;
    while lt(index, count) {
        let previous = corner;
        let x = sra(m.lw(current), 1) & 0xffff;
        let z = sra(m.lw(current + 8), 1) & 0xffff;
        corner = x | (z << 16);
        if lt(0, sdk::normal_clip(m, origin, previous, corner)) {
            positive = positive.wrapping_add(1);
        }
        index = index.wrapping_add(1);
        current = current.wrapping_add(0x10);
    }
    m.pop_frame(0x38);
    u32::from(positive == 0 || positive == count)
}

/// 8005DAE8: Cohen-Sutherland clip of the screen segment `a`-`b` (halfword
/// x, y pairs, modified in place) to |x| <= `x_limit`, |y| <= `y_limit`, at
/// most eight steps. Returns 0 when rejected, 1 otherwise.
pub fn clip_segment(m: &mut Machine, a: u32, b: u32, x_limit: u32, y_limit: u32) -> u32 {
    let low_x = x_limit.wrapping_neg(); // t5
    let low_y = y_limit.wrapping_neg(); // t4
    let code = |m: &mut Machine, point: u32| -> u32 {
        let x = lh(m, point);
        let mut code = 0;
        if lt(x, low_x) {
            code = 1;
        } else if lt(x_limit, x) {
            code = 2;
        }
        let y = lh(m, point + 2);
        if lt(y, low_y) {
            code |= 4;
        } else if lt(y_limit, y) {
            code |= 8;
        }
        code
    };
    let mut t1 = a;
    let mut a1 = b;
    let mut code_a = code(m, t1);
    let mut code_b = code(m, a1);
    for _ in 0..8 {
        if code_a & code_b != 0 {
            return 0;
        }
        if code_a | code_b == 0 {
            return 1;
        }
        if code_a == 0 {
            std::mem::swap(&mut t1, &mut a1);
            code_b = 0;
        } else if code_a & 3 != 0 {
            let edge = if code_a & 1 != 0 { low_x } else { x_limit };
            let dy = lh(m, a1 + 2).wrapping_sub(lh(m, t1 + 2));
            let x0 = lh(m, t1);
            let product = dy.wrapping_mul(edge.wrapping_sub(x0));
            let dx = lh(m, a1).wrapping_sub(x0);
            let (step, _) = div(m, product, dx, 0x8005_dc1c);
            let y0 = lhu(m, t1 + 2);
            sh(m, t1, edge);
            sh(m, t1 + 2, y0.wrapping_add(step));
        } else if code_a & 0xc != 0 {
            let edge = if code_a & 4 != 0 { low_y } else { y_limit };
            let dx = lh(m, a1).wrapping_sub(lh(m, t1));
            let y0 = lh(m, t1 + 2);
            let product = dx.wrapping_mul(edge.wrapping_sub(y0));
            let dy = lh(m, a1 + 2).wrapping_sub(y0);
            let (step, _) = div(m, product, dy, 0x8005_dc9c);
            let x0 = lhu(m, t1);
            sh(m, t1 + 2, edge);
            sh(m, t1, x0.wrapping_add(step));
        }
        code_a = code(m, t1);
    }
    1
}

/// 8005DD44: intersect the ray `direction` (VECTOR) from `origin` (VECTOR)
/// with the horizontal plane y = `height`, limited to the direction's own
/// extent, into `out`. Returns 0 when the plane is behind, else whether the
/// result's y differs from the direction's y.
pub fn plane_intersection(
    m: &mut Machine,
    origin: u32,
    direction: u32,
    height: u32,
    out: u32,
) -> u32 {
    let origin_y = m.lw(origin + 4);
    let dir_y = m.lw(direction + 4);
    let distance = height.wrapping_sub(origin_y); // a0
    let z;
    if lt(0, dir_y) || lt(dir_y, 0) {
        if lt(0, dir_y) {
            if lt(height, origin_y) {
                return 0;
            }
        } else if lt(origin_y, height) {
            return 0;
        }
        let pc = if lt(0, dir_y) {
            0x8005_dd7c
        } else {
            0x8005_de28
        };
        let product = m.lw(direction).wrapping_mul(distance);
        let (x, _) = div(m, product, dir_y, pc);
        m.sw(out, x);
        let product = m.lw(direction + 8).wrapping_mul(distance);
        let divisor = m.lw(direction + 4);
        let (q, _) = div(m, product, divisor, pc + 0x4c);
        m.sw(out + 4, distance);
        z = q;
    } else {
        if height != origin_y {
            return 0;
        }
        let x = m.lw(direction);
        m.sw(out, x);
        z = m.lw(direction + 8);
        m.sw(out + 4, distance);
    }
    m.sw(out + 8, z);
    let dx = m.lw(direction);
    let ox = m.lw(out);
    let beyond = if lt(dx, 0) { lt(ox, dx) } else { lt(dx, ox) };
    if beyond {
        m.sw(out, dx);
        let y = m.lw(direction + 4);
        m.sw(out + 4, y);
    }
    let dz = m.lw(direction + 8);
    let oz = m.lw(out + 8);
    let beyond = if lt(dz, 0) { lt(oz, dz) } else { lt(dz, oz) };
    if beyond {
        m.sw(out + 8, dz);
        let y = m.lw(direction + 4);
        m.sw(out + 4, y);
    }
    let y = m.lw(out + 4);
    u32::from(y != m.lw(direction + 4))
}

/// 8005DF68: scale the screen point `point` (x, y, z halfwords) by z /
/// `divisor` and rotate it with the current matrix into `out` (VECTOR).
pub fn unproject(m: &mut Machine, point: u32, out: u32, divisor: u32) -> u32 {
    unproject_xy(m, point, out, divisor, divisor)
}

/// [`unproject`] with separate divisors for x and y (the same for the
/// original; widescreen widens x).
fn unproject_xy(m: &mut Machine, point: u32, out: u32, divisor_x: u32, divisor_y: u32) -> u32 {
    let sp = m.push_frame(0x28);
    let z = lh(m, point + 4);
    let product = lh(m, point).wrapping_mul(z);
    let (x, _) = div(m, product, divisor_x, 0x8005_df8c);
    m.sw(sp + 0x10, x);
    let z = lh(m, point + 4);
    let product = lh(m, point + 2).wrapping_mul(z);
    let (y, _) = div(m, product, divisor_y, 0x8005_dfdc);
    m.sw(sp + 0x14, y);
    let z = lh(m, point + 4);
    m.sw(sp + 0x18, z);
    let result = sdk::apply_rot_matrix_lv(m, sp + 0x10, out);
    m.pop_frame(0x28);
    result
}

/// 8005E030: perspective-project the VECTOR `vector` with the current
/// rotation, screen distance and offset into the halfword pair `out`.
pub fn project_vector(m: &mut Machine, vector: u32, out: u32) -> u32 {
    let sp = m.push_frame(0x38);
    let h = sdk::read_geom_screen(m);
    sdk::read_geom_offset(m, sp + 0x20, sp + 0x24);
    sdk::apply_rot_matrix_lv(m, vector, sp + 0x10);
    if m.lw(sp + 0x18) == 0 {
        m.sw(sp + 0x18, 1);
    }
    let product = m.lw(sp + 0x10).wrapping_mul(h);
    let z = m.lw(sp + 0x18);
    let (x, _) = div(m, product, z, 0x8005_e098);
    let x = lhu(m, sp + 0x20).wrapping_add(x);
    sh(m, out, x);
    let product = m.lw(sp + 0x14).wrapping_mul(h);
    let z = m.lw(sp + 0x18);
    let (y, _) = div(m, product, z, 0x8005_e0ec);
    let y = lhu(m, sp + 0x24).wrapping_add(y);
    sh(m, out + 2, y);
    m.pop_frame(0x38);
    y
}

/// 8005FD68: tile origin, fractional offset and clamped grid-relative
/// outline points of the view (uses the minimum stored by 8005E140 at
/// `gp+0x4FC/0x500`).
pub fn view_tiles(m: &mut Machine, view: u32) {
    let min_x = m.gw(0x4fc);
    let x = m.lw(view).wrapping_add(0x8000).wrapping_add(min_x); // t2
    let min_z = m.gw(0x500);
    let z = m.lw(view + 8).wrapping_add(0x8000).wrapping_add(min_z); // t0
    let tile_x = sra(x, 11);
    let fraction_x = x & 0x7ff;
    let tile_z = sra(z, 11);
    let fraction_z = z & 0x7ff;
    m.sw(view + 0x3c, tile_z.wrapping_add(0x80));
    let count = m.lw(view + 0x18);
    m.sw(view + 0x38, tile_x.wrapping_add(0x80));
    m.sw(view + 0x40, fraction_x.wrapping_sub(0x400));
    m.sw(view + 0x44, fraction_z.wrapping_sub(0x400));
    if !lt(0, count) {
        return;
    }
    let clamp = |value: u32| -> u32 {
        if lt(value, 0xffff_8000) {
            0xffff_8000
        } else if lt(value, 0x7800) {
            value
        } else {
            0x77ff
        }
    };
    let mut point = view;
    let mut index = 0_u32;
    loop {
        let px = lh(m, point + 0x1c).wrapping_add(fraction_x);
        let pz = lh(m, point + 0x1e).wrapping_add(fraction_z);
        sh(m, point + 0x1c, clamp(px));
        sh(m, point + 0x1e, clamp(pz));
        index = index.wrapping_add(1);
        let count = m.lw(view + 0x18);
        point = point.wrapping_add(4);
        if !lt(index, count) {
            break;
        }
    }
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

/// 8005E140: view frustum outline on the ground. Rotates the four screen
/// corner directions, intersects them with the ground (y 0) and the
/// -3000 plane, clips the horizon against the screen, selects up to seven
/// outline points in view order and stores their camera-relative x/z into
/// `view` +0x1C (count at +0x18). The minimum x/z go to `gp+0x4FC/0x500`.
/// `screen` +4/+6 are the screen width/height, `position` the camera VECTOR
/// (20.12), `angles` the camera SVECTOR.
pub fn view_frustum(m: &mut Machine, screen: u32, position: u32, angles: u32, view: u32) {
    let sp = m.push_frame(0x3d0);
    view_frustum_body(m, sp, screen, position, angles, view);
    m.pop_frame(0x3d0);
}

fn view_frustum_body(m: &mut Machine, sp: u32, screen: u32, position: u32, angles: u32, view: u32) {
    let s5 = sp + 0x10;
    let pos = sp + 0x290; // VECTOR position >> 12 ([sp+0x390])
    m.sw(view + 0x18, 0);
    let h = sdk::read_geom_screen(m); // [sp+0x394]
    // Widescreen: the squeezed view is 1/k wider than H says, so the
    // horizontal extents use H * k (rounded down: never narrower); the
    // vertical ones keep H. The original with the setting off.
    let h_x = wide_screen_distance(m, h);
    let s3 = sp + 0x1f0;
    let s2 = sp + 0x230;
    let mut s8 = 0xf_u32;
    let half_width = sra(lhu(m, screen + 4) << 16, 17);
    let mut s7 = 0x25_u32;
    let numerator = (half_width << 15).wrapping_sub(half_width);
    let (s1, _) = div(m, numerator, h_x, 0x8005_e1b8);
    let x = sra(m.lw(position), 12);
    let half_height = lhu(m, screen + 6);
    m.sw(pos, x);
    let y = sra(m.lw(position + 4), 12);
    m.sw(pos + 4, y);
    let z = sra(m.lw(position + 8), 12);
    m.sw(pos + 8, z);
    let half_height = sra(half_height << 16, 17);
    m.sw(view, x);
    let y = m.lw(pos + 4);
    m.sw(view + 4, y);
    let z = m.lw(pos + 8);
    let numerator = sllv(half_height, s8);
    m.sw(view + 8, z);
    let angle = lhu(m, angles);
    let numerator = numerator.wrapping_sub(half_height);
    sh(m, view + 0x10, angle);
    let angle = lhu(m, angles + 2);
    let mut low_offset = 0xc0_u32; // [sp+0x3a0]
    sh(m, view + 0x12, angle);
    let (s0, _) = div(m, numerator, h, 0x8005_e26c);
    let angle = lhu(m, angles + 4);
    let mut high_offset = 0x80_u32; // [sp+0x3a4]
    sh(m, view + 0x14, angle);
    let pitch = lhu(m, angles);
    let height = m.lw(pos + 4);
    sh(m, s5 + 0x1e2, 0);
    sh(m, s5 + 0x1e0, pitch);
    let roll = lhu(m, angles + 4);
    let below = u32::from(lt(height, 0xffff_f448)); // [sp+0x398]
    sh(m, s5 + 0x1e4, roll);
    sdk::rot_matrix_yxz(m, s3, s2);
    sdk::transpose_matrix(m, s2, sp + 0x250);
    for offset in [0x23c, 0x238, 0x234, 0x25c, 0x258, 0x254] {
        m.sw(s5 + offset, 0);
    }
    for offset in [0x1e4, 0x1ec, 0x1f4, 0x1fc] {
        sh(m, s5 + offset, 0x7fff);
    }
    let left = s1.wrapping_neg();
    sh(m, s5 + 0x1e0, left);
    sh(m, s5 + 0x1e8, s1);
    sh(m, s5 + 0x1f0, s1);
    sh(m, s5 + 0x1f8, left);
    let top = s0.wrapping_neg();
    sh(m, s5 + 0x1e2, top);
    sh(m, s5 + 0x1ea, top);
    sh(m, s5 + 0x1f2, s0);
    sh(m, s5 + 0x1fa, s0);
    sdk::set_rot_matrix(m, s2);
    sdk::set_trans_matrix(m, s2);
    let flag = sp + 0x388;
    sdk::rot_trans(m, s3, s5, flag);
    sdk::rot_trans(m, sp + 0x1f8, sp + 0x20, flag);
    sdk::rot_trans(m, sp + 0x200, sp + 0x30, flag);
    sdk::rot_trans(m, sp + 0x208, sp + 0x40, flag);
    // Copy the 32-byte MATRIX at 800B0968 to sp+0x270.
    let words: [u32; 8] = std::array::from_fn(|i| m.lw(0x800b_0968 + i as u32 * 4));
    for (i, word) in words.into_iter().enumerate() {
        m.sw(sp + 0x270 + i as u32 * 4, word);
    }
    m.sw(s5 + 0x1c0, u32::MAX);
    let lowest = min_x_index(m, s5, 4);
    m.sw(s5 + 0x1c8, lowest);
    let highest = max_x_index(m, s5, 4);
    m.sw(s5 + 0x1d4, highest);
    let mut direction = s5; // s6
    for corner in 0..4_u32 {
        if plane_intersection(m, pos, direction, 0, s5.wrapping_add(high_offset)) != 0 {
            s8 |= sllv(0x100, corner);
        }
        if plane_intersection(m, pos, direction, 0xffff_f448, s5.wrapping_add(low_offset)) != 0 {
            s8 |= sllv(0x1000, corner);
        }
        direction = direction.wrapping_add(0x10);
        low_offset = low_offset.wrapping_add(0x10);
        high_offset = high_offset.wrapping_add(0x10);
    }
    let matrix = s5 + 0x260;
    if s8 & 0xf00 != 0 {
        sdk::set_rot_matrix(m, matrix);
        sdk::set_trans_matrix(m, matrix);
        project_corners(m, s5, sp, 0x80, 0x2a0, 0x100);
    }
    if s8 & 0xf000 != 0 {
        sdk::set_rot_matrix(m, matrix);
        sdk::set_trans_matrix(m, matrix);
        project_corners(m, s5, sp, 0xc0, 0x2b0, 0x140);
    }
    if below != 0 && s8 & 0xff00 == 0 {
        return;
    }
    // Horizon: project the two ground-plane edge lines.
    let y = lhu(m, sp + 0x294);
    sh(m, s5 + 0x1e0, 0x8000);
    sh(m, s5 + 0x1e4, 0x7fff);
    sh(m, s5 + 0x1e8, 0x7fff);
    sh(m, s5 + 0x1e2, y.wrapping_neg());
    let y = lhu(m, sp + 0x294);
    sh(m, s5 + 0x1ec, 0x7fff);
    sh(m, s5 + 0x1f0, 0x8000);
    sh(m, s5 + 0x1ea, y.wrapping_neg());
    let y = lhu(m, sp + 0x294);
    sh(m, s5 + 0x1f4, 0x7fff);
    sh(m, s5 + 0x1f8, 0x7fff);
    sh(m, s5 + 0x1f2, y.wrapping_neg().wrapping_sub(0xbb8));
    let y = lhu(m, sp + 0x294);
    sh(m, s5 + 0x1fc, 0x7fff);
    sh(m, s5 + 0x1fa, y.wrapping_neg().wrapping_sub(0xbb8));
    sdk::set_rot_matrix(m, s5 + 0x240);
    let p = sp + 0x38c;
    let s3 = s5 + 0x200;
    let s2 = s5 + 0x208;
    let s6 = s5 + 0x210;
    let t3 = s5 + 0x218; // [sp+0x39c]
    sdk::rot_trans_pers(m, s5 + 0x1e0, s3, p, flag);
    sdk::rot_trans_pers(m, s5 + 0x1e8, s2, p, flag);
    sdk::rot_trans_pers(m, s5 + 0x1f0, s6, p, flag);
    sdk::rot_trans_pers(m, s5 + 0x1f8, t3, p, flag);
    sdk::set_rot_matrix(m, s5 + 0x220);
    let x_limit = sra(lhu(m, screen + 4) << 16, 17);
    let y_limit = sra(lhu(m, screen + 6) << 16, 17);
    if clip_segment(m, s3, s2, x_limit, y_limit) != 0 {
        sh(m, s5 + 0x204, 0x7fff);
        sh(m, s5 + 0x20c, 0x7fff);
        unproject_xy(m, s3, s5 + 0x40, h_x, h);
        unproject_xy(m, s2, s5 + 0x50, h_x, h);
        s8 |= 0x30;
        s7 |= 0x18;
        m.sw(s5 + 0x1cc, 4);
        m.sw(s5 + 0x1d0, 5);
    }
    let x_limit = sra(lhu(m, screen + 4) << 16, 17);
    let y_limit = sra(lhu(m, screen + 6) << 16, 17);
    if clip_segment(m, s6, t3, x_limit, y_limit) != 0 {
        sh(m, s5 + 0x214, 0x7fff);
        sh(m, s5 + 0x21c, 0x7fff);
        unproject_xy(m, s6, s5 + 0x60, h_x, h);
        unproject_xy(m, t3, s5 + 0x70, h_x, h);
        s8 |= 0xc0;
        if !s7 & 8 != 0 || lt(m.lw(s5 + 0x60), m.lw(s5 + 0x40)) {
            m.sw(s5 + 0x1cc, 6);
            s7 |= 8;
        }
        if !s7 & 0x10 != 0 || lt(m.lw(s5 + 0x50), m.lw(s5 + 0x70)) {
            m.sw(s5 + 0x1d0, 7);
            s7 |= 0x10;
        }
    }
    let full = if below != 0 {
        s8 & 0xf00 == 0xf00 || (s8 & 0xf000 == 0xf000 && m.lw(pos + 4) != 0)
    } else {
        s8 & 0xf00 == 0xf00 || (s8 & 0xf000 == 0xf000 && m.lw(pos + 4) != 0xffff_f448)
    };
    let list = sp + 0x2c0;
    if full {
        frustum_full(m, sp, s8, &mut s7, below, view, pos);
    } else if below != 0 {
        frustum_below(m, sp, s8, &mut s7);
    } else {
        // Above the ground plane: the visible ground intersections.
        let mut visible = 0_u32; // s1
        let mut slot = list; // a0
        let mut record = sp + 0x2e8; // v1
        for corner in 0..4_u32 {
            if sllv(0x100, corner) & s8 != 0 {
                let source = s5 + 0x80 + corner * 0x10;
                let words: [u32; 4] = std::array::from_fn(|i| m.lw(source + i as u32 * 4));
                for (i, word) in words.into_iter().enumerate() {
                    m.sw(record + i as u32 * 4, word);
                }
                m.sw(slot, corner + 8);
                slot += 4;
                record += 0x10;
                visible += 1;
            }
        }
        if !lt(visible, 3) {
            let records = sp + 0x2e8;
            let lowest = min_x_index(m, records, visible);
            let highest = max_x_index(m, records, visible);
            let value = m.lw(list + (lowest << 2));
            m.sw(s5 + 0x1c8, value);
            let value = m.lw(list + (highest << 2));
            m.sw(s5 + 0x1d4, value);
        }
    }
    frustum_outline(m, sp, s7, view, angles);
}

/// 8005E490..8005E5CC: project the four plane intersections at
/// `s5 + source` (8005E030) into halfword pairs at `sp + pairs` and widen
/// them to VECTOR records (x, 0, y) at `s5 + records`.
fn project_corners(m: &mut Machine, s5: u32, sp: u32, source: u32, pairs: u32, records: u32) {
    for corner in 0..4 {
        project_vector(m, s5 + source + corner * 0x10, sp + pairs + corner * 4);
    }
    for corner in 0..4 {
        let x = lh(m, sp + pairs + corner * 4);
        m.sw(s5 + records + corner * 0x10 + 4, 0);
        m.sw(s5 + records + corner * 0x10, x);
        let y = lh(m, sp + pairs + corner * 4 + 2);
        m.sw(s5 + records + corner * 0x10 + 8, y);
    }
}

/// 8005E8AC..8005EDBC: camera below the -3000 plane and not every corner
/// hit it: order the visible lower-plane intersections.
fn frustum_below(m: &mut Machine, sp: u32, s8: u32, s7: &mut u32) {
    let s5 = sp + 0x10;
    let list = sp + 0x2c0;
    *s7 &= 0xffff_fffe;
    let mut count = 0_u32; // a2
    let mut s2 = 0_u32;
    let mut out = s5; // a1
    let mut slot = list; // a3
    for corner in 0..4_u32 {
        let source = s5 + 0xc0 + corner * 0x10;
        if sllv(0x1000, corner) & s8 != 0 {
            s2 = corner + 0xc;
            m.sw(slot, s2);
            slot += 4;
            let x = m.lw(source);
            count += 1;
            m.sw(out + 0x180, x);
            let y = m.lw(source + 4);
            m.sw(out + 0x184, y);
            let z = m.lw(source + 8);
            out += 0x10;
            m.sw(out + 0x178, z);
        }
    }
    let records = s5 + 0x180;
    match count {
        1 => {
            if *s7 & 8 != 0 {
                *s7 &= 0xffff_fffb;
            }
            if *s7 & 0x10 != 0 {
                *s7 &= 0xffff_ffdf;
            }
            let value = m.lw(list);
            *s7 |= 1;
            m.sw(s5 + 0x1c0, value);
        }
        2 => {
            let highest = max_x_index(m, records, 2);
            let lowest = min_x_index(m, records, 2);
            let value = m.lw(list + (lowest << 2));
            m.sw(s5 + 0x1c4, value);
            let value = m.lw(list + (highest << 2));
            *s7 |= 0x42;
            m.sw(s5 + 0x1d8, value);
        }
        3 => {
            let highest = max_x_index(m, records, 3);
            let lowest = min_x_index(m, records, 3);
            *s7 |= 0x42;
            let middle_mask = (!sllv(1, highest) & 7) & !sllv(1, lowest);
            let value = m.lw(list + (lowest << 2));
            m.sw(s5 + 0x1c4, value);
            let value = m.lw(list + (highest << 2));
            m.sw(s5 + 0x1d8, value);
            for index in 0..3_u32 {
                if middle_mask & sllv(1, index) != 0 {
                    s2 = index;
                    break;
                }
            }
            let value = m.lw(list + (s2 << 2));
            m.sw(s5 + 0x1c0, value);
            let a0 = m.lw(list + (lowest << 2));
            *s7 |= 1;
            if sllv(0x100, a0.wrapping_sub(0xc)) & s8 != 0 {
                m.sw(s5 + 0x1c8, a0.wrapping_sub(4));
                *s7 |= 4;
            }
            let v1 = m.lw(list + (highest << 2));
            if sllv(0x100, v1.wrapping_sub(0xc)) & s8 != 0 {
                m.sw(s5 + 0x1d4, v1.wrapping_sub(4));
                *s7 |= 0x20;
            }
        }
        4 => {
            let highest = max_x_index(m, records, 4); // s1
            let lowest = min_x_index(m, records, 4); // a3
            let remaining = (!sllv(1, highest) & 0xf) & !sllv(1, lowest);
            let others = sp + 0x2e0;
            let mut slot = others;
            for index in 0..4_u32 {
                if remaining & sllv(1, index) != 0 {
                    m.sw(slot, index);
                    slot += 4;
                }
            }
            let order = sp + 0x2d0;
            if abs(lowest.wrapping_sub(highest)) == 2 {
                let r0 = m.lw(others);
                let r1 = m.lw(others + 4);
                let z0 = m.lw((r0 << 4).wrapping_add(s5) + 0x188);
                let z1 = m.lw((r1 << 4).wrapping_add(s5) + 0x188);
                let near = u32::from(lt(z1, z0));
                let index = m.lw(others + (near << 2));
                let value = m.lw(list.wrapping_add(index << 2));
                m.sw(order, value);
                let value = m.lw(list.wrapping_add(lowest << 2));
                m.sw(order + 4, value);
                let index = m.lw(others + ((near ^ 1) << 2));
                let value = m.lw(list.wrapping_add(index << 2));
                m.sw(order + 8, value);
                let value = m.lw(list.wrapping_add(highest << 2));
                m.sw(order + 0xc, value);
            } else {
                let r0 = m.lw(others);
                let r1 = m.lw(others + 4);
                let x0 = m.lw((r0 << 4).wrapping_add(s5) + 0x180);
                let x1 = m.lw((r1 << 4).wrapping_add(s5) + 0x180);
                let a0 = u32::from(lt(x1, x0));
                let a2 = m.lw(others + ((a0 ^ 1) << 2));
                let reference = m.lw((highest << 4).wrapping_add(s5) + 8);
                let candidate = m.lw((a2 << 4).wrapping_add(s5) + 8);
                let a1 = m.lw(others + (a0 << 2));
                if lt(candidate, reference) {
                    m.sw(order, a1);
                    m.sw(order + 4, lowest);
                    m.sw(order + 8, highest);
                    m.sw(order + 0xc, a2);
                } else {
                    m.sw(order, lowest);
                    m.sw(order + 4, a1);
                    m.sw(order + 8, a2);
                    m.sw(order + 0xc, highest);
                }
            }
            for (slot, target) in [(0, 0x1c4), (4, 0x1c8), (8, 0x1d4), (0xc, 0x1d8)] {
                let index = m.lw(order + slot);
                let value = m.lw(list.wrapping_add(index << 2));
                m.sw(s5 + target, value);
                let index = m.lw(order + slot);
                if sllv(0x100, index) & s8 != 0 {
                    m.sw(s5 + target, value.wrapping_sub(4));
                }
            }
            *s7 |= 0x66;
            *s7 &= 0xffff_ffe7;
            let v1 = m.lw(s5 + 0x1c8);
            if sllv(0x100, v1) & s8 != 0 {
                m.sw(s5 + 0x1c8, v1.wrapping_add(8));
            }
            let v1 = m.lw(s5 + 0x1d4);
            if sllv(0x100, v1) & s8 != 0 {
                m.sw(s5 + 0x1d4, v1.wrapping_add(8));
            }
        }
        _ => {}
    }
}

/// 8005EE78..8005F28C: every corner hits a plane: order the four
/// intersections of the nearer plane(s).
fn frustum_full(m: &mut Machine, sp: u32, s8: u32, s7: &mut u32, below: u32, view: u32, pos: u32) {
    let s5 = sp + 0x10;
    let list = sp + 0x2c0;
    let mut s1 = 0_u32;
    let mut a3 = 0_u32;
    let mut s2 = 0_u32;
    if s8 & 0xf000 == 0xf000 {
        s1 = min_x_index(m, s5 + 0xc0, 4);
        a3 = max_x_index(m, s5 + 0xc0, 4);
        s2 = 2;
    }
    if s8 & 0xf00 == 0xf00 {
        s1 = min_x_index(m, s5 + 0x80, 4);
        a3 = max_x_index(m, s5 + 0x80, 4);
        s2 |= 1;
    }
    let remaining = (!sllv(1, a3) & 0xf) & !sllv(1, s1);
    let others = sp + 0x2d0;
    let mut slot = others;
    for index in 0..4_u32 {
        if remaining & sllv(1, index) != 0 {
            m.sw(slot, index);
            slot += 4;
        }
    }
    let s0;
    if abs(s1.wrapping_sub(a3)) == 2 {
        s0 = 1_u32;
        let r0 = m.lw(others);
        let r1 = m.lw(others + 4);
        let z0 = m.lw((r0 << 4).wrapping_add(s5) + 8);
        let z1 = m.lw((r1 << 4).wrapping_add(s5) + 8);
        let near = u32::from(lt(z1, z0));
        let first = m.lw(others + (near << 2));
        m.sw(list + 4, s1);
        m.sw(list, first);
        let second = m.lw(others + ((near ^ 1) << 2));
        m.sw(list + 0xc, a3);
        m.sw(list + 8, second);
    } else {
        s0 = 0;
        let r0 = m.lw(others);
        let r1 = m.lw(others + 4);
        let x0 = m.lw((r0 << 4).wrapping_add(s5));
        let x1 = m.lw((r1 << 4).wrapping_add(s5));
        let a0 = u32::from(lt(x1, x0));
        let a2 = m.lw(others + ((a0 ^ 1) << 2));
        let reference = m.lw((a3 << 4).wrapping_add(s5) + 8);
        let candidate = m.lw((a2 << 4).wrapping_add(s5) + 8);
        let a1 = m.lw(others + (a0 << 2));
        if lt(candidate, reference) {
            m.sw(list, a1);
            m.sw(list + 4, s1);
            m.sw(list + 8, a3);
            m.sw(list + 0xc, a2);
        } else {
            m.sw(list, s1);
            m.sw(list + 4, a1);
            m.sw(list + 8, a2);
            m.sw(list + 0xc, a3);
        }
    }
    if corners_same_side(m, s5, 4) != 0 {
        if below != 0 {
            for corner in 0..4_u32 {
                let index = m.lw(list + corner * 4);
                let value = if sllv(0x100, index) & s8 != 0 {
                    index.wrapping_add(8)
                } else {
                    index
                };
                m.sw(s5 + corner * 4 + 0x1c0, value);
            }
        } else {
            let bias = if lt(0, lh(m, view + 0x10)) { 0xc } else { 8 };
            for corner in 0..4_u32 {
                let index = m.lw(list + corner * 4);
                m.sw(s5 + corner * 4 + 0x1c0, index.wrapping_add(bias));
            }
        }
        *s7 = 0xf;
        return;
    }
    let horizon = below != 0 || m.lw(pos + 4) == 0xffff_f448;
    if horizon {
        *s7 = 0xf;
        let i0 = m.lw(list);
        m.sw(s5 + 0x1c0, i0.wrapping_add(0xc));
        if s0 != 0 {
            for (slot, target) in [(4, 0x1c4), (8, 0x1c8), (0xc, 0x1cc)] {
                let index = m.lw(list + slot);
                m.sw(s5 + target, index.wrapping_add(8));
            }
        } else {
            let i0 = m.lw(list);
            m.sw(s5 + 0x1c4, i0.wrapping_add(8));
            for (slot, target) in [(4, 0x1c8), (8, 0x1cc), (0xc, 0x1d0)] {
                let index = m.lw(list + slot);
                m.sw(s5 + target, index.wrapping_add(8));
            }
            let i3 = m.lw(list + 0xc);
            *s7 = 0x3f;
            m.sw(s5 + 0x1d4, i3.wrapping_add(0xc));
        }
        return;
    }
    m.sw(s5 + 0x1c0, u32::MAX);
    let bias = if s2 & 2 != 0 { 0xc } else { 8 };
    for (slot, target) in [(0, 0x1c4), (4, 0x1c8), (8, 0x1cc)] {
        let index = m.lw(list + slot);
        m.sw(s5 + target, index.wrapping_add(bias));
    }
    let index = m.lw(list + 0xc);
    *s7 = 0x1f;
    m.sw(s5 + 0x1d0, index.wrapping_add(bias));
}

/// 8005F290..8005F4B8: rotate the selected outline points by the camera
/// heading, store them relative to their minimum (clamped to signed 16
/// bits) as view +0x1C halfword pairs, and the minimum at gp+0x4FC/0x500.
fn frustum_outline(m: &mut Machine, sp: u32, s7: u32, view: u32, angles: u32) {
    let s5 = sp + 0x10;
    let matrix = s5 + 0x220;
    let mut min_x = 0x7fff_ffff_u32; // s6
    let mut min_z = 0x7fff_ffff_u32; // s3
    sh(m, s5 + 0x1e0, 0);
    let points = sp + 0x318; // s2
    let heading = lhu(m, angles + 2);
    sh(m, s5 + 0x1e4, 0);
    sh(m, s5 + 0x1e2, heading);
    sdk::rot_matrix_yxz(m, s5 + 0x1e0, matrix);
    sdk::set_rot_matrix(m, matrix);
    for bit in 0..7_u32 {
        if sllv(1, bit) & s7 == 0 {
            continue;
        }
        let index = m.lw(s5 + bit * 4 + 0x1c0);
        let count = m.lw(view + 0x18);
        let target = (count << 4).wrapping_add(points);
        if !lt(index, 0) {
            let source = (index << 4).wrapping_add(s5);
            let x = m.lw(source);
            m.sw(sp + 0x2c0, x);
            let y = m.lw(source + 4);
            m.sw(sp + 0x2c4, y);
            let z = m.lw(source + 8);
            m.sw(sp + 0x2c8, z);
            sdk::apply_rot_matrix_lv(m, sp + 0x2c0, s5 + 0x180);
            let x = m.lw(s5 + 0x180);
            m.sw(target, x);
            let z = m.lw(s5 + 0x188);
            m.sw(target + 8, z);
        } else {
            m.sw(target + 8, 0);
            m.sw(target, 0);
        }
        let x = m.lw(target);
        if lt(x, min_x) {
            min_x = x;
        }
        let z = m.lw(target + 8);
        if lt(z, min_z) {
            min_z = z;
        }
        m.sw(view + 0x18, count.wrapping_add(1));
    }
    if lt(0, m.lw(view + 0x18)) {
        let clamp = |value: u32| -> u32 {
            if lt(value, 0xffff_8000) {
                0xffff_8000
            } else if lt(0x7fff, value) {
                0x7fff
            } else {
                value
            }
        };
        let mut point = points;
        let mut out = view;
        let mut index = 0_u32;
        loop {
            let x = m.lw(point).wrapping_add(0xffff_8000).wrapping_sub(min_x);
            let z = m
                .lw(point + 8)
                .wrapping_add(0xffff_8000)
                .wrapping_sub(min_z);
            m.sw(point, x);
            m.sw(point + 8, z);
            for offset in [0, 4, 8] {
                let value = m.lw(point + offset);
                m.sw(point + offset, clamp(value));
            }
            let x = lhu(m, point);
            sh(m, out + 0x1c, x);
            let z = lhu(m, point + 8);
            index = index.wrapping_add(1);
            sh(m, out + 0x1e, z);
            point = point.wrapping_add(0x10);
            out = out.wrapping_add(4);
            if !lt(index, m.lw(view + 0x18)) {
                break;
            }
        }
    }
    m.set_gw(0x4fc, min_x);
    m.set_gw(0x500, min_z);
}

/// 8005F4EC `test_makegrid`: rasterize the view outline (view +0x1C, count
/// +0x18) into per-row column masks over the 2048-unit grid. Writes the
/// grid bounds (+0x48..+0x54, in tiles), the width/height (+0x58/+0x5C) and
/// the row masks (+0x60..). The work area is the original 0x278-byte frame
/// (+0x10: row left masks, +0x90 row masks, +0x110 row edge masks, edge
/// cells at +0x190.., bounds at +0x200..), including its stale contents.
pub fn make_grid(m: &mut Machine, view: u32) {
    let sp = m.push_frame(0x278);
    make_grid_body(m, sp + 0x10, view);
    m.pop_frame(0x278);
}

fn make_grid_body(m: &mut Machine, t3: u32, view: u32) {
    let count = m.lw(view + 0x18); // s1
    if count == 0 {
        for offset in [0x48, 0x4c, 0x50, 0x54, 0x58, 0x5c] {
            m.sw(view + offset, 0);
        }
        let mask = m.lw(t3 + 0x80);
        m.sw(view + 0x60, mask);
        return;
    }
    let points = view + 0x1c; // s5
    for row in 0..0x20_u32 {
        m.sw(t3 + row * 4 + 0x80, 0);
        m.sw(t3 + row * 4, u32::MAX);
    }
    sh(m, t3 + 0x1f2, 0x7fff);
    sh(m, t3 + 0x1f0, 0x7fff);
    sh(m, t3 + 0x1f6, 0x8000);
    sh(m, t3 + 0x1f4, 0x8000);
    if lt(0, count) {
        let mut a1 = points;
        let mut a0 = points + 2;
        let mut index = 0_u32;
        while lt(index, count) {
            if lt(lh(m, t3 + 0x1f4), lh(m, a1)) {
                let value = lhu(m, a1);
                sh(m, t3 + 0x1f4, value);
            }
            if lt(lh(m, t3 + 0x1f6), lh(m, a0)) {
                let value = lhu(m, a0);
                sh(m, t3 + 0x1f6, value);
            }
            if lt(lh(m, a1), lh(m, t3 + 0x1f0)) {
                let value = lhu(m, a1);
                sh(m, t3 + 0x1f0, value);
            }
            if lt(lh(m, a0), lh(m, t3 + 0x1f2)) {
                let value = lhu(m, a0);
                sh(m, t3 + 0x1f2, value);
            }
            index = index.wrapping_add(1);
            a0 = a0.wrapping_add(4);
            a1 = a1.wrapping_add(4);
        }
    }
    let grid = 0xffff_f800_u32;
    let min_x = lhu(m, t3 + 0x1f0) & grid;
    let max_x = lhu(m, t3 + 0x1f4).wrapping_add(0x7ff);
    sh(m, t3 + 0x1f8, min_x);
    let min_z = lhu(m, t3 + 0x1f2);
    sh(m, t3 + 0x1fc, max_x & grid);
    let width = sra(lh(m, t3 + 0x1fc).wrapping_sub(lh(m, t3 + 0x1f8)), 11); // s4
    sh(m, t3 + 0x1fa, min_z & grid);
    let max_z = lhu(m, t3 + 0x1f6).wrapping_add(0x7ff) & grid;
    let low_z = lh(m, t3 + 0x1fa);
    sh(m, t3 + 0x1fe, max_z);
    let height = sra(sext16(max_z).wrapping_sub(low_z), 11); // s3
    if lt(width, 0) || lt(height, 0) || !lt(width, 0x20) || !lt(height, 0x20) {
        sdk::printf(m, [0x8002_4058, width, height, max_z]);
        for offset in [0x48, 0x4c, 0x50, 0x54, 0x58, 0x5c] {
            m.sw(view + offset, 0);
        }
        return;
    }
    // Edge bounding boxes: cell ranges per outline edge.
    if lt(0, count) {
        let mut t1 = t3;
        let mut edge = 0_u32;
        loop {
            let (_, start) = div(m, edge, count, 0x8005_f708);
            let (_, end) = div(m, edge.wrapping_add(1), count, 0x8005_f73c);
            let a3 = (start << 2).wrapping_add(points);
            let t2 = (end << 2).wrapping_add(points);
            let ax = lh(m, a3);
            let bx = lh(m, t2);
            let (high_x, low_x) = if lt(bx, ax) { (ax, bx) } else { (bx, ax) };
            let az = lh(m, a3 + 2);
            let bz = lh(m, t2 + 2);
            let (high_z, low_z) = if lt(bz, az) { (az, bz) } else { (bz, az) };
            let low_x = low_x & grid;
            let low_z = low_z & grid;
            let span_x = sra((high_x.wrapping_add(0x7ff) & grid).wrapping_sub(low_x), 11);
            let span_z = (high_z.wrapping_add(0x7ff) & grid).wrapping_sub(low_z);
            sh(m, t1 + 0x200, low_x);
            sh(m, t1 + 0x202, low_z);
            let origin_x = lh(m, t3 + 0x1f8);
            let span_z = sra(span_z, 11);
            m.sw(t1 + 0x1b8, sra(low_x.wrapping_sub(origin_x), 11));
            let origin_z = lh(m, t3 + 0x1fa);
            edge = edge.wrapping_add(1);
            m.sw(t1 + 0x1d4, sra(low_z.wrapping_sub(origin_z), 11));
            let x0 = m.lw(t1 + 0x1b8);
            let z0 = m.lw(t1 + 0x1d4);
            m.sw(t1 + 0x180, x0.wrapping_add(span_x));
            m.sw(t1 + 0x19c, z0.wrapping_add(span_z));
            t1 = t1.wrapping_add(4);
            if !lt(edge, count) {
                break;
            }
        }
    }
    if lt(0, count) {
        let mut s0 = t3;
        let mut edge = 0_u32;
        loop {
            make_grid_edge(m, t3, s0, points, edge, count);
            s0 = s0.wrapping_add(4);
            edge = edge.wrapping_add(1);
            if !lt(edge, count) {
                break;
            }
        }
    }
    // Leftmost covered column per row.
    let mut row = t3;
    for _ in 0..height {
        let covered = m.lw(row + 0x80);
        let mut fill = 1_u32;
        for _ in 0..width {
            if fill & covered != 0 {
                m.sw(row + 0x100, sra(fill, 1));
                break;
            }
            fill |= fill << 1;
        }
        row = row.wrapping_add(4);
    }
    // Rightmost covered column per row: final row masks.
    let mut row = t3;
    for _ in 0..height {
        let mut fill = 0x8000_0000_u32;
        let covered = m.lw(row + 0x80);
        let mut remaining = 0x1f_u32;
        loop {
            if fill & covered != 0 {
                let left = m.lw(row + 0x100);
                m.sw(row + 0x80, !((fill << 1) | left));
                break;
            }
            fill |= sra(fill, 1);
            remaining = remaining.wrapping_sub(1);
            if lt(remaining, 0) {
                break;
            }
        }
        row = row.wrapping_add(4);
    }
    let bound = |m: &mut Machine, offset: u32| -> u32 { sra(lhu(m, t3 + offset) << 16, 27) };
    let value = bound(m, 0x1f8);
    m.sw(view + 0x48, value);
    let value = bound(m, 0x1fa);
    m.sw(view + 0x4c, value);
    let value = bound(m, 0x1fc);
    m.sw(view + 0x50, value);
    let value = lhu(m, t3 + 0x1fe);
    m.sw(view + 0x58, width);
    m.sw(view + 0x5c, height);
    m.sw(view + 0x54, sra(value << 16, 27));
    for row in 0..height {
        let mask = m.lw(t3 + row * 4 + 0x80);
        m.sw(view + row * 4 + 0x60, mask);
    }
}

/// 8005F848..8005FBF4: one outline edge of 8005F4EC. NCLIP against the edge
/// marks the grid corners on its inner side (+0x110 per row), then marks
/// the cells the edge crosses in the row masks (+0x90).
fn make_grid_edge(m: &mut Machine, t3: u32, s0: u32, points: u32, edge: u32, count: u32) {
    let s2 = 1_u32;
    let (_, start) = div(m, edge, count, 0x8005_f848);
    let (_, end) = div(m, edge.wrapping_add(1), count, 0x8005_f87c);
    let a = m.lw((start << 2).wrapping_add(points));
    m.gte.mtc2(reg::SXY0, a);
    let b = m.lw((end << 2).wrapping_add(points));
    m.gte.mtc2(reg::SXY1, b);
    let a2 = m.lw(s0 + 0x1b8); // first column
    let t6 = m.lw(s0 + 0x180); // last column
    let t9 = m.lw(s0 + 0x1d4); // first row
    let t7 = m.lw(s0 + 0x19c); // last row
    let low_z = lhu(m, s0 + 0x202);
    sh(m, t3 + 0x23a, low_z);
    if !lt(t7, t9) {
        let mut a1 = (t9 << 2).wrapping_add(t3);
        let mut row = t9;
        loop {
            let low_x = lhu(m, s0 + 0x200);
            let mut inner = 0_u32;
            sh(m, t3 + 0x238, low_x);
            let corner = m.lw(t3 + 0x238);
            m.gte.mtc2(reg::SXY2, corner);
            let mut column = a2;
            while !lt(t6, column) {
                m.gte.nclip();
                let x = lhu(m, t3 + 0x238).wrapping_add(0x800);
                sh(m, t3 + 0x238, x);
                let area = m.gte.mfc2(reg::MAC0);
                if lt(0, area) {
                    inner |= sllv(s2, column);
                }
                let corner = m.lw(t3 + 0x238);
                m.gte.mtc2(reg::SXY2, corner);
                column = column.wrapping_add(1);
            }
            m.sw(a1 + 0x100, !inner);
            let z = lhu(m, t3 + 0x23a).wrapping_add(0x800);
            row = row.wrapping_add(1);
            sh(m, t3 + 0x23a, z);
            a1 = a1.wrapping_add(4);
            if lt(t7, row) {
                break;
            }
        }
    }
    // Rows strictly inside the edge's row range.
    let mut row = t9.wrapping_add(1);
    if lt(row, t7) {
        let mut t4 = (row << 2).wrapping_add(t3);
        let mut t5 = (row << 2).wrapping_sub(4);
        loop {
            let mut column = a2.wrapping_add(1);
            let corners = m.lw(t4 + 0x100);
            let t1 = t3.wrapping_add(t5);
            let t2 = t4;
            while lt(column, t6) {
                let left = column.wrapping_sub(1);
                let differ = (sra(corners, left) ^ sra(corners, column)) & 1;
                column = column.wrapping_add(1);
                if differ != 0 {
                    let bit = sllv(s2, left);
                    let value = m.lw(t1 + 0x80) | bit;
                    m.sw(t1 + 0x80, value);
                    let value = m.lw(t2 + 0x80) | bit;
                    m.sw(t2 + 0x80, value);
                    break;
                }
            }
            t4 = t4.wrapping_add(4);
            row = row.wrapping_add(1);
            t5 = t5.wrapping_add(4);
            if !lt(row, t7) {
                break;
            }
        }
    }
    // Columns strictly inside the edge's column range.
    let mut column = a2.wrapping_add(1);
    if lt(column, t6) {
        let t4 = (t9 << 2).wrapping_add(t3);
        loop {
            let mut corners = m.lw(t4 + 0x100);
            let mut row = t9.wrapping_add(1);
            let t2 = sllv(s2, column);
            if lt(row, t7) {
                let t1 = sllv(3, column.wrapping_sub(1));
                let mut a1 = (row << 2).wrapping_sub(4);
                let mut v1 = (row << 2).wrapping_add(t3);
                loop {
                    let previous = corners;
                    corners = m.lw(v1 + 0x100);
                    if (previous ^ corners) & t2 != 0 {
                        let target = t3.wrapping_add(a1);
                        let value = m.lw(target + 0x80) | t1;
                        m.sw(target + 0x80, value);
                        break;
                    }
                    a1 = a1.wrapping_add(4);
                    row = row.wrapping_add(1);
                    v1 = v1.wrapping_add(4);
                    if !lt(row, t7) {
                        break;
                    }
                }
            }
            column = column.wrapping_add(1);
            if !lt(column, t6) {
                break;
            }
        }
    }
    // Degenerate spans: a single column or a single row.
    let a0;
    if lt(t6.wrapping_sub(a2), 2) {
        let target = (t7.wrapping_sub(1) << 2).wrapping_add(t3);
        let value = m.lw(target + 0x80) | sllv(s2, a2);
        m.sw(target + 0x80, value);
        a0 = t6;
    } else {
        let mut column = a2.wrapping_add(1);
        if lt(column, t6) {
            let t0 = (t7 << 2).wrapping_add(t3);
            let a1 = (t7.wrapping_sub(1) << 2).wrapping_add(t3);
            let first = m.lw(a1 + 0x100);
            loop {
                let last = m.lw(t0 + 0x100);
                if (first ^ last) & sllv(s2, column) != 0 {
                    let value = m.lw(a1 + 0x80) | sllv(3, column.wrapping_sub(1));
                    m.sw(a1 + 0x80, value);
                }
                column = column.wrapping_add(1);
                if !lt(column, t6) {
                    break;
                }
            }
        }
        a0 = t6;
    }
    if lt(t7.wrapping_sub(t9), 2) {
        let target = (t9 << 2).wrapping_add(t3);
        let value = m.lw(target + 0x80) | sllv(s2, t6.wrapping_sub(1));
        m.sw(target + 0x80, value);
        return;
    }
    let mut row = t9.wrapping_add(1);
    let t2 = t6.wrapping_sub(1);
    if lt(row, t7) {
        let t1 = sllv(s2, t2);
        let mut t0 = (row << 2).wrapping_sub(4);
        let mut a1 = (row << 2).wrapping_add(t3);
        loop {
            let corners = m.lw(a1 + 0x100);
            let differ = (sra(corners, t2) ^ sra(corners, a0)) & 1;
            let above = t3.wrapping_add(t0);
            if differ != 0 {
                let value = m.lw(above + 0x80) | t1;
                m.sw(above + 0x80, value);
                let value = m.lw(a1 + 0x80) | t1;
                m.sw(a1 + 0x80, value);
            }
            t0 = t0.wrapping_add(4);
            row = row.wrapping_add(1);
            a1 = a1.wrapping_add(4);
            if !lt(row, t7) {
                break;
            }
        }
    }
}

// ---- fog: 80092BCC ---------------------------------------------------------------

/// 80092BCC: the 17 depth-fog chains. Each chain (offscreen-area draw
/// environment, a VRAM copy / blended sprite / far tile, then the caller's
/// environment restored from `draw_rect`/`offset`) is prepended to the
/// depth bucket from the gameplay +0x288 table. Returns the advanced
/// primitive pointer.
pub fn draw_fog(m: &mut Machine, ot: u32, packets: u32, draw_rect: u32, offset: u32) -> u32 {
    let flags = m.gw(0x40c);
    let flags = m.lw(flags + 0x5c8);
    let gameplay = m.gw(GAMEPLAY);
    let mask = if flags & 4 != 0 {
        0xe600_0001
    } else {
        0xe600_0000
    }; // t6
    let buckets = m.lw(gameplay + 0x288); // t8
    let offset = m.lw(offset); // s0
    let x = lh(m, draw_rect);
    let y = lh(m, draw_rect + 2);
    let area_start = (x & 0x3ff) | ((y & 0x3ff) << 10) | 0xe300_0000; // a3
    let right = x.wrapping_add(lh(m, draw_rect + 4)).wrapping_sub(1);
    let bottom = y.wrapping_add(lh(m, draw_rect + 6)).wrapping_sub(1);
    let area_end = (right & 0x3ff) | ((bottom & 0x3ff) << 10) | 0xe400_0000; // t7
    let draw_offset = (offset & 0x7ff) | ((sra(offset, 16) & 0x7ff) << 11) | 0xe500_0000; // s0
    let mut t3 = packets;
    let mut bucket = buckets;
    for strip in 0..17_u32 {
        let t0 = t3 + 0x14;
        m.sw(t3, (t0 & 0x00ff_ffff) | 0x0400_0000);
        m.sw(t3 + 4, 0xe304_0140);
        m.sw(t3 + 8, 0xe404_fd4f);
        m.sw(t3 + 0xc, 0xe508_0140);
        m.sw(t3 + 0x10, 0);
        let t1;
        if strip == 16 {
            t1 = t3 + 0x2c;
            let gameplay = m.gw(GAMEPLAY);
            let params = m.lw(gameplay + 0x244);
            let rgb = m.lw(params + 8);
            m.sw(t3 + 0x14, (t1 & 0x00ff_ffff) | 0x0500_0000);
            m.sw(t0 + 4, 0xe600_0001);
            m.sw(t0 + 0xc, 0);
            m.sw(t0 + 0x10, 0x0040_0010);
            m.sw(t0 + 0x14, mask);
            m.sw(t0 + 8, (rgb & 0x00ff_ffff) | 0x6000_0000);
        } else {
            let u = (strip & 6) << 3;
            let v = (strip & 8) << 3;
            if strip & 1 != 0 {
                t1 = t3 + 0x38;
                m.sw(t3 + 0x14, (t1 & 0x00ff_ffff) | 0x0800_0000);
                m.sw(t0 + 4, 0xe600_0000);
                m.sw(t0 + 8, 0xe100_0315);
                m.sw(t0 + 0xc, 0x6683_8383);
                m.sw(t0 + 0x10, 0);
                m.sw(t0 + 0x14, u | (v.wrapping_add(0x80) << 8));
                m.sw(t0 + 0x18, 0x0040_0010);
                m.sw(t0 + 0x1c, 0xe100_0335);
                m.sw(t0 + 0x20, mask);
            } else {
                t1 = t3 + 0x34;
                m.sw(t3 + 0x14, (t1 & 0x00ff_ffff) | 0x0700_0000);
                m.sw(t0 + 8, 0x0100_0000);
                m.sw(t0 + 0xc, 0x8000_0000);
                m.sw(t0 + 4, 0xe600_0000);
                m.sw(t0 + 0x1c, mask);
                m.sw(t0 + 0x10, (u + 0x140) | ((v + 0x180) << 16));
                m.sw(t0 + 0x14, 0x0100_0140);
                m.sw(t0 + 0x18, 0x0040_0010);
            }
        }
        let depth = lh(m, bucket);
        m.sw(t1, 0x0400_0000);
        m.sw(t1 + 4, area_start);
        m.sw(t1 + 8, area_end);
        m.sw(t1 + 0xc, draw_offset);
        m.sw(t1 + 0x10, 0);
        let entry = (depth << 2).wrapping_add(ot);
        // lwl/swl through a0: only the bucket's low 24 bits reach the packet,
        // so a0's previous low byte (shifted out by swl) is irrelevant.
        let a0 = lwl(m, 0, entry + 2);
        swl(m, a0, t1 + 2);
        swl(m, t3 << 8, entry + 2);
        t3 = t1 + 0x14;
        bucket = bucket.wrapping_add(2);
    }
    t3
}

// ---- sky: 80093E2C and its stages ---------------------------------------------------

/// 80093E2C: horizon/sky background. Scratchpad 0x1F800000 holds the OT
/// pointer and primitive pointer, 0x1F80001C the sky work block. Ends with
/// a mask-mode packet linked to `ot`. Returns the primitive pointer.
pub fn draw_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x28);
    let base = SCRATCH; // s1
    let sky = SCRATCH + 0x1c; // s0
    m.sw(base, ot);
    m.sw(base + 4, packets + 8);
    sky_camera(m, base, sky);
    let context = m.gw(CONTEXT);
    let mode = lh(m, context + 0x390);
    m.sw(sky + 0x170, u32::from(mode == 6));
    sky_colors(m, base, sky);
    let bands = sky_bands(m, base, sky);
    project_bands(m, sky, bands);
    classify_corners(m, sky);
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
    let flags = m.gw(0x40c);
    m.sw(base + 4, a1 + 8);
    m.sw(flags + 0x5c8, 0);
    let result = m.lw(base + 4);
    m.pop_frame(0x28);
    result
}

/// 80092E50: camera position (>> 12), angles and rotation for the sky.
pub fn sky_camera(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x20);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    let x = m.lw(camera + 4);
    let y = m.lw(camera + 8);
    let z = m.lw(camera + 0xc);
    m.sw(base + 8, sra(x, 12));
    m.sw(base + 0xc, sra(y, 12));
    m.sw(base + 0x10, sra(z, 12));
    let angles = m.lw(camera + 0x14);
    let roll = lh(m, camera + 0x18);
    let a2 = lh(m, camera + 0x24);
    m.sw(base + 0x14, angles);
    let pitch = lhu(m, base + 0x14);
    let matrix = sky + 8;
    sh(m, base + 0x18, roll);
    sh(m, base + 0x1a, a2);
    m.sw(sky, pitch);
    let roll = lhu(m, base + 0x18);
    sh(m, sky + 4, roll);
    shared::f_800306ac(m, [sky, matrix, a2, 0]);
    set_rotation(m, matrix);
    clear_translation(m);
    m.sw(sky + 0x28, 0);
    let flags = m.gw(0x40c);
    sh(m, sky + 0xf4, 0xff60);
    sh(m, sky + 0xe8, 0xff60);
    sh(m, sky + 0xf0, 0xa0);
    sh(m, sky + 0xec, 0xa0);
    sh(m, sky + 0xee, 0xff80);
    sh(m, sky + 0xea, 0xff80);
    sh(m, sky + 0xf6, 0x80);
    sh(m, sky + 0xf2, 0x80);
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

/// 800933CC: band colours (+0xF8..+0x10C) from mission parameters, blended
/// by camera altitude and pitch, then the optional flash tint.
pub fn sky_colors(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x30);
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244); // s6
    let color = m.lw(params + 0x1c) | 0x0003_0303;
    m.sw(sky + 0xf8, color);
    let altitude = m.lw(base + 0xc);
    if lt(altitude, 0xffff_7f18) {
        sky_altitude_color(m, sky + 0xf8, altitude);
    }
    let mut s0 = lh(m, params + 0x3e).wrapping_shl(1);
    let pitch = sra(lhu(m, base + 0x16) << 16, 17);
    s0 = s0.wrapping_sub(pitch) & 0x1fff;
    if !lt(s0, 0x1001) {
        s0 = 0x2000 - s0;
    }
    for (target, source) in [(0xfc, 0x14), (0x100, 0x24), (0x104, 0x18), (0x108, 0x28)] {
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
                blend_color(m, sky + band * 4 + 0xf8, target, s0);
            }
        }
    }
    let value = m.lw(params + 8);
    m.sw(sky + 0x108, value);
    m.sw(sky + 0x104, value);
    let flags = m.lw(sky + 0x168);
    let value = m.lw(params + 4);
    m.sw(sky + 0x10c, value);
    if flags & 2 != 0 {
        let flash = m.gw(0x40c);
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

/// 800935D8: darken/brighten the top colour word at `color` for high
/// altitudes (DPCS toward the colour minus 0x60/0x60/0x20).
pub fn sky_altitude_color(m: &mut Machine, color: u32, altitude: u32) {
    let factor = if lt(0xffff_5f18, altitude) {
        lwc2(m, reg::RGBC, color);
        sra(0xffff_7f18_u32.wrapping_sub(altitude), 1)
    } else {
        m.gte.mtc2(reg::RGBC, 0);
        0x1000_u32.wrapping_sub(sra(0xffff_5f18_u32.wrapping_sub(altitude), 3))
    };
    m.gte.mtc2(reg::IR0, factor);
    for (index, (register, bias)) in [(ctrl::RFC, 0x60), (ctrl::GFC, 0x60), (ctrl::BFC, 0x20)]
        .into_iter()
        .enumerate()
    {
        let channel = lbu(m, color + index as u32).wrapping_sub(bias);
        m.gte
            .ctc2(register, if lt(channel, 0) { 0 } else { channel << 4 });
    }
    m.gte.dpcs(true, false);
    swc2(m, reg::RGB2, color);
}

/// 800936A0: blend the colour word at `color` toward `target` by `t`
/// (DPCS, 0x1000 = target).
pub fn blend_color(m: &mut Machine, color: u32, target: u32, t: u32) {
    if !lt(0, t) {
        return;
    }
    if !lt(t, 0x1000) {
        m.sw(color, target);
        return;
    }
    m.gte.ctc2(ctrl::RFC, (target & 0xff) << 4);
    let shifted = sra(target, 8);
    m.gte.ctc2(ctrl::GFC, (shifted & 0xff) << 4);
    m.gte.ctc2(ctrl::BFC, sra(shifted, 4) & 0xff0);
    lwc2(m, reg::RGBC, color);
    m.gte.mtc2(reg::IR0, t);
    m.gte.dpcs(true, false);
    swc2(m, reg::RGB2, color);
}

/// 800931B8: band edge points (+0x2C.., five x pairs at the band heights)
/// and limits for the camera altitude; returns the band count (4 or 5).
pub fn sky_bands(m: &mut Machine, base: u32, sky: u32) -> u32 {
    m.push_frame(0x20);
    let gameplay = m.gw(GAMEPLAY); // t1
    let height = m.lw(base + 0xc).wrapping_neg(); // t0
    let shift = if lt(0x8ffe, height) { 3 } else { 2 }; // s2
    let params = m.lw(gameplay + 0x244);
    let high = lh(m, params + 0x22);
    let middle = lh(m, params + 0x20);
    let low = lhu(m, params + 0x10) << 16;
    sh(m, sky + 0x4e, sra(height, shift));
    sh(m, sky + 0x2e, sra(height.wrapping_add(high << 1), shift));
    let second = sra(low, 15);
    let half = sra(low, 17);
    sh(m, sky + 0x36, sra(height.wrapping_add(middle << 1), shift));
    sh(m, sky + 0x3e, sra(height.wrapping_add(second), shift));
    let last = sra(height.wrapping_add(half), shift);
    sh(m, sky + 0x46, last);
    // Widescreen: the band edges as far out as the wider view (H * k).
    let distance = lh(m, base + 0x1a);
    let distance = wide_screen_distance(m, distance);
    let (mut width, _) = div(m, 0x003b_ff88, distance, 0x8009_3268);
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
        let root = shared::f_80082740(m, [a.wrapping_sub(b), last, 0x003b_ff88, 5]);
        m.sw(sky + 0x8c, sra(root, shift));
    }
    m.pop_frame(0x20);
    bands
}

/// 80092F90: project the band edges (RTPT of the two edge points per band)
/// and clip each consecutive pair to the screen (8005DAE8). Sets the
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
            clip_segment(m, s0 - 8, s0 - 4, 0xa0, 0x80);
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
        clip_segment(m, s0 - 8, s0 - 4, 0xa0, 0x80);
    }
    m.pop_frame(0x30);
}

/// 8009309C: classify the four screen corners against each visible band
/// edge (two bits per corner, duplicated into the high byte), storing the
/// signed corner areas at +0x98 and codes at +0x80; unused codes take
/// 0xAAA (first) and 0x555 (after the last band).
pub fn classify_corners(m: &mut Machine, sky: u32) {
    let mut t3 = sky + 0x2c;
    let mut codes = sky + 0x80; // a2
    let mut band = 0_u32; // t1
    let mut visible = lh(m, sky + 0x28); // t2
    m.sw(sky + 0x7c, 0xaaa);
    if visible != 0 {
        let mut t4 = 0x98_u32;
        loop {
            if !lt(band, 5) {
                break;
            }
            m.sw(codes, 0);
            if visible & 1 != 0 {
                lwc2(m, reg::SXY0, t3);
                lwc2(m, reg::SXY1, t3 + 4);
                let mut corner = sky + 0xe8; // t0
                let mut area = (sky + t4).wrapping_sub(4); // a3
                let mut shift = 0_u32;
                while lt(shift, 8) {
                    lwc2(m, reg::SXY2, corner);
                    corner += 4;
                    area = area.wrapping_add(4);
                    m.gte.nclip();
                    swc2(m, reg::MAC0, area);
                    let value = m.lw(area);
                    let code = if value == 0 {
                        sllv(3, shift)
                    } else if lt(0, value) {
                        sllv(2, shift)
                    } else {
                        sllv(1, shift)
                    };
                    let current = m.lw(codes) | code;
                    m.sw(codes, current);
                    shift += 2;
                }
                let current = m.lw(codes);
                m.sw(codes, current | (current << 8));
            }
            visible = sra(visible, 1);
            t4 += 0x10;
            band += 1;
            t3 += 8;
            codes += 4;
            if visible == 0 {
                break;
            }
        }
    }
    band += 1;
    let mut slot = (band << 2).wrapping_add(sky);
    while lt(band, 7) {
        m.sw(slot + 0x7c, 0x555);
        band += 1;
        slot += 4;
    }
}

/// 800936FC: draw the sky bands (flat and gradient), stars and cloud band,
/// or a single background tile when no band edge is on screen.
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
            flat_band(m, base, sky, 0);
        }
        if visible & 3 != 0 {
            gradient_band(m, base, sky, 1);
        }
        if visible & 6 != 0 {
            gradient_band(m, base, sky, 2);
        }
        if stars != 0 {
            draw_stars(m, base, sky, stars);
        }
        if visible & 0xc != 0 {
            gradient_band(m, base, sky, 3);
        }
        if visible & 8 != 0 {
            if m.lw(sky + 0x170) != 0 {
                let color = m.lw(sky + 0x10c);
                m.sw(sky + 0x108, color);
                gradient_band(m, base, sky, 4);
            } else {
                flat_band(m, base, sky, 4);
            }
        }
        if visible & 0x10 != 0 && m.lw(sky + 0x16c) != 0 {
            flat_band(m, base, sky, 5);
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
        m.sw(packet + 0xc, 0x0100_0140);
        m.sw(packet + 8, corner);
        m.sw(base + 4, next);
    }
    if stars != 0 {
        draw_stars(m, base, sky, stars);
    }
    m.pop_frame(0x28);
}

/// 80093920: one flat-shaded sky band (POLY_F4 fan plus optional POLY_F3)
/// in the colour at +0xF8 + 4 * `band`. Vertices are the screen corners
/// inside the band and the clipped edge points, in screen order (the walk
/// `flight_world::SkyBand::vertices` describes).
pub fn flat_band(m: &mut Machine, base: u32, sky: u32, band: u32) {
    let edges = (band << 3).wrapping_add(sky); // t4
    let mut upper = m.lw((band << 2).wrapping_add(sky) + 0x7c);
    let mut lower = m.lw(((band + 1) << 2).wrapping_add(sky) + 0x7c);
    let mut count = 0_u32; // a3
    let mut out = sky; // v1
    let mut corner = sky; // t5
    for _ in 0..4 {
        let upper_code = upper & 0xf;
        if (upper | lower) & 3 == 3 {
            let point = m.lw(corner + 0xe8);
            count += 1;
            m.sw(out + 0x114, point);
            out += 4;
        }
        let lower_code = lower & 0xf;
        for (code, value, offset) in [
            (upper_code, 9, 0x50),
            (lower_code, 9, 0x58),
            (lower_code, 6, 0x54),
            (upper_code, 6, 0x4c),
        ] {
            if code == value {
                let point = m.lw(edges + offset);
                count += 1;
                m.sw(out + 0x114, point);
                out += 4;
            }
        }
        corner += 4;
        upper = sra(upper, 2);
        lower = sra(lower, 2);
    }
    if lt(count, 3) {
        return;
    }
    let color = m.lw((band << 2).wrapping_add(sky) + 0xf8);
    let quad = color | 0x2800_0000;
    let triangle = color | 0x2000_0000;
    let mut last = (count << 2).wrapping_add(sky); // a2
    loop {
        if !lt(count, 4) {
            let packet = m.lw(base + 4);
            let next = packet + 0x18;
            m.sw(base + 4, next);
            m.sw(packet, (next & 0x00ff_ffff) | 0x0500_0000);
            m.sw(packet + 4, quad);
            let first = m.lw(sky + 0x114);
            m.sw(packet + 8, first);
            let value = m.lw(last + 0x108);
            m.sw(packet + 0xc, value);
            let value = m.lw(last + 0x110);
            count = count.wrapping_sub(2);
            m.sw(packet + 0x10, value);
            let value = m.lw(last + 0x10c);
            last = last.wrapping_sub(8);
            m.sw(packet + 0x14, value);
        }
        if count == 3 {
            let packet = m.lw(base + 4);
            let next = packet + 0x14;
            m.sw(base + 4, next);
            m.sw(packet, (next & 0x00ff_ffff) | 0x0400_0000);
            m.sw(packet + 4, triangle);
            for (offset, source) in [(8, 0x114), (0xc, 0x118), (0x10, 0x11c)] {
                let value = m.lw(sky + source);
                m.sw(packet + offset, value);
            }
            return;
        }
        if lt(count, 3) {
            return;
        }
    }
}

/// 80093AF4: one Gouraud sky band between colours `band - 1` and `band`.
/// Corner colours are depth-cued by their signed area relative to the band
/// edge (NCLIP of the edge triangle); edge points take the band colours.
pub fn gradient_band(m: &mut Machine, base: u32, sky: u32, band: u32) {
    m.push_frame(0x10);
    let lower_color = m.lw((band << 2).wrapping_add(sky) + 0xf8); // t7
    let upper_color = m.lw((band.wrapping_sub(1) << 2).wrapping_add(sky) + 0xf8); // t6
    m.gte.ctc2(ctrl::RFC, (lower_color & 0xff) << 4);
    m.gte.ctc2(ctrl::GFC, sra(lower_color, 4) & 0xff0);
    m.gte.ctc2(ctrl::BFC, sra(lower_color, 12) & 0xff0);
    m.gte.mtc2(reg::RGBC, upper_color);
    let edges = (band << 3).wrapping_add(sky); // t2
    let triangle = edges + 0x24;
    lwc2(m, reg::SXY0, triangle);
    lwc2(m, reg::SXY1, triangle + 4);
    lwc2(m, reg::SXY2, triangle + 8);
    let mut upper = m.lw((band << 2).wrapping_add(sky) + 0x7c); // t5
    let mut lower = m.lw(((band + 1) << 2).wrapping_add(sky) + 0x7c); // t4
    m.gte.nclip();
    let mut count = 0_u32; // a2
    let divisor = sra(m.gte.mfc2(reg::MAC0), 4); // t3
    let mut areas = band.wrapping_sub(1) << 4; // a1
    let mut corner = sky; // t8
    let mut out = sky; // a0
    let mut colors = 0_u32; // a3
    for _ in 0..4 {
        if (upper | lower) & 3 == 3 {
            let point = m.lw(corner + 0xe8);
            m.sw(out + 0x114, point);
            let area = m.lw(sky.wrapping_add(areas) + 0x98);
            if divisor != 0 && lt(0, area) {
                let (fraction, _) = div(m, area << 8, divisor, 0x8009_3be4);
                m.gte.mtc2(reg::IR0, fraction);
                m.gte.dpcs(true, false);
                swc2(m, reg::RGB2, sky.wrapping_add(colors) + 0x134);
            } else {
                m.sw(out + 0x134, upper_color);
            }
            out += 4;
            colors += 4;
            count += 1;
        }
        let upper_code = upper & 0xf;
        let lower_code = lower & 0xf;
        for (code, value, offset, color) in [
            (upper_code, 9, 0x50, upper_color),
            (lower_code, 9, 0x58, lower_color),
            (lower_code, 6, 0x54, lower_color),
            (upper_code, 6, 0x4c, upper_color),
        ] {
            if code == value {
                colors += 4;
                let point = m.lw(edges + offset);
                count += 1;
                m.sw(out + 0x134, color);
                m.sw(out + 0x114, point);
                out += 4;
            }
        }
        areas = areas.wrapping_add(4);
        corner += 4;
        upper = sra(upper, 2);
        lower = sra(lower, 2);
    }
    loop {
        if lt(count, 3) {
            break;
        }
        if !lt(count, 4) {
            let packet = m.lw(base + 4);
            let next = packet + 0x24;
            m.sw(base + 4, next);
            m.sw(packet, (next & 0x00ff_ffff) | 0x0800_0000);
            let value = m.lw(sky + 0x134) | 0x3800_0000;
            m.sw(packet + 4, value);
            let value = m.lw(sky + 0x114);
            let last = (count << 2).wrapping_add(sky);
            m.sw(packet + 8, value);
            let value = m.lw(((count - 3) << 2).wrapping_add(sky) + 0x134);
            m.sw(packet + 0xc, value);
            let value = m.lw(last + 0x108);
            m.sw(packet + 0x10, value);
            let value = m.lw(((count - 1) << 2).wrapping_add(sky) + 0x134);
            m.sw(packet + 0x14, value);
            let value = m.lw(last + 0x110);
            let previous = count - 2;
            m.sw(packet + 0x18, value);
            let value = m.lw((previous << 2).wrapping_add(sky) + 0x134);
            m.sw(packet + 0x1c, value);
            let value = m.lw(last + 0x10c);
            count = previous;
            m.sw(packet + 0x20, value);
        }
        if count == 3 {
            let packet = m.lw(base + 4);
            let next = packet + 0x1c;
            m.sw(base + 4, next);
            m.sw(packet, (next & 0x00ff_ffff) | 0x0600_0000);
            let value = m.lw(sky + 0x134) | 0x3000_0000;
            m.sw(packet + 4, value);
            for (offset, source) in [
                (8, 0x114),
                (0xc, 0x138),
                (0x10, 0x118),
                (0x14, 0x13c),
                (0x18, 0x11c),
            ] {
                let value = m.lw(sky + source);
                m.sw(packet + offset, value);
            }
            break;
        }
    }
    m.pop_frame(0x10);
}

/// 80093FB8: the sun/moon strip of semi-transparent POLY_FT4 packets along
/// the horizon direction from the gameplay +0x268 block.
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
    let wide = word & 1; // s3
    let s0 = sra(word, 16).wrapping_sub(altitude);
    let depth = sra(s0.wrapping_sub(0xfa0), 3);
    if !lt(depth, 0x1000) {
        return;
    }
    set_rotation(m, gameplay + 0x4c);
    clear_translation(m);
    m.gte.mtc2(reg::IR0, depth);
    far_color(m, 0, 0, 0);
    color = (color & 0x00ff_ffff) | 0x2e00_0000;
    m.gte.mtc2(reg::RGBC, color);
    m.gte.dpcs(true, false);
    // Widescreen: the glow band over the wider view (H * k).
    let distance = lh(m, s2 + 0x1a);
    let distance = wide_screen_distance(m, distance);
    let elevation = sdk::ratan2(m, 0xcd, distance); // a2
    let heading = sra(lhu(m, s2 + 0x16) << 16, 18);
    let mut t0 = heading.wrapping_sub(elevation) & 0xf80;
    color = m.gte.mfc2(reg::RGB2);
    let spread = if wide != 0 { 0xffff_e800 } else { 0xffff_f400 };
    sh(m, s1 + 0x116, sra(s0.wrapping_add(spread), 2));
    sh(m, s1 + 0x11a, sra(s0.wrapping_sub(spread), 2));
    let table = 0x800b_4f84_u32; // rcossin_tbl
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
                0x129,
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
                sh(m, packet + 0x16, 0x39);
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
        m.sw(s2 + 4, packet);
    }
}

/// 80094300: up to 256 stars (TILE_1 runs of seven packed into 0x0E-word
/// packets) from the star table at `[gp+0x40C]`, drifting with the camera.
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
    let mut table = m.gw(0x40c); // t7
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
        if !lt(abs(sra(screen, 16)), 0x81) || !lt(abs(sext16(screen)), 0xa1) {
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

// ---- background type 2: cloud layer and banded sky (80095B94) ------------------------------

/// 80095B94: background type 2. Draws the cloud layer over the terrain view
/// (80095090), then the banded sky (80094900). Returns the primitive
/// pointer.
pub fn draw_cloud_sky(m: &mut Machine, view: u32, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x18);
    let packets = draw_cloud_layer(m, view, ot, packets);
    let result = draw_cloud_sky_bands(m, ot, packets);
    m.pop_frame(0x18);
    result
}

/// 80094900: banded sky of background type 2 (like 80093E2C with the
/// 800945E8 band set-up, four bands and 800947CC drawing).
pub fn draw_cloud_sky_bands(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x28);
    let base = SCRATCH; // s1
    let sky = SCRATCH + 0x1c;
    m.sw(base, ot);
    let first = packets + 8; // s0
    m.sw(base + 4, first);
    sky_camera(m, base, sky);
    cloud_sky_bands(m, base, sky);
    project_bands(m, sky, 4);
    classify_corners(m, sky);
    draw_cloud_bands(m, base, sky);
    let mask = u32::from(m.lw(base + 0x184) & 4 != 0);
    let mut a1 = packets;
    if m.lw(base + 4) == first {
        m.sw(a1, 0x0100_0000);
        m.sw(a1 + 4, 0xe600_0000 | mask);
        let link = lwl(m, mask, ot + 2);
        swl(m, link, a1 + 2);
        swl(m, a1 << 8, ot + 2);
    } else {
        m.sw(a1, (first & 0x00ff_ffff) | 0x0100_0000);
        m.sw(a1 + 4, 0xe600_0001);
        a1 = m.lw(base + 4);
        m.sw(a1, 0x0100_0000);
        m.sw(a1 + 4, 0xe600_0000 | mask);
        let link = lwl(m, 0x0100_0000, ot + 2);
        swl(m, link, a1 + 2);
        swl(m, packets << 8, ot + 2);
    }
    let flags = m.gw(0x40c);
    m.sw(base + 4, a1 + 8);
    m.sw(flags + 0x5c8, 0);
    let result = m.lw(base + 4);
    m.pop_frame(0x28);
    result
}

/// 800945E8: band colours and the four band edges of background type 2,
/// from the mission parameters and the camera's cloud heights.
pub fn cloud_sky_bands(m: &mut Machine, base: u32, sky: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let params = m.lw(gameplay + 0x244);
    let top = m.lw(params + 0x1c);
    m.sw(sky + 0xf8, top);
    let params = m.lw(gameplay + 0x244);
    let fog = m.lw(params + 8);
    m.sw(sky + 0x100, fog);
    m.sw(sky + 0xfc, fog);
    let params = m.lw(gameplay + 0x244);
    let ground = m.lw(params + 4);
    m.sw(sky + 0x108, ground);
    m.sw(sky + 0x104, ground);
    let params = m.lw(gameplay + 0x244);
    if lhu(m, params + 2) & 0x400 == 0 && m.lw(sky + 0x168) & 1 == 0 {
        let value = m.lw(sky + 0x108) | 0x0100_0000;
        m.sw(sky + 0x108, value);
    }
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    let mut upper = lh(m, camera + 0x2a); // t1
    if !lt(upper, 0x6801) {
        upper = 0x6800;
    }
    let mut lower = lh(m, camera + 0x28); // t0
    if lt(lower, 0x3e8) {
        lower = 0x3e8;
    }
    let times200 = |value: u32| -> u32 {
        let v = (value << 1).wrapping_add(value);
        ((v << 3).wrapping_add(value)) << 3
    };
    upper = sra(upper, 1);
    // Widescreen: the band edges as far out as the wider view (H * k).
    let distance = lh(m, base + 0x1a);
    let distance = wide_screen_distance(m, distance);
    let (upper_width, _) = div(m, times200(upper), distance, 0x8009_46d0); // t3
    lower = sra(lower, 1);
    let (lower_width, _) = div(m, times200(lower), distance, 0x8009_4714); // t2
    let mut y = sra(m.lw(base + 0xc).wrapping_neg().wrapping_sub(0xe00), 1); // a3
    let mut edge = sky; // a1
    let mut depth = sky; // a2
    for band in 0..4_u32 {
        if band.wrapping_sub(1) < 2 {
            sh(m, edge + 0x2c, upper_width.wrapping_neg());
            sh(m, edge + 0x30, upper_width);
            m.sw(depth + 0x7c, upper);
        } else {
            m.sw(depth + 0x7c, lower);
            sh(m, edge + 0x2c, lower_width.wrapping_neg());
            sh(m, edge + 0x30, lower_width);
        }
        sh(m, edge + 0x32, y);
        sh(m, edge + 0x2e, y);
        if band == 1 {
            y = sra(m.lw(base + 0xc).wrapping_neg().wrapping_add(0x200), 1);
        }
        edge += 8;
        depth += 4;
    }
}

/// 800947CC: draw the four bands of background type 2 (or one far tile
/// when none is visible and the ground colour's top byte is set).
pub fn draw_cloud_bands(m: &mut Machine, base: u32, sky: u32) {
    m.push_frame(0x28);
    let s4 = lhu(m, sky + 0x28) << 16;
    let visible = sra(s4, 16);
    if visible != 0 {
        if m.lb(sky + 0x10b) == 0 {
            let first = visible & 1;
            if first != 0 {
                flat_band(m, base, sky, 0);
            }
            if visible & 8 != 0 {
                flat_band(m, base, sky, 4);
            }
            if first != 0 {
                gradient_band(m, base, sky, 1);
            }
            if sra(s4, 18) & 1 != 0 {
                gradient_band(m, base, sky, 3);
            }
        }
        if visible & 6 != 0 {
            flat_band(m, base, sky, 2);
        }
    } else if m.lb(sky + 0x10b) != 0 {
        let packet = m.lw(base + 4);
        let next = packet + 0x10;
        m.sw(packet, (next & 0x00ff_ffff) | 0x0300_0000);
        let color = m.lw(sky + 0xfc) | 0x6000_0000;
        m.sw(packet + 4, color);
        let corner = m.lw(sky + 0xe8);
        m.sw(packet + 0xc, 0x0100_0140);
        m.sw(packet + 8, corner);
        m.sw(base + 4, next);
    }
    m.pop_frame(0x28);
}

/// 80094A40: 1 when the screen bounding box of four SXY words is at least
/// 0x200 high or 0x400 wide.
pub fn quad_too_large(sxy0: u32, sxy1: u32, sxy2: u32, sxy3: u32) -> u32 {
    let extent = |values: [u32; 4]| -> u32 {
        let [a, b, c, d] = values;
        let mut high = if lt(b, a) { a } else { b };
        let other = if lt(d, c) { c } else { d };
        if lt(high, other) {
            high = other;
        }
        let mut low = if lt(a, b) { a } else { b };
        let other = if lt(c, d) { c } else { d };
        if lt(low, other) {
            // keep low
        } else {
            low = other;
        }
        high.wrapping_sub(low)
    };
    let ys = [sxy0, sxy1, sxy2, sxy3].map(|word| sra(word, 16));
    if !lt(extent(ys), 0x200) {
        return 1;
    }
    let xs = [sxy0, sxy1, sxy2, sxy3].map(sext16);
    u32::from(!lt(extent(xs), 0x400))
}

/// 80094B60: midpoint of two cloud vertices (0x14 bytes: position, code,
/// SXY, colour, SZ, uv) into `out`: positions averaged, colour and uv
/// averaged per byte lane.
pub fn cloud_midpoint(m: &mut Machine, out: u32, a: u32, b: u32) {
    let low = 0x00ff_00ff_u32;
    let high = 0xff00_ff00_u32;
    for offset in [0, 2, 4] {
        let value = sra(lh(m, a + offset).wrapping_add(lh(m, b + offset)), 1);
        sh(m, out + offset, value);
    }
    let ca = m.lw(a + 0xc);
    let cb = m.lw(b + 0xc);
    let lanes = (((ca & low).wrapping_add(cb & low)) >> 1) & low;
    let upper = (((ca & high).wrapping_add(cb & high)) >> 1) & high;
    m.sw(out + 0xc, lanes | upper);
    let ua = lhu(m, a + 0x12);
    let ub = lhu(m, b + 0x12);
    let lanes = ((ua & low).wrapping_add(ub & low)) >> 1;
    let upper = sra((ua & high).wrapping_add(ub & high), 1) & 0xff00;
    sh(m, out + 0x12, lanes | upper);
}

/// 80094C1C: outcode of the screen point at `point` against the 320x256
/// screen (1 left, 2 right, 4 above, 8 below).
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
    if lt(y, 0xffff_ff80) {
        code |= 4;
    } else if !lt(y, 0x81) {
        code |= 8;
    }
    code
}

/// 80094C78: subdivide a cloud quad (four vertex pointers at `quad`) into
/// four children (pointers at `quad + 0x10`), projecting the five new
/// vertices into `vertices`, and draw or recurse into each child; `level`
/// limits the depth. `packets` is the scratchpad block (+0 primitive
/// pointer, +0x28/+0x2A clut and tpage).
pub fn subdivide_cloud_quad(m: &mut Machine, quad: u32, vertices: u32, packets: u32, level: u32) {
    m.push_frame(0x38);
    let s2 = quad;
    let s5 = vertices;
    let corner = |m: &mut Machine, index: u32| m.lw(s2 + index * 4);
    let (c0, c1) = (corner(m, 0), corner(m, 1));
    cloud_midpoint(m, s5, c0, c1);
    lwc2(m, reg::VXY0, s5);
    lwc2(m, reg::VZ0, s5 + 4);
    let (c0, c2) = (corner(m, 0), corner(m, 2));
    cloud_midpoint(m, s5 + 0x14, c0, c2);
    m.gte.rtps(true, false);
    let (c1, c3) = (corner(m, 1), corner(m, 3));
    cloud_midpoint(m, s5 + 0x28, c1, c3);
    let (c2, c3) = (corner(m, 2), corner(m, 3));
    cloud_midpoint(m, s5 + 0x3c, c2, c3);
    let (c1, c2) = (corner(m, 1), corner(m, 2));
    cloud_midpoint(m, s5 + 0x50, c1, c2);
    let error_mask = 0x7e01_e000;
    let mut vertex = s5; // s0
    for _ in 0..4 {
        swc2(m, reg::SXY2, vertex + 8);
        let z = m.gte.mfc2(reg::SZ3);
        sh(m, vertex + 0x10, z);
        let flags = m.gte.cfc2(ctrl::FLAG);
        lwc2(m, reg::VXY0, vertex + 0x14);
        lwc2(m, reg::VZ0, vertex + 0x18);
        m.gte.rtps(true, false);
        let code = if flags & error_mask != 0 {
            u32::MAX
        } else {
            screen_outcode(m, vertex + 8)
        };
        sh(m, vertex + 6, code);
        vertex += 0x14;
    }
    swc2(m, reg::SXY2, vertex + 8);
    let z = m.gte.mfc2(reg::SZ3);
    sh(m, vertex + 0x10, z);
    let flags = m.gte.cfc2(ctrl::FLAG);
    let code = if flags & error_mask != 0 {
        u32::MAX
    } else {
        screen_outcode(m, vertex + 8)
    };
    sh(m, vertex + 6, code);
    let level = level.wrapping_sub(1); // s7
    let mut s4 = s2;
    for child in 0..4_u32 {
        let mut all = u32::MAX; // a2
        let mut any = 0_u32; // s1
        let mut far = 0_u32; // t0
        let mut near = 0x7fff_u32; // t1
        for index in 0..4_u32 {
            let slot = s2 + index * 4; // a3
            let sum = index + child;
            if child == index {
                let value = m.lw(s4);
                m.sw(s4 + 0x10, value);
            } else {
                let offset = if sum == 3 {
                    0x50
                } else if lt(sum, 3) {
                    (sum * 5) * 4 - 0x14
                } else {
                    (sum * 5) * 4 - 0x28
                };
                m.sw(slot + 0x10, s5.wrapping_add(offset));
            }
            let point = m.lw(slot + 0x10);
            let code = lh(m, point + 6);
            let z = lh(m, point + 0x10);
            all &= code;
            any |= code;
            if lt(far, z) {
                far = z;
            } else if lt(z, near) {
                near = z;
            }
        }
        let mut s1 = any;
        let mut draw = false;
        if far.wrapping_sub(0x79) < 0x777f && all == 0 {
            if lt(s1, 0) {
                // a vertex failed to project: subdivide (or draw at the limit)
            } else {
                let vertex = |m: &mut Machine, index: u32| {
                    let point = m.lw(s2 + 0x10 + index * 4);
                    m.lw(point + 8)
                };
                let v3 = vertex(m, 3);
                m.gte.mtc2(reg::SXY0, v3);
                let v1 = vertex(m, 1);
                m.gte.mtc2(reg::SXY1, v1);
                let v2 = vertex(m, 2);
                m.gte.mtc2(reg::SXY2, v2);
                let depth_range = far.wrapping_sub(near);
                m.gte.nclip();
                let first = m.gte.mfc2(reg::MAC0);
                if lt(first, 0) {
                    let v0 = vertex(m, 0);
                    m.gte.mtc2(reg::SXY0, v0);
                    m.gte.nclip();
                    let second = m.gte.mfc2(reg::MAC0);
                    if lt(0, second) {
                        let area = first.wrapping_neg().wrapping_add(second);
                        if !lt(area, 0x401) {
                            let product = area.wrapping_mul(depth_range);
                            if !lt(sra(product, 16), 0x201) {
                                s1 = u32::MAX;
                            } else if lt(0, s1) {
                                let (v0, v1, v2, v3) =
                                    (vertex(m, 0), vertex(m, 1), vertex(m, 2), vertex(m, 3));
                                if quad_too_large(v0, v1, v2, v3) != 0 {
                                    s1 = u32::MAX;
                                }
                            }
                        }
                        draw = true;
                    }
                }
            }
            if draw || lt(s1, 0) {
                if lt(s1, 0) && !lt(level, 0) {
                    subdivide_cloud_quad(m, s2 + 0x10, s5 + 0x64, packets, level);
                    s4 += 4;
                    continue;
                }
                emit_cloud_quad(m, s2 + 0x10, packets);
            }
        }
        s4 += 4;
    }
    m.pop_frame(0x38);
}

/// 80094F84..80095048: one POLY_GT4 of four cloud vertices (pointers at
/// `corners`), chained forward from the primitive pointer at `packets`.
fn emit_cloud_quad(m: &mut Machine, corners: u32, packets: u32) {
    let packet = m.lw(packets);
    let next = packet + 0x34;
    m.sw(packet, (next & 0x00ff_ffff) | 0x0c00_0000);
    m.sw(packets, next);
    for (index, (color, xy)) in [(4, 8), (0x10, 0x14), (0x1c, 0x20), (0x28, 0x2c)]
        .into_iter()
        .enumerate()
    {
        let vertex = m.lw(corners + index as u32 * 4);
        let rgb = m.lw(vertex + 0xc);
        let sxy = m.lw(vertex + 8);
        m.sw(packet + color, rgb);
        m.sw(packet + xy, sxy);
    }
    let clut = lhu(m, packets + 0x28);
    sh(m, packet + 0xe, clut);
    let tpage = lhu(m, packets + 0x2a);
    sh(m, packet + 0x1a, tpage);
    for (index, uv) in [(0, 0xc), (1, 0x18), (2, 0x24), (3, 0x30)] {
        let vertex = m.lw(corners + index * 4);
        let value = lhu(m, vertex + 0x12);
        sh(m, packet + uv, value);
    }
}

/// Cloud colour index lookup of 80095090: nibble `(column - bias) & 7` of
/// the row pattern word at `work + base + ((column - bias) >> 1 & 4)`, or
/// the fixed colour at +0x88 when the column state is 2.
fn cloud_color(m: &mut Machine, work: u32, state: u32, column: u32, bias: u32, base: u32) -> u32 {
    if sext16(state) == 2 {
        return m.lw(work + 0x88);
    }
    let index = column.wrapping_sub(bias);
    let word = m.lw(work.wrapping_add(sra(index, 1) & 4) + base);
    let nibble = srlv(word, (index & 7) << 2) & 0xf;
    m.lw(work + (nibble << 2) + 0x4c)
}

/// One projected cloud vertex of 80095090: FLAG, SXY2 and SZ3 of the last
/// RTPS, then the depth-cued colour (or the raw colour and code -1 when the
/// projection failed). Returns SZ3.
fn cloud_vertex(m: &mut Machine, vertex: u32, color: u32) -> u32 {
    let flags = m.gte.cfc2(ctrl::FLAG);
    swc2(m, reg::SXY2, vertex + 8);
    let z = m.gte.mfc2(reg::SZ3);
    m.gte.mtc2(reg::RGBC, color);
    if flags & 0x7e01_e000 != 0 {
        sh(m, vertex + 6, u32::MAX);
        m.sw(vertex + 0xc, color);
    } else {
        m.gte.dpcs(true, false);
        let code = screen_outcode(m, vertex + 8);
        sh(m, vertex + 6, code);
        swc2(m, reg::RGB2, vertex + 0xc);
    }
    z
}

/// 80095090: the cloud layer of background type 2 over the visible grid of
/// `view` (row masks, bounds and camera position from 8005FE48). Clouds are
/// 2048-unit cells at height -0xC00..-0x800 coloured from the row patterns
/// at 800B2474; edge rows/columns fade through the +0x8C/+0x90 states. Near
/// cells are subdivided (80094C78). Packets are chained forward and linked
/// to `ot` as one list. Returns the primitive pointer.
pub fn draw_cloud_layer(m: &mut Machine, view: u32, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x38);
    let result = draw_cloud_layer_body(m, view, ot, packets);
    m.pop_frame(0x38);
    result
}

fn draw_cloud_layer_body(m: &mut Machine, view: u32, ot: u32, packets: u32) -> u32 {
    let s2 = SCRATCH;
    sh(m, s2 + 0x28, 0x4ed6);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(s2, packets);
    sh(m, s2 + 0x2a, 0x39);
    set_rotation(m, gameplay + 0x4c);
    sdk::set_trans_matrix(m, gameplay + 0x4c);
    let x = m.lw(view + 0x48).wrapping_add(m.lw(view + 0x38));
    sh(m, s2 + 0x24, x);
    let z = m.lw(view + 0x4c).wrapping_add(m.lw(view + 0x3c));
    sh(m, s2 + 0x26, z);
    let columns = m.lw(view + 0x58);
    let rows = m.lw(view + 0x5c);
    m.sw(s2 + 0x14, columns);
    m.sw(s2 + 0x18, rows);
    let camera_x = m.lw(view);
    let camera_y = m.lw(view + 4);
    let camera_z = m.lw(view + 8);
    m.sw(s2 + 0x88, 0x3ca6_c8c5);
    m.sw(s2 + 8, camera_y);
    m.sw(s2 + 4, camera_x);
    m.sw(s2 + 0xc, camera_z);
    sh(m, s2 + 0x94, 0xffff_f400_u32.wrapping_sub(camera_y));
    sh(m, s2 + 0x96, 0xffff_f500_u32.wrapping_sub(camera_y));
    sh(m, s2 + 0x98, 0xffff_f800_u32.wrapping_sub(camera_y));
    far_color(m, 0xc50, 0xc80, 0xa60);
    m.gte.mtc2(reg::RGBC, 0x3c72_9081);
    let mut depth = 0_u32;
    for index in 0..15_u32 {
        m.gte.mtc2(reg::IR0, depth);
        m.gte.dpcs(true, false);
        depth += 0x111;
        swc2(m, reg::RGB2, s2 + 0x4c + index * 4);
    }
    far_color(m, 0x1d0, 0x390, 0x210);
    let row_z = lhu(m, s2 + 0x26);
    let rows = m.lw(s2 + 0x18);
    m.sw(s2 + 0x1c, 0);
    let mut s7 = sext16(row_z);
    if !lt(0, rows) {
        return cloud_layer_link(m, ot, packets);
    }
    loop {
        cloud_row(m, view, s7);
        let row = m.lw(s2 + 0x1c).wrapping_add(1);
        let rows = m.lw(s2 + 0x18);
        m.sw(s2 + 0x1c, row);
        s7 = s7.wrapping_add(1);
        if !lt(row, rows) {
            break;
        }
    }
    cloud_layer_link(m, ot, packets)
}

/// 80095B1C..80095B60: link the forward chain into `ot`.
fn cloud_layer_link(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    let end = m.lw(SCRATCH);
    if packets == end {
        return packets;
    }
    let last = end.wrapping_sub(0x34);
    let link = lwl(m, 0, ot + 2);
    swl(m, link, last + 2);
    swl(m, packets << 8, ot + 2);
    m.lw(SCRATCH)
}

/// 80095238..80095B00: one grid row of the cloud layer.
fn cloud_row(m: &mut Machine, view: u32, s7: u32) {
    let s2 = SCRATCH;
    let s8 = 2_u32;
    if s7.wrapping_sub(0x57) < 0x4a {
        m.sw(s2 + 0x8c, 0);
    } else {
        if s7.wrapping_sub(0x55) >= 0x4e {
            return;
        }
        let state = match s7 {
            0x55 => 0x0001_0002,
            0x56 => 1,
            0xa1 => 0x0001_0000,
            _ => 0x0002_0001,
        };
        m.sw(s2 + 0x8c, state);
    }
    let far_z = (s7 << 11).wrapping_sub(lhu(m, s2 + 0xc));
    sh(m, s2 + 0x30, far_z);
    if !lt(sext16(far_z), 0x7800) {
        return;
    }
    sh(m, s2 + 0x38, far_z.wrapping_add(0x800));
    let state = lhu(m, s2 + 0x8c);
    let height = lhu(m, s2.wrapping_add(sra(state << 16, 15)) + 0x94);
    sh(m, s2 + 0x2e, height);
    let state = lhu(m, s2 + 0x8e);
    let height = lhu(m, s2.wrapping_add(sra(state << 16, 15)) + 0x94);
    sh(m, s2 + 0x36, height);
    for edge in 0..2_u32 {
        let state = lh(m, s2 + 0x8c + edge * 2);
        let (low, high) = if state == s8 {
            (u32::MAX, u32::MAX)
        } else {
            let entry = ((s7.wrapping_add(edge).wrapping_sub(6) & 0xf) << 3) + 0x800b_2474;
            (m.lw(entry), m.lw(entry + 4))
        };
        m.sw(s2 + 0x3c + edge * 8, low);
        m.sw(s2 + 0x40 + edge * 8, high);
    }
    let row = m.lw(s2 + 0x1c);
    let mask = m.lw((row << 2).wrapping_add(view) + 0x60);
    let column_x = lhu(m, s2 + 0x24);
    let mut s5 = 0_u32;
    m.sw(s2 + 0x20, 0);
    let mut s6 = sext16(column_x);
    m.sw(s2 + 0x10, mask);
    if mask == 0 {
        return;
    }
    loop {
        if !lt(m.lw(s2 + 0x20), m.lw(s2 + 0x14)) {
            return;
        }
        // Every path of a column advances s6 once (the subdivision path in
        // the delay slot of its jump to 80095AE4).
        s5 = cloud_column(m, s5, s6);
        s6 = s6.wrapping_add(1);
        let index = m.lw(s2 + 0x20).wrapping_add(1);
        let mask = m.lw(s2 + 0x10) >> 1;
        m.sw(s2 + 0x20, index);
        m.sw(s2 + 0x10, mask);
        if mask == 0 {
            return;
        }
    }
}

/// 80095390..80095AE0: one grid cell of a cloud row (column `s6`). `s5`
/// counts the cells of the current run; the left vertices of a continued
/// run are the previous cell's right vertices (the two 0x14-byte vertex
/// pairs at scratchpad +0xFC alternate by run parity). Returns the new run
/// length.
fn cloud_column(m: &mut Machine, s5: u32, s6: u32) -> u32 {
    let s2 = SCRATCH;
    if m.lw(s2 + 0x10) & 1 == 0 {
        return 0;
    }
    if s6.wrapping_sub(0x3f) < 0x4a {
        m.sw(s2 + 0x90, 0);
    } else {
        if s6.wrapping_sub(0x3d) >= 0x4e || m.lw(s2 + 0x8c) != 0 {
            return 0;
        }
        let state = match s6 {
            0x3d => 0x0001_0002,
            0x3e => 1,
            0x89 => 0x0001_0000,
            _ => 0x0002_0001,
        };
        m.sw(s2 + 0x90, state);
    }
    let left_x = (s6 << 11).wrapping_sub(lhu(m, s2 + 4));
    sh(m, s2 + 0x2c, left_x);
    if !lt(sext16(left_x), 0x7800) {
        return 0;
    }
    sh(m, s2 + 0x34, left_x);
    // s4 tracks the far depth of the cell. The original also tracks the near
    // depth in s3, but overwrites s3 with the outcode OR before any use
    // (see below), so that tracking has no effect and is omitted.
    let mut s5 = s5;
    let mut s4;
    if s5 == 0 {
        // First cell of a run: project its two left vertices.
        if m.lw(s2 + 0x90) != 0 {
            let state = lhu(m, s2 + 0x90);
            let height = lhu(m, s2.wrapping_add(sra(state << 16, 15)) + 0x94);
            sh(m, s2 + 0x36, height);
            sh(m, s2 + 0x2e, height);
        } else if m.lw(s2 + 0x8c) == 0 {
            let height = lhu(m, s2 + 0x94);
            sh(m, s2 + 0x36, height);
            sh(m, s2 + 0x2e, height);
        }
        let xy = m.lw(s2 + 0x2c);
        let z = m.lw(s2 + 0x30);
        m.gte.mtc2(reg::VXY0, xy);
        m.gte.mtc2(reg::VZ0, z);
        let mut s1 = s2 + 0xfc;
        sh(m, s1 + 4, z);
        m.gte.rtps(true, false);
        let state = lhu(m, s2 + 0x90);
        m.sw(s2 + 0xfc, xy);
        let color = cloud_color(m, s2, state, s6, 4, 0x3c);
        let bottom = m.lw(s2 + 0x34);
        m.sw(s1 + 0x28, bottom);
        s4 = cloud_vertex(m, s1, color);
        s1 += 0x28;
        let z = m.lw(s2 + 0x38);
        m.gte.mtc2(reg::VXY0, bottom);
        m.gte.mtc2(reg::VZ0, z);
        sh(m, s1 + 4, z);
        m.gte.rtps(true, false);
        sh(m, s1.wrapping_sub(0x18), s4);
        let state = lhu(m, s2 + 0x90);
        let color = cloud_color(m, s2, state, s6, 4, 0x44);
        // FLAG/SZ3/SXY2 order differs slightly here: SZ3 before SXY2.
        let flags = m.gte.cfc2(ctrl::FLAG);
        let z = m.gte.mfc2(reg::SZ3);
        swc2(m, reg::SXY2, s1 + 8);
        if lt(s4, z) {
            s4 = z;
        }
        sh(m, s1 + 0x10, z);
        m.gte.mtc2(reg::RGBC, color);
        if flags & 0x7e01_e000 != 0 {
            sh(m, s1 + 6, u32::MAX);
            m.sw(s1 + 0xc, color);
        } else {
            m.gte.dpcs(true, false);
            let code = screen_outcode(m, s1 + 8);
            sh(m, s1 + 6, code);
            swc2(m, reg::RGB2, s1 + 0xc);
        }
    } else {
        // Continuing a run: the previous right edge is this cell's left.
        let s1 = s2 + 0xfc + (s5 & 1) * 20;
        let top = lh(m, s1 + 0x10);
        let bottom = lh(m, s1 + 0x38);
        s4 = if lt(top, bottom) { bottom } else { top };
    }
    if m.lw(s2 + 0x90) != 0 {
        let state = lhu(m, s2 + 0x92);
        let height = lhu(m, s2.wrapping_add(sra(state << 16, 15)) + 0x94);
        sh(m, s2 + 0x36, height);
        sh(m, s2 + 0x2e, height);
    }
    let right_x = lhu(m, s2 + 0x2c).wrapping_add(0x800);
    let z = m.lw(s2 + 0x30);
    sh(m, s2 + 0x2c, right_x);
    let xy = m.lw(s2 + 0x2c);
    sh(m, s2 + 0x34, right_x);
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    let mut s1 = s2 + 0xfc + ((s5.wrapping_add(1)) & 1) * 20;
    m.gte.rtps(true, false);
    m.sw(s1, xy);
    sh(m, s1 + 4, z);
    let state = lhu(m, s2 + 0x92);
    let color = cloud_color(m, s2, state, s6, 3, 0x3c);
    let flags = m.gte.cfc2(ctrl::FLAG);
    let z = m.gte.mfc2(reg::SZ3);
    swc2(m, reg::SXY2, s1 + 8);
    if lt(s4, z) {
        s4 = z;
    }
    sh(m, s1 + 0x10, z);
    finish_cloud_vertex(m, s1, flags, color);
    let xy = m.lw(s2 + 0x34);
    let z = m.lw(s2 + 0x38);
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    s1 += 0x28;
    m.sw(s1, xy);
    m.gte.rtps(true, false);
    sh(m, s1 + 4, z);
    let state = lhu(m, s2 + 0x92);
    let color = cloud_color(m, s2, state, s6, 3, 0x44);
    s5 = s5.wrapping_add(1);
    let flags = m.gte.cfc2(ctrl::FLAG);
    let z = m.gte.mfc2(reg::SZ3);
    swc2(m, reg::SXY2, s1 + 8);
    if lt(s4, z) {
        s4 = z;
    }
    sh(m, s1 + 0x10, z);
    finish_cloud_vertex(m, s1, flags, color);
    if s4.wrapping_sub(0x79) >= 0x777f {
        return s5;
    }
    let codes = [0x102, 0x116, 0x12a, 0x13e].map(|offset| lhu(m, s2 + offset));
    if codes[0] & codes[1] & codes[2] & codes[3] != 0 {
        return s5;
    }
    // The original keeps the OR of the codes in s3 and subtracts it from
    // the far depth below (where a near depth would be expected).
    let mut s3 = sext16(codes[0] | codes[1] | codes[2] | codes[3]);
    // After the increment of s5 the right (new) vertices are at parity
    // s5 & 1 and the left ones at (s5 + 1) & 1.
    let right = s2 + 0xfc + (s5 & 1) * 20;
    let left = s2 + 0xfc + (s5.wrapping_add(1) & 1) * 20;
    let mut draw = false;
    if !lt(s3, 0) {
        // FIFO: SXY2 holds the bottom-right projection; NCLIP over
        // (bottom-right, top-right, bottom-left), then (top-left, ...).
        lwc2(m, reg::SXYP, right + 8);
        lwc2(m, reg::SXYP, left + 0x28 + 8);
        let depth_range = s4.wrapping_sub(s3);
        m.gte.nclip();
        let first = m.gte.mfc2(reg::MAC0);
        if !lt(first, 0) {
            return s5;
        }
        lwc2(m, reg::SXY0, left + 8);
        m.gte.nclip();
        let second = m.gte.mfc2(reg::MAC0);
        if !lt(0, second) {
            return s5;
        }
        let area = first.wrapping_neg().wrapping_add(second);
        if !lt(area, 0x401) {
            let product = area.wrapping_mul(depth_range);
            if !lt(sra(product, 16), 0x201) {
                s3 = u32::MAX;
            } else if lt(0, s3) {
                let corners = [0x104, 0x118, 0x12c, 0x140].map(|offset| m.lw(s2 + offset));
                if quad_too_large(corners[0], corners[1], corners[2], corners[3]) != 0 {
                    s3 = u32::MAX;
                }
            }
        }
        draw = !lt(s3, 0);
    }
    if !draw {
        // Subdivide the cell through 80094C78 with corner uvs.
        m.sw(s2 + 0x9c, left);
        let first = m.lw(s2 + 0x9c);
        m.sw(s2 + 0xa8, right + 0x28);
        m.sw(s2 + 0xa4, left + 0x28);
        m.sw(s2 + 0xa0, right);
        sh(m, first + 0x12, 0x8000);
        let vertex = m.lw(s2 + 0xa0);
        sh(m, vertex + 0x12, 0x803f);
        let vertex = m.lw(s2 + 0xa4);
        sh(m, vertex + 0x12, 0xbf00);
        let vertex = m.lw(s2 + 0xa8);
        sh(m, vertex + 0x12, 0xbf3f);
        subdivide_cloud_quad(m, s2 + 0x9c, s2 + 0x14c, s2, 4);
        return s5;
    }
    let packet = m.lw(s2);
    let next = packet + 0x34;
    m.sw(packet, (next & 0x00ff_ffff) | 0x0c00_0000);
    sh(m, packet + 0x24, 0xbf00);
    m.sw(packet + 0xc, 0x4ed6_8000);
    m.sw(packet + 0x18, 0x0039_803f);
    sh(m, packet + 0x30, 0xbf3f);
    for (vertex, color) in [
        (left, 4),
        (left + 0x28, 0x1c),
        (right, 0x10),
        (right + 0x28, 0x28),
    ] {
        let rgb = m.lw(vertex + 0xc);
        let sxy = m.lw(vertex + 8);
        m.sw(packet + color, rgb);
        m.sw(packet + color + 4, sxy);
    }
    m.sw(s2, next);
    s5
}

/// Colour half of a cloud vertex after its FLAG/SZ3/SXY2 were taken.
fn finish_cloud_vertex(m: &mut Machine, vertex: u32, flags: u32, color: u32) {
    m.gte.mtc2(reg::RGBC, color);
    if flags & 0x7e01_e000 != 0 {
        sh(m, vertex + 6, u32::MAX);
        m.sw(vertex + 0xc, color);
    } else {
        m.gte.dpcs(true, false);
        let code = screen_outcode(m, vertex + 8);
        sh(m, vertex + 6, code);
        swc2(m, reg::RGB2, vertex + 0xc);
    }
}

// ---- background type 3: lit dome (800964FC) ------------------------------------------------

/// 800964FC: background type 3. A black backdrop tile, 256 stars and the lit
/// dome (80095D40), closed by the mask-mode packet linked to `ot`. Returns
/// the primitive pointer.
pub fn draw_dome_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x28);
    let base = SCRATCH; // s1
    let s0 = packets + 8;
    let tile = packets + 0x18;
    m.sw(base, ot);
    m.sw(base + 4, s0);
    m.sw(packets + 8, (tile & 0x00ff_ffff) | 0x0300_0000);
    m.sw(s0 + 8, 0xff80_ff60);
    m.sw(s0 + 0xc, 0x0100_0140);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(s0 + 4, 0x6000_0000);
    m.sw(base + 4, tile);
    for (target, source) in [(0x14, 0x14), (0x16, 0x16), (0x18, 0x18), (0x1a, 0x24)] {
        let camera = m.lw(gameplay + 0x28);
        let value = lhu(m, camera + source);
        sh(m, base + target, value);
    }
    draw_stars(m, base, base + 0x1c, 0x100);
    draw_dome(m);
    let flags = m.gw(0x40c);
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
    let flags = m.gw(0x40c);
    m.sw(flags + 0x5c8, 0);
    m.pop_frame(0x28);
    a0 + 8
}

/// 80095BC4: one dome ring of 24 vertices (+ the closing copy of the first)
/// at elevation step `ring` (9 and above: the horizon ring) from the
/// sine/cosine pairs at `circle`; 16-byte records at `out`: outcode (or -1),
/// SXY, uv pair, lit colour (NCCS).
pub fn dome_ring(m: &mut Machine, out: u32, circle: u32, ring: u32) {
    m.push_frame(0x30);
    let (mut height, radius) = if lt(ring, 9) {
        let angle = ((((ring << 1).wrapping_add(ring)) << 3).wrapping_add(ring)) << 2;
        let h = sdk::rcos(m, angle);
        (h, ((ring << 7).wrapping_sub(ring)) << 2)
    } else {
        (sdk::rcos(m, 0x35e), 0x1000)
    };
    let normal_z = height.wrapping_neg(); // s6
    height = height.wrapping_sub(sra(height, 2));
    let mut record = out; // s0
    let mut circle = circle; // s4
    for _ in 0..0x18 {
        let x = radius.wrapping_mul(lh(m, circle + 2).wrapping_neg());
        let y = radius.wrapping_mul(lh(m, circle).wrapping_neg());
        let xy = (sra(x, 12) & 0xffff) | (sra(y, 12) << 16);
        m.gte.mtc2(reg::VXY0, xy);
        m.gte.mtc2(reg::VZ0, height);
        let u = sra(x, 16);
        let v = sra(xy, 20);
        m.gte.rtps(true, false);
        circle = circle.wrapping_add(4);
        sh(m, record + 8, u);
        sh(m, record + 0xa, v);
        if lt(m.gte.cfc2(ctrl::FLAG), 0) {
            m.sw(record, u32::MAX);
        } else {
            swc2(m, reg::SXY2, record + 4);
            m.gte.mtc2(reg::VZ0, normal_z);
            m.gte.nccs(true, true);
            let code = screen_outcode(m, record + 4);
            m.sw(record, code);
            swc2(m, reg::RGB2, record + 0xc);
        }
        record += 0x10;
    }
    for offset in [0, 4, 8, 0xc] {
        let value = m.lw(record.wrapping_sub(0x180) + offset);
        m.sw(record + offset, value);
    }
    m.pop_frame(0x30);
}

/// Texture coordinates of a dome quad corner pair (80095FA4..80095FF0 and
/// 800961B4..80096200): offsets from the uv halfwords of `a`/`b`, the tpage.
fn dome_uv(m: &mut Machine, a: u32, b: u32) -> (u32, u32, u32) {
    let v = sra(lh(m, a + 0xa).wrapping_add(lh(m, b + 0xa)), 1);
    let u = lh(m, a + 8).wrapping_add(lh(m, b + 8));
    let mut tpage = 0xa8_u32;
    let du = if lt(u, 0) {
        0xff
    } else {
        tpage = 0xaa;
        0
    };
    let dv = if lt(v, 0) {
        0xff
    } else {
        tpage += 4;
        0
    };
    (du, dv, tpage)
}

/// 80095D40: the lit dome of background type 3 in scratchpad
/// (0x1F80001C: angles, matrices, the 24-entry circle table, then ring
/// buffers at +0x6C/+0x1FC): apex fan, seven rings of POLY_GT4, the horizon
/// band and its glow sprites.
pub fn draw_dome(m: &mut Machine) {
    m.push_frame(0x30);
    let s4 = SCRATCH + 0x1c;
    let s6 = SCRATCH;
    sh(m, s4, 0x3000);
    sh(m, s4 + 2, 0);
    sh(m, s4 + 4, 0);
    shared::f_80030534(m, [s4, SCRATCH + 0x24, 0, 0]);
    let gameplay = m.gw(GAMEPLAY);
    sdk::mul_matrix0(m, gameplay + 0x6c, SCRATCH + 0x24, SCRATCH + 0x44);
    // Inline SetLightMatrix(0x1F800044).
    let light: [u32; 5] = std::array::from_fn(|i| m.lw(SCRATCH + 0x44 + i as u32 * 4));
    for (i, word) in light.into_iter().enumerate() {
        m.gte.ctc2(ctrl::L11L12 + i, word);
    }
    let gameplay = m.gw(GAMEPLAY);
    sdk::set_mul_matrix(m, gameplay + 0x4c, SCRATCH + 0x24);
    clear_translation(m);
    m.gte.ctc2(ctrl::RBK, 0x300);
    m.gte.ctc2(ctrl::RBK + 1, 0x380);
    m.gte.ctc2(ctrl::RBK + 2, 0x600);
    m.gte.ctc2(ctrl::LR1LR2, 0x1000);
    m.gte.ctc2(ctrl::LR1LR2 + 1, 0x1000_0000);
    m.gte.ctc2(ctrl::LR1LR2 + 2, 0);
    m.gte.ctc2(ctrl::LR1LR2 + 3, 0x1000);
    m.gte.ctc2(ctrl::LB3, 0);
    m.gte.mtc2(reg::RGBC, 0x3483_8383);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0xc00);
    let first_ring = s4 + 0x6c; // s3
    m.gte.rtps(true, false);
    for step in 0..0x18_u32 {
        let numerator = step << 12;
        let high = ((i64::from(numerator as i32) * 0x2aaa_aaab) >> 32) as u32;
        let index = sra(high, 2).wrapping_sub(sra(numerator, 31));
        let pair = m.lw((index << 2).wrapping_add(0x800b_4f84));
        m.sw(s4 + step * 4 + 0xc, pair);
    }
    if lt(m.gte.cfc2(ctrl::FLAG), 0) {
        m.sw(s4, u32::MAX);
    } else {
        swc2(m, reg::SXY2, s4 + 4);
        m.gte.mtc2(reg::VXY0, 0);
        m.gte.mtc2(reg::VZ0, 0xffff_f000);
        m.gte.nccs(true, true);
        let code = screen_outcode(m, s4 + 4);
        m.sw(s4, code);
        swc2(m, reg::RGB2, s4 + 8);
    }
    m.gte.mtc2(reg::RGBC, 0x3c83_8383);
    dome_ring(m, first_ring, s4 + 0xc, 1);
    // Apex fan: POLY_GT3 from the apex to ring 1.
    let apex_code = m.lw(s4);
    if apex_code != u32::MAX {
        let mut packet = m.lw(s6 + 4); // t1
        let mut next = (packet << 8).wrapping_add(0x2800); // t4
        let mut code = m.lw(first_ring); // t2
        for k in 0..0x18_u32 {
            let record = first_ring + k * 0x10;
            let previous = code;
            code = m.lw(record + 0x10);
            if previous == u32::MAX || code == u32::MAX || apex_code & previous & code != 0 {
                continue;
            }
            m.sw(packet, (next >> 8) | 0x0900_0000);
            let apex_color = m.lw(s4 + 8);
            let color0 = m.lw(record + 0xc);
            let color1 = m.lw(record + 0x1c);
            m.sw(packet + 4, apex_color);
            m.sw(packet + 0x10, color0);
            m.sw(packet + 0x1c, color1);
            let sxy1 = m.lw(record + 0x14);
            let apex = m.lw(s4 + 4);
            let sxy0 = m.lw(record + 4);
            m.sw(packet + 0x20, sxy1);
            m.sw(packet + 8, apex);
            m.sw(packet + 0x14, sxy0);
            let (du, dv, tpage) = dome_uv(m, record, record + 0x10);
            sh(m, packet + 0x1a, tpage);
            m.sw(packet + 0xc, (dv << 8) | 0x3ff0_0000 | du);
            for (offset, source, delta) in [
                (0x18, record + 8, du),
                (0x19, record + 0xa, dv),
                (0x24, record + 0x18, du),
                (0x25, record + 0x1a, dv),
            ] {
                let value = lbu(m, source).wrapping_add(delta);
                sb(m, packet + offset, value);
            }
            next = next.wrapping_add(0x2800);
            packet = packet.wrapping_add(0x28);
        }
        m.sw(s6 + 4, packet);
    }
    // Rings 2..8 (`ring + 1`): POLY_GT4 between consecutive rings.
    let mut packet = m.lw(s6 + 4); // s2
    for ring in 1..8_u32 {
        let previous = s4 + 0x6c + ((ring + 1) & 1) * 400; // s3
        let current = s4 + 0x6c + (ring & 1) * 400; // s0
        if ring == 7 {
            for register in [ctrl::RBK, ctrl::RBK + 1, ctrl::RBK + 2] {
                m.gte.ctc2(register, 0x500);
            }
            m.gte.ctc2(ctrl::LR1LR2, 0x924);
            m.gte.ctc2(ctrl::LR1LR2 + 1, 0x0924_0000);
            m.gte.ctc2(ctrl::LR1LR2 + 3, 0x924);
            m.gte.mtc2(reg::RGBC, 0x3aa0_6858);
        }
        dome_ring(m, current, s4 + 0xc, ring + 1);
        let mut next = (packet << 8).wrapping_add(0x3400); // t5
        let mut code_previous = m.lw(previous); // t4
        let mut code_current = m.lw(current); // t2
        for k in 0..0x18_u32 {
            let a = previous + k * 0x10; // t1 - 0x1A
            let b = current + k * 0x10; // t0 - 0x1A
            let (a0, a2) = (code_previous, code_current);
            code_previous = m.lw(a + 0x10);
            code_current = m.lw(b + 0x10);
            if a0 == u32::MAX
                || a2 == u32::MAX
                || code_previous == u32::MAX
                || code_current == u32::MAX
                || a0 & a2 & code_previous & code_current != 0
            {
                continue;
            }
            m.sw(packet, (next >> 8) | 0x0c00_0000);
            let color0 = m.lw(a + 0xc);
            let color1 = m.lw(a + 0x1c);
            m.sw(packet + 4, color0);
            m.sw(packet + 0x10, color1);
            if lt(ring, 7) {
                let color2 = m.lw(b + 0xc);
                let color3 = m.lw(b + 0x1c);
                m.sw(packet + 0x1c, color2);
                m.sw(packet + 0x28, color3);
            } else {
                m.sw(packet + 0x28, 0);
                m.sw(packet + 0x1c, 0);
            }
            for (offset, source) in [
                (8, a + 4),
                (0x14, a + 0x14),
                (0x20, b + 4),
                (0x2c, b + 0x14),
            ] {
                let value = m.lw(source);
                m.sw(packet + offset, value);
            }
            let (du, dv, tpage) = dome_uv(m, b, b + 0x10);
            sh(m, packet + 0xe, 0x3ff0);
            sh(m, packet + 0x1a, tpage);
            for (offset, source, delta) in [
                (0xc, a + 8, du),
                (0xd, a + 0xa, dv),
                (0x18, a + 0x18, du),
                (0x19, a + 0x1a, dv),
                (0x24, b + 8, du),
                (0x25, b + 0xa, dv),
                (0x30, b + 0x18, du),
                (0x31, b + 0x1a, dv),
            ] {
                let value = lbu(m, source).wrapping_add(delta);
                sb(m, packet + offset, value);
            }
            next = next.wrapping_add(0x3400);
            packet = packet.wrapping_add(0x34);
        }
    }
    // Horizon band: draw mode, then flat-textured quads between ring 7 and
    // the horizon ring (ring buffer +0x1FC).
    let mut a3 = packet + 8;
    m.sw(s6 + 4, packet);
    m.sw(packet, (a3 & 0x00ff_ffff) | 0x0100_0000);
    m.sw(packet + 4, 0xe100_0220);
    let mut code_upper = m.lw(s4 + 0x6c); // t2
    let mut code_lower = m.lw(s4 + 0x1fc); // t1
    m.sw(s6 + 4, a3);
    for k in 0..0x18_u32 {
        let upper = s4 + 0x6c + k * 0x10; // t0 - 0x14
        let lower = s4 + 0x1fc + k * 0x10; // a1 - 0x14
        let (a0, a2) = (code_upper, code_lower);
        code_upper = m.lw(upper + 0x10);
        code_lower = m.lw(lower + 0x10);
        if a0 == u32::MAX
            || a2 == u32::MAX
            || code_upper == u32::MAX
            || code_lower == u32::MAX
            || a0 & a2 & code_upper & code_lower != 0
        {
            continue;
        }
        let next = a3 + 0x24;
        m.sw(a3, (next & 0x00ff_ffff) | 0x0800_0000);
        m.sw(a3 + 4, 0x3a00_0000);
        m.sw(a3 + 0xc, 0);
        for (offset, source) in [
            (0x14, lower + 0xc),
            (0x1c, lower + 0x1c),
            (8, upper + 4),
            (0x10, upper + 0x14),
            (0x18, lower + 4),
            (0x20, lower + 0x14),
        ] {
            let value = m.lw(source);
            m.sw(a3 + offset, value);
        }
        a3 = next;
    }
    m.sw(s6 + 4, a3);
    dome_ring(m, s4 + 0x6c, s4 + 0xc, 9);
    // Glow: two semi-transparent POLY_FT4-sized packets per segment.
    let mut a3 = m.lw(s6 + 4);
    let mut code_lower = m.lw(s4 + 0x1fc); // t4
    let mut code_upper = m.lw(s4 + 0x6c); // t2
    for k in 0..0x18_u32 {
        let lower = s4 + 0x1fc + k * 0x10; // t0 - 0x14
        let upper = s4 + 0x6c + k * 0x10; // t1 - 0x14
        let (a0, a2) = (code_lower, code_upper);
        code_lower = m.lw(lower + 0x10);
        code_upper = m.lw(upper + 0x10);
        if a0 == u32::MAX
            || a2 == u32::MAX
            || code_lower == u32::MAX
            || code_upper == u32::MAX
            || a0 & a2 & code_lower & code_upper != 0
        {
            continue;
        }
        let second = a3 + 0x28;
        let next = a3 + 0x50;
        m.sw(a3, (second & 0x00ff_ffff) | 0x0900_0000);
        m.sw(a3 + 0x28, (next & 0x00ff_ffff) | 0x0900_0000);
        m.sw(a3 + 4, 0xe100_0240);
        m.sw(a3 + 0x2c, 0xe100_0220);
        m.sw(a3 + 0x30, 0x3a01_0101);
        m.sw(a3 + 8, 0x3a01_0101);
        m.sw(a3 + 0x38, 0x0001_0101);
        m.sw(a3 + 0x10, 0x0001_0101);
        m.sw(a3 + 0x20, 0x0093_9393);
        m.sw(a3 + 0x18, 0x0093_9393);
        let lower_sxy0 = m.lw(lower + 0xc);
        let lower_sxy1 = m.lw(lower + 0x1c);
        m.sw(a3 + 0x40, lower_sxy0);
        m.sw(a3 + 0x48, lower_sxy1);
        let upper0 = m.lw(upper + 4);
        let upper1 = m.lw(upper + 0x14);
        m.sw(a3 + 0x34, upper0);
        m.sw(a3 + 0xc, upper0);
        m.sw(a3 + 0x3c, upper1);
        m.sw(a3 + 0x14, upper1);
        let lower0 = m.lw(lower + 4);
        let lower1 = m.lw(lower + 0x14);
        m.sw(a3 + 0x44, lower0);
        m.sw(a3 + 0x1c, lower0);
        m.sw(a3 + 0x4c, lower1);
        m.sw(a3 + 0x24, lower1);
        a3 = next;
    }
    m.sw(s6 + 4, a3);
    m.pop_frame(0x30);
}

// ---- background type 4: night sky with fade overlay (8009668C) -------------------------------

/// `mult` by a magic constant: the high word of the signed 64-bit product.
fn mulhi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}

/// 8009668C: background type 4. A clock-driven fade overlay (8008E238), a
/// black backdrop, the horizon dome (80096A5C) and ground grid (80096E98),
/// the mask-mode packets, then the OT run compaction (80083330). Returns the
/// primitive pointer.
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
    let mut level = lh(m, ((phase & 0xfff) << 2).wrapping_add(0x800b_4f86));
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
    m.sw(tile + 8, 0xff80_ff60);
    m.sw(tile + 0xc, 0x0100_0140);
    m.sw(s2 + 4, after);
    set_rotation(m, gameplay + 0x4c);
    sdk::set_trans_matrix(m, gameplay + 0x4c);
    night_dome(m, s2, clock);
    night_ground(m, s2);
    let flags = m.gw(0x40c);
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
    let flags = m.gw(0x40c);
    m.sw(flags + 0x5c8, 0);
    compact_ot_runs(m, ot.wrapping_sub(0x3ff8), 0xfff);
    m.pop_frame(0x30);
    result
}

/// 80083330: shorten runs of empty entries in a reverse-linked OT of `count`
/// entries: an entry ending a run of five or more empty entries links past
/// the run.
pub fn compact_ot_runs(m: &mut Machine, ot: u32, count: u32) {
    let mut expected = (ot << 8) >> 8; // v1
    let mut index = 1_u32; // t0
    let mut run = 0_u32; // a3
    let mut entry = ot + 4; // a2
    if lt(1, count) {
        loop {
            let value = m.lw(entry);
            if value == expected {
                run = run.wrapping_add(4);
            } else {
                if !lt(run, 5) {
                    m.sw(entry - 4, expected.wrapping_sub(run));
                }
                run = 0;
            }
            entry += 4;
            index += 1;
            expected = expected.wrapping_add(4);
            if !lt(index, count) {
                break;
            }
        }
    }
    if run != 0 {
        let target = (index << 2).wrapping_add(ot);
        m.sw(target.wrapping_sub(4), expected.wrapping_sub(run));
    }
}

/// 8008E238: full-screen fade overlay: a mask packet, an optional draw
/// offset (from `environment`, when non-zero), two 256-wide subtractive and
/// additive sprites/tiles in the depth-cued grey of `brightness`, chained
/// and linked to `ot`. Returns the primitive pointer.
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
        let size = if x != 0 { 0x0100_0040 } else { 0x0100_0100 };
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

/// 80096954: one horizon-dome ring of 20 vertices (+ the closing copy) at
/// height `z` from the circle table at `block + 0x4C`; 12-byte records:
/// outcode (or -1), SXY, depth-cued colour. A ring behind the camera marks
/// all 20 records -1.
pub fn night_ring(m: &mut Machine, block: u32, out: u32, z: u32) {
    m.push_frame(0x28);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, z);
    let mut circle = block + 0x4c;
    m.gte.mvmva(true, false, 0, 0, 3);
    let depth = m.gte.mfc2(reg::MAC3);
    let x = m.gte.mfc2(reg::MAC1);
    let y = m.gte.mfc2(reg::MAC2);
    if lt(depth, 0xffff_f800) {
        let mut record = out + 0xe4;
        for _ in 0..20 {
            m.sw(record, u32::MAX);
            record = record.wrapping_sub(0xc);
        }
        m.pop_frame(0x28);
        return;
    }
    m.gte.ctc2(ctrl::TRX, x);
    m.gte.ctc2(ctrl::TRY, y);
    m.gte.ctc2(ctrl::TRZ, depth);
    m.gte.mtc2(reg::VZ0, 0);
    let mut record = out;
    for _ in 0..20 {
        lwc2(m, reg::VXY0, circle);
        circle += 4;
        m.gte.rtps(true, false);
        if lt(m.gte.cfc2(ctrl::FLAG), 0) {
            m.sw(record, u32::MAX);
        } else {
            swc2(m, reg::SXY2, record + 4);
            m.gte.dpcs(true, false);
            let code = screen_outcode(m, record + 4);
            m.sw(record, code);
            swc2(m, reg::RGB2, record + 8);
        }
        record += 0xc;
    }
    for offset in [0, 4, 8] {
        let value = m.lw(record.wrapping_sub(0xf0) + offset);
        m.sw(record + offset, value);
    }
    m.pop_frame(0x28);
}

/// 80096A5C: the horizon dome of background type 4: saves the rotation,
/// sets a clock-dependent projection distance and depth cue (8005D0C4),
/// builds the 20-step circle table and draws 24 rings of POLY_GT4 between
/// consecutive rings (80096954); restores the rotation and distance.
pub fn night_dome(m: &mut Machine, base: u32, clock: u32) {
    m.push_frame(0x38);
    let s4 = base + 0x1c;
    let rotation: [u32; 5] = std::array::from_fn(|i| m.gte.cfc2(ctrl::R11R12 + i));
    for (i, word) in rotation.into_iter().enumerate() {
        m.sw(s4 + i as u32 * 4, word);
    }
    let table = 0x800b_4f86_u32; // s0
    let wave = lh(m, ((clock << 7) & 0x3f80).wrapping_add(table));
    let distance = lh(m, base + 0x1a);
    let lifted = wave.wrapping_add(0x1000);
    let lifted = if lt(lifted, 0) {
        sra(wave.wrapping_add(0x103f), 6)
    } else {
        sra(lifted, 6)
    };
    let h = lifted.wrapping_add(0x80);
    m.gte.ctc2(ctrl::H, h);
    m.sw(s4 + 0x48, h << 11);
    crate::dispatch::call(m, 0x8005_d0c4, [0x2710, 0x4e20, distance, 0]);
    let mut source = table.wrapping_sub(2);
    for step in 0..20_u32 {
        let pair = m.lw(source);
        source = source.wrapping_add(0x330);
        sh(m, s4 + step * 4 + 0x4c, sra(pair, 17));
        sh(m, s4 + step * 4 + 0x4e, sra(pair << 16, 17));
    }
    let first = m.lw(s4 + 0x4c);
    m.sw(s4 + 20 * 4 + 0x4c, first);
    m.gte.mtc2(reg::RGBC, 0x3c87_8787);
    far_color(m, 0, 0, 0);
    let mut z = 0x3000_u32.wrapping_sub((clock << 7) & 0x3ff); // s5
    let mut packet = m.lw(base + 4); // s2
    night_ring(m, s4, s4 + 0xa0, z);
    z = z.wrapping_sub(0x400);
    for ring in 0..0x18_u32 {
        let current = s4 + 0xa0 + ((ring + 1) & 1) * 252; // s1
        let previous = s4 + 0xa0 + (ring & 1) * 252; // s0
        night_ring(m, s4, current, z);
        let mut code_previous = m.lw(previous);
        let mut code_current = m.lw(current);
        for k in 0..20_u32 {
            let a = previous + k * 0xc;
            let b = current + k * 0xc;
            let (a0, a2) = (code_previous, code_current);
            code_previous = m.lw(a + 0xc);
            code_current = m.lw(b + 0xc);
            if a0 == u32::MAX
                || a2 == u32::MAX
                || code_previous == u32::MAX
                || code_current == u32::MAX
                || a0 & a2 & code_previous & code_current != 0
            {
                continue;
            }
            let next = packet + 0x34;
            m.sw(packet, (next & 0x00ff_ffff) | 0x0c00_0000);
            for (offset, source) in [
                (4, a + 8),
                (0x10, a + 0x14),
                (8, a + 4),
                (0x14, a + 0x10),
                (0x1c, b + 8),
                (0x28, b + 0x14),
            ] {
                let value = m.lw(source);
                m.sw(packet + offset, value);
            }
            let sxy2 = m.lw(b + 4);
            let sxy3 = m.lw(b + 0x10);
            sh(m, packet + 0x24, 0xbf00);
            m.sw(packet + 0xc, 0x4ed6_8000);
            m.sw(packet + 0x18, 0x0019_803f);
            sh(m, packet + 0x30, 0xbf3f);
            m.sw(packet + 0x20, sxy2);
            m.sw(packet + 0x2c, sxy3);
            packet = next;
        }
        z = z.wrapping_sub(0x400);
    }
    m.sw(base + 4, packet);
    set_rotation(m, s4);
    clear_translation(m);
    let distance = lhu(m, base + 0x1a);
    m.gte.ctc2(ctrl::H, distance);
    m.pop_frame(0x38);
}

/// 80096D5C: one ground-grid row (16 vertices + an end marker) at grid row
/// `row` around the camera cell (`block` +0x20..+0x28), 12-byte records:
/// outcode (or -1), SXY, depth-cued colour.
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
            if lt(sy, 0xffff_ff80) {
                code |= 4;
            } else if !lt(sy, 0x81) {
                code |= 8;
            }
            m.sw(record, code);
            swc2(m, reg::RGB2, record + 8);
        }
        record += 0xc;
    }
    m.sw(record, u32::MAX);
}

/// 80096E98: the ground grid of background type 4 below the camera: four
/// passes of 16x16 cells (POLY_G4 quads, G3 triangles at the edge),
/// depth cue from 8005D0C4, two discarded rand() calls per quad.
pub fn night_ground(m: &mut Machine, base: u32) {
    m.push_frame(0x40);
    let s8 = base + 0x1c;
    let distance = lh(m, base + 0x1a);
    far_color(m, 0, 0, 0);
    crate::dispatch::call(m, 0x8005_d0c4, [0x7d0, 0x4e20, distance, 0]);
    // Inline ReadRotMatrix(s8): rotation and translation.
    let saved: [u32; 8] = std::array::from_fn(|i| m.gte.cfc2(ctrl::R11R12 + i));
    for (i, word) in saved.into_iter().enumerate() {
        m.sw(s8 + i as u32 * 4, word);
    }
    let gameplay = m.gw(GAMEPLAY);
    set_rotation(m, gameplay + 0x2c);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0x1000);
    let camera_y = m.lw(base + 8);
    let camera_z = m.lw(base + 0x10);
    m.gte.mvmva(true, false, 0, 0, 3);
    m.sw(s8 + 0x20, sra(camera_y.wrapping_neg(), 11) & 0x3ff);
    m.sw(s8 + 0x28, sra(camera_z.wrapping_neg(), 11) & 0x3ff);
    let height = m.lw(base + 0xc);
    m.sw(
        s8 + 0x24,
        (sra(height.wrapping_neg(), 11) & 0x7ff).wrapping_sub(0x14cc),
    );
    swc2(m, reg::IR3, s8 + 0x30);
    set_rotation(m, s8);
    m.gte.mtc2(reg::RGBC, 0x00c0_5020);
    let mut pass = 0_u32; // [sp+0x10]
    let header = m.lw(base + 4);
    let mut s2 = header + 8;
    m.sw(header, (s2 & 0x00ff_ffff) | 0x0100_0000);
    m.sw(header + 4, 0xe100_0220);
    loop {
        let tilt = sra(m.lw(s8 + 0x30), 10).wrapping_neg();
        let mut first = tilt.wrapping_add(4); // a2
        let mut last = tilt.wrapping_add(0xd); // [sp+0x14]
        if lt(first, 0) {
            first = 0;
        }
        if !lt(last, 0x11) {
            last = 0x10;
        }
        let s0 = first.wrapping_add(1);
        night_ground_row(m, s8, s8 + 0x34 + (s0 & 1) * 204, first);
        let mut row = s0; // s7
        while !lt(last, row) {
            let mut s6 = s8 + 0x34 + ((row + 1) & 1) * 204;
            let previous = s8 + 0x34 + (row & 1) * 204; // s0
            if row == last {
                let mut record = s6 + 0xc0;
                for _ in 0..17 {
                    m.sw(record, u32::MAX);
                    record = record.wrapping_sub(0xc);
                }
            } else {
                night_ground_row(m, s8, s6, row);
            }
            let mut s5 = m.lw(previous);
            let mut s4 = s6 + 4;
            let mut s1 = previous + 4;
            for _ in 0..16 {
                let a2 = s5;
                s5 = m.lw(s1 + 8);
                let v1 = m.lw(s6);
                let both = s5 & v1;
                let next = s2 + 0x20;
                if a2 != u32::MAX && both != u32::MAX && a2 & both == 0 {
                    if lt(s5 | v1, 0) {
                        // Triangle (one far corner missing).
                        let packet = s2;
                        s2 += 0x14;
                        let color = m.lw(s1 + 4);
                        let xy = m.lw(s1);
                        m.sw(packet, (s2 & 0x00ff_ffff) | 0x0400_0000);
                        m.sw(packet + 4, color | 0x5200_0000);
                        m.sw(packet + 8, xy);
                        let (color, xy) = if lt(s5, 0) {
                            (m.lw(s4 + 4), m.lw(s4))
                        } else {
                            (m.lw(s1 + 0x10), m.lw(s1 + 0xc))
                        };
                        m.sw(packet + 0xc, color);
                        m.sw(packet + 0x10, xy);
                    } else {
                        let color = m.lw(s1 + 0x10);
                        let xy = m.lw(s1 + 0xc);
                        m.sw(s2, (next & 0x00ff_ffff) | 0x0700_0000);
                        m.sw(s2 + 4, color | 0x5a00_0000);
                        m.sw(s2 + 8, xy);
                        let color = m.lw(s1 + 4);
                        let xy = m.lw(s1);
                        m.sw(s2 + 0xc, color);
                        m.sw(s2 + 0x10, xy);
                        let color = m.lw(s4 + 4);
                        let xy = m.lw(s4);
                        m.sw(s2 + 0x14, color);
                        m.sw(s2 + 0x18, xy);
                        sdk::rand(m);
                        sdk::rand(m);
                        m.sw(s2 + 0x1c, 0x5555_5555);
                        s2 = next;
                    }
                }
                s1 += 0xc;
                s4 += 0xc;
                s6 += 0xc;
            }
            row = row.wrapping_add(1);
        }
        pass += 1;
        let y = m.lw(s8 + 0x24).wrapping_add(0x800);
        m.sw(s8 + 0x24, y);
        if !lt(pass, 4) {
            break;
        }
    }
    m.sw(base + 4, s2);
    m.pop_frame(0x40);
}
