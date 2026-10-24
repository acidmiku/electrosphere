//! Terrain, fog and sky rendering: the 8003346C tree (called from 800339E0).
//!
//! Every routine is translated from the original instructions over the
//! address-preserving [`Machine`]: structure and pointer values keep their
//! original addresses, GTE work goes through the Redux-exact `ac3_gte`
//! register file, and primitives/ordering-table links are written to RAM
//! exactly as the original writes them.
//!
//! Stack frames whose addresses the original passes to callees (or stores,
//! like 8005E6DC's two vertex-row buffers kept in scratchpad 0x1F80000C and
//! 0x1F800010) live in machine RAM at their original offsets
//! (`Machine::push_frame`). Every routine with a frame that calls another
//! routine pushes its original frame size, so stack addresses stay exact.
//! Pure register spills stay in Rust. Those scratchpad words match the
//! original only when `Machine::sp` at 8003346C's entry is the original
//! `$sp` (0x801FFF98 in the captured flight frames), so the callers must push
//! their original frames too.
//!
//! Compiler-emitted division checks (`break 7` for a zero divisor, `break 6`
//! for `MIN / -1`) have no native trap handler: they raise [`Fault::Trap`]
//! with the `break` address and code and continue with the hardware
//! quotient.
//!
//! The libgte routines of the tree (NormalClip, rcos, MulMatrix0,
//! SetMulMatrix 800A6A08) and printf are `sdk.rs`'s.
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

// ---- 8003346C: the world/background pass of the flight frame -------------------------

// JP-REVIEW(R0540): exact function, calls differ: PAL 800331D4 -> JP 8003346C: instructions equal once addresses are masked, but only 10/11 calls go to the PAL callee's counterpart
/// 8003346C: terrain (one or two views), the depth masks and animated
/// palette when enabled, the depth fog, and the sky of the mission's
/// background type (gameplay +0x244 +2, low three bits). Every stage
/// advances the primitive pointer at gameplay +0x14. Returns the original
/// `v0` (the last stage's primitive pointer, or 1/4 without a sky).
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::render_world` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn render_world(m: &mut Machine) -> u32 {
    crate::jp::res_p3_terrain::render_world(m)
}

/// 8005EFF0: 16 depth-mask packets (the 24-byte template at 80024580)
/// prepended to the first 16 depth buckets of gameplay +0x288. Returns the
/// primitive pointer.
pub fn depth_masks(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    let template: [u32; 6] = std::array::from_fn(|i| m.lw(0x8002_4580 + i as u32 * 4));
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

/// 8005EDC8: animated palette entries (mission parameters +0x74 count,
/// +0x78 12-byte entries): INTPL toward the entry colour by a wave from the
/// table at 800104AC, then 16 depth-cued single-pixel VRAM writes (GP0 A0)
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
        let wave = lhu(m, ((t1 & 0x3fff) << 1).wrapping_add(0x8001_04ac));
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
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8003_346c => render_world(m),
        0x8005_e4cc => select_cells(m, a0, a1, a2),
        0x8005_e6dc => draw_terrain(m, a0, a1, a2, a3),
        0x8005_edc8 => animate_palette(m, a0, a1, a2, a3),
        0x8005_eff0 => depth_masks(m, a0, a1),
        0x8005_f124 => max_x_index(m, a0, a1),
        0x8005_f174 => min_x_index(m, a0, a1),
        0x8005_f1c4 => corners_same_side(m, a0, a1),
        0x8005_f2b4 => clip_segment(m, a0, a1, a2, a3),
        0x8005_f510 => plane_intersection(m, a0, a1, a2, a3),
        0x8005_f734 => unproject(m, a0, a1, a3),
        0x8005_f7fc => project_vector(m, a0, a1),
        0x8005_f90c => {
            view_frustum(m, a0, a1, a2, a3);
            0
        }
        0x8006_0cb8 => {
            make_grid(m, a0);
            0
        }
        0x8006_1534 => {
            view_tiles(m, a0);
            0
        }
        0x8006_1614 => {
            build_view(m, a0, a1, a2, a3);
            0
        }
        0x8006_1768 => {
            draw_cells(m, a0, a1, a2);
            0
        }
        0x8006_19e0 => {
            mesh_flat_quads(m, a0);
            0
        }
        0x8006_1c3c => {
            mesh_gouraud_quads(m, a0);
            0
        }
        0x8006_1e50 => {
            midpoint(m, a0, a1, a2);
            0
        }
        0x8006_1ed4 => {
            emit_triangle(m, a0, a1, a2, a3);
            0
        }
        0x8006_1fa8 => subdivide_level(m, a0, a1, a2, a3),
        0x8006_258c => subdivide_quad(m, a0, a1),
        0x8009_4718 => {
            sky_camera(m, a0, a1);
            0
        }
        0x8009_48a0 => {
            project_bands(m, a0, a1);
            0
        }
        0x8009_49ac => {
            classify_corners(m, a0);
            0
        }
        0x8009_4ac8 => sky_bands(m, a0, a1),
        0x8009_4d10 => {
            sky_colors(m, a0, a1);
            0
        }
        0x8009_4f50 => {
            sky_altitude_color(m, a0, a1);
            0
        }
        0x8009_5018 => {
            blend_color(m, a0, a1, a2);
            0
        }
        0x8009_5074 => {
            draw_bands(m, a0, a1);
            0
        }
        0x8009_5298 => {
            flat_band(m, a0, a1, a2);
            0
        }
        0x8009_546c => {
            gradient_band(m, a0, a1, a2);
            0
        }
        0x8009_57a4 => draw_sky(m, a0, a1),
        0x8009_5930 => {
            draw_sun(m, a0, a1);
            0
        }
        0x8009_5cb8 => {
            draw_stars(m, a0, a1, a2);
            0
        }
        0x8009_5fa0 => {
            cloud_sky_bands(m, a0, a1);
            0
        }
        0x8009_6184 => {
            draw_cloud_bands(m, a0, a1);
            0
        }
        0x8009_62b8 => draw_cloud_sky_bands(m, a0, a1),
        0x8009_63f8 => quad_too_large(a0, a1, a2, a3),
        0x8009_6518 => {
            cloud_midpoint(m, a0, a1, a2);
            0
        }
        0x8009_65d4 => screen_outcode(m, a0),
        0x8009_6630 => {
            subdivide_cloud_quad(m, a0, a1, a2, a3);
            0
        }
        0x8009_6a48 => draw_cloud_layer(m, a0, a1, a2),
        0x8009_754c => draw_cloud_sky(m, a0, a1, a2),
        0x8009_757c => {
            dome_ring(m, a0, a1, a2);
            0
        }
        0x8009_76f8 => {
            draw_dome(m);
            0
        }
        0x8009_7eb4 => draw_dome_sky(m, a0, a1),
        0x8009_8044 => draw_night_sky(m, a0, a1),
        0x8009_830c => {
            night_ring(m, a0, a1, a2);
            0
        }
        0x8009_8414 => {
            night_dome(m, a0, a1);
            0
        }
        0x8009_8714 => {
            night_ground_row(m, a0, a1, a2);
            0
        }
        0x8009_8850 => {
            night_ground(m, a0);
            0
        }
        0x8008_4978 => {
            compact_ot_runs(m, a0, a1);
            0
        }
        0x8008_faa8 => fade_overlay(m, a0, a1, a2, a3),
        _ => return None,
    })
}

// ---- terrain: 8005E6DC and its mesh renderer ------------------------------------------

/// 8005E6DC: draw the visible terrain. `params` is the mission pointer array
/// at gameplay +0x244 (+4 grid, +8 geometry directory), `view` the 80061614
/// descriptor, `ot` the depth OT (+4), `packets` the primitive pointer.
/// Returns the advanced primitive pointer.
pub fn draw_terrain(m: &mut Machine, params: u32, view: u32, ot: u32, packets: u32) -> u32 {
    let sp = m.push_frame(0x128);
    let grid = m.lw(params + 4);
    let end = select_cells(m, grid, view, packets);
    let work = SCRATCH;
    m.sw(work + 4, end);
    m.sw(work + 0xc, sp + 0x10);
    let x = m.lw(0x800b_d068);
    m.sw(work + 0x10, sp + 0x90);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(work + 0x14, x.wrapping_add(0x7c00));
    let z = m.lw(0x800b_d06c);
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

/// 8005E4CC: select the visible cells of `view` from the 256x256 signed
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

/// 80061768: transform and draw the meshes of the selected cells. `work` is
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

/// Shared face test of 800619E0/80061C3C: back-face cull with NCLIP over
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

/// 800619E0: flat-shaded textured quads (POLY_FT4) of a mesh face list
/// (`work + 8`: count, then 0x24-byte faces). Near faces are subdivided
/// through 8006258C with a Gouraud record built at `work + 0x234`.
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

/// 80061C3C: Gouraud-shaded textured quads (POLY_GT4) of a mesh face list
/// (`work + 8`: count, then 0x30-byte faces); near faces go to 8006258C.
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

/// 8006258C: set up the four-vertex buffer at `work + 0x270` (12-byte
/// vertices: xy, z, uv, colour) for a near Gouraud face `record`, depth-cue
/// its colours and subdivide it (80061FA8). Returns the primitive pointer.
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

/// 80061E50: midpoint vertex of `a` and `b` into `out` (positions averaged,
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

/// 80061ED4: emit one POLY_GT3 of three subdivision vertices unless RTPT
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

// JP-REVIEW(R0544): near function: PAL 800607DC -> JP 80061FA8, imm:2, 2 instructions differ (whole routine)
/// 80061FA8: one level of the recursive near-face subdivision. `quad` holds
/// four 12-byte vertices, +0x30.. their projections, +0x40.. their SZ.
/// Emits a POLY_GT4 when small enough, otherwise recurses into four
/// children at `quad + 0x48` (`bits` selects the split diagonal). Returns 1
/// when drawn or subdivided, 0 when rejected.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::subdivide_level` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn subdivide_level(m: &mut Machine, quad: u32, work: u32, level: u32, bits: u32) -> u32 {
    crate::jp::res_p3_terrain::subdivide_level(m, quad, work, level, bits)
}

// ---- visibility: 80061614 and the frustum/grid routines -------------------------------

/// 80061614: build the terrain view descriptor (0x118 bytes at `view`) for
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

/// 8005F124: index of the record with the largest first word among `count`
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

/// 8005F174: index of the record with the smallest first word (first
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

/// 8005F1C4: 1 when every edge of the `count` corner directions (VECTOR
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

/// 8005F2B4: Cohen-Sutherland clip of the screen segment `a`-`b` (halfword
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
            let (step, _) = div(m, product, dx, 0x8005_f3e8);
            let y0 = lhu(m, t1 + 2);
            sh(m, t1, edge);
            sh(m, t1 + 2, y0.wrapping_add(step));
        } else if code_a & 0xc != 0 {
            let edge = if code_a & 4 != 0 { low_y } else { y_limit };
            let dx = lh(m, a1).wrapping_sub(lh(m, t1));
            let y0 = lh(m, t1 + 2);
            let product = dx.wrapping_mul(edge.wrapping_sub(y0));
            let dy = lh(m, a1 + 2).wrapping_sub(y0);
            let (step, _) = div(m, product, dy, 0x8005_f468);
            let x0 = lhu(m, t1);
            sh(m, t1 + 2, edge);
            sh(m, t1, x0.wrapping_add(step));
        }
        code_a = code(m, t1);
    }
    1
}

/// 8005F510: intersect the ray `direction` (VECTOR) from `origin` (VECTOR)
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
            0x8005_f548
        } else {
            0x8005_f5f4
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

/// 8005F734: scale the screen point `point` (x, y, z halfwords) by z /
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
    let (x, _) = div(m, product, divisor_x, 0x8005_f758);
    m.sw(sp + 0x10, x);
    let z = lh(m, point + 4);
    let product = lh(m, point + 2).wrapping_mul(z);
    let (y, _) = div(m, product, divisor_y, 0x8005_f7a8);
    m.sw(sp + 0x14, y);
    let z = lh(m, point + 4);
    m.sw(sp + 0x18, z);
    let result = sdk::apply_rot_matrix_lv(m, sp + 0x10, out);
    m.pop_frame(0x28);
    result
}

/// 8005F7FC: perspective-project the VECTOR `vector` with the current
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
    let (x, _) = div(m, product, z, 0x8005_f864);
    let x = lhu(m, sp + 0x20).wrapping_add(x);
    sh(m, out, x);
    let product = m.lw(sp + 0x14).wrapping_mul(h);
    let z = m.lw(sp + 0x18);
    let (y, _) = div(m, product, z, 0x8005_f8b8);
    let y = lhu(m, sp + 0x24).wrapping_add(y);
    sh(m, out + 2, y);
    m.pop_frame(0x38);
    y
}

/// 80061534: tile origin, fractional offset and clamped grid-relative
/// outline points of the view (uses the minimum stored by 8005F90C at
/// `gp+0x4F4/0x4F8`).
pub fn view_tiles(m: &mut Machine, view: u32) {
    let min_x = m.gw(0x4f4);
    let x = m.lw(view).wrapping_add(0x8000).wrapping_add(min_x); // t2
    let min_z = m.gw(0x4f8);
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

/// 8005F90C: view frustum outline on the ground. Rotates the four screen
/// corner directions, intersects them with the ground (y 0) and the
/// -3000 plane, clips the horizon against the screen, selects up to seven
/// outline points in view order and stores their camera-relative x/z into
/// `view` +0x1C (count at +0x18). The minimum x/z go to `gp+0x4F4/0x4F8`.
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
    let (s1, _) = div(m, numerator, h_x, 0x8005_f984);
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
    let (s0, _) = div(m, numerator, h, 0x8005_fa38);
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
    // Copy the 32-byte MATRIX at 800B2BF8 to sp+0x270.
    let words: [u32; 8] = std::array::from_fn(|i| m.lw(0x800b_2bf8 + i as u32 * 4));
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

/// 8005FC5C..8005FD98: project the four plane intersections at
/// `s5 + source` (8005F7FC) into halfword pairs at `sp + pairs` and widen
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

/// 80060078..80060588: camera below the -3000 plane and not every corner
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

/// 80060644..80060A58: every corner hits a plane: order the four
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

/// 80060A5C..80060C84: rotate the selected outline points by the camera
/// heading, store them relative to their minimum (clamped to signed 16
/// bits) as view +0x1C halfword pairs, and the minimum at gp+0x4F4/0x4F8.
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
    m.set_gw(0x4f4, min_x);
    m.set_gw(0x4f8, min_z);
}

/// 80060CB8 `test_makegrid`: rasterize the view outline (view +0x1C, count
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
        sdk::printf(m, [0x8002_4598, width, height, max_z]);
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
            let (_, start) = div(m, edge, count, 0x8006_0ed4);
            let (_, end) = div(m, edge.wrapping_add(1), count, 0x8006_0f08);
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

/// 80061014..800613C0: one outline edge of 80060CB8. NCLIP against the edge
/// marks the grid corners on its inner side (+0x110 per row), then marks
/// the cells the edge crosses in the row masks (+0x90).
fn make_grid_edge(m: &mut Machine, t3: u32, s0: u32, points: u32, edge: u32, count: u32) {
    let s2 = 1_u32;
    let (_, start) = div(m, edge, count, 0x8006_1014);
    let (_, end) = div(m, edge.wrapping_add(1), count, 0x8006_1048);
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

// ---- fog: PAL 80092BCC ---------------------------------------------------------------

// ---- sky: 800957A4 and its stages ---------------------------------------------------

// JP-REVIEW(R0547): near function: PAL 80093E2C -> JP 800957A4, imm:1, 1 instructions differ (whole routine)
/// 800957A4: horizon/sky background. Scratchpad 0x1F800000 holds the OT
/// pointer and primitive pointer, 0x1F80001C the sky work block. Ends with
/// a mask-mode packet linked to `ot`. Returns the primitive pointer.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::draw_sky` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    crate::jp::res_p3_terrain::draw_sky(m, ot, packets)
}

// JP-REVIEW(R0548): changed function: PAL 80092E50 -> JP 80094718, del:1 imm:3 ins:19 op:8 reg:6, 37 instructions differ (whole routine)
/// 80094718: camera position (>> 12), angles and rotation for the sky.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::sky_camera` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn sky_camera(m: &mut Machine, base: u32, sky: u32) {
    crate::jp::res_p3_terrain::sky_camera(m, base, sky)
}

// JP-REVIEW(R0549): near function: PAL 800933CC -> JP 80094D10, del:4 imm:2 ins:17 op:2 ref:1 reg:6, 32 instructions differ (whole routine)
/// 80094D10: band colours (+0xF8..+0x10C) from mission parameters, blended
/// by camera altitude and pitch, then the optional flash tint.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::sky_colors` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn sky_colors(m: &mut Machine, base: u32, sky: u32) {
    crate::jp::res_p3_terrain::sky_colors(m, base, sky)
}

/// 80094F50: darken/brighten the top colour word at `color` for high
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

/// 80095018: blend the colour word at `color` toward `target` by `t`
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

// JP-REVIEW(R0550): changed function: PAL 800931B8 -> JP 80094AC8, del:9 imm:1 ins:22 op:18 reg:18, 68 instructions differ (whole routine)
/// 80094AC8: band edge points (+0x2C.., five x pairs at the band heights)
/// and limits for the camera altitude; returns the band count (4 or 5).
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::sky_bands` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn sky_bands(m: &mut Machine, base: u32, sky: u32) -> u32 {
    crate::jp::res_p3_terrain::sky_bands(m, base, sky)
}

// JP-REVIEW(R0551): near function: PAL 80092F90 -> JP 800948A0, imm:2, 2 instructions differ (whole routine)
/// 800948A0: project the band edges (RTPT of the two edge points per band)
/// and clip each consecutive pair to the screen (8005F2B4). Sets the
/// visible-band bits at +0x28.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::project_bands` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn project_bands(m: &mut Machine, sky: u32, bands: u32) {
    crate::jp::res_p3_terrain::project_bands(m, sky, bands)
}

/// 800949AC: classify the four screen corners against each visible band
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

// JP-REVIEW(R0552): near function: PAL 800936FC -> JP 80095074, imm:1, 1 instructions differ (whole routine)
/// 80095074: draw the sky bands (flat and gradient), stars and cloud band,
/// or a single background tile when no band edge is on screen.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::draw_bands` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_bands(m: &mut Machine, base: u32, sky: u32) {
    crate::jp::res_p3_terrain::draw_bands(m, base, sky)
}

/// 80095298: one flat-shaded sky band (POLY_F4 fan plus optional POLY_F3)
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

/// 8009546C: one Gouraud sky band between colours `band - 1` and `band`.
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
                let (fraction, _) = div(m, area << 8, divisor, 0x8009_555c);
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

// JP-REVIEW(R0553): changed function: PAL 80093FB8 -> JP 80095930, del:1 imm:6 ins:17 op:6 reg:38, 68 instructions differ (whole routine)
/// 80095930: the sun/moon strip of semi-transparent POLY_FT4 packets along
/// the horizon direction from the gameplay +0x268 block.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::draw_sun` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_sun(m: &mut Machine, base: u32, sky: u32) {
    crate::jp::res_p3_terrain::draw_sun(m, base, sky)
}

// JP-REVIEW(R0555): near function: PAL 80094300 -> JP 80095CB8, imm:1, 1 instructions differ (whole routine)
/// 80095CB8: up to 256 stars (TILE_1 runs of seven packed into 0x0E-word
/// packets) from the star table at `[gp+0x41C]`, drifting with the camera.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::draw_stars` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_stars(m: &mut Machine, base: u32, sky: u32, count: u32) {
    crate::jp::res_p3_terrain::draw_stars(m, base, sky, count)
}

// ---- background type 2: cloud layer and banded sky (8009754C) ------------------------------

/// 8009754C: background type 2. Draws the cloud layer over the terrain view
/// (80096A48), then the banded sky (800962B8). Returns the primitive
/// pointer.
pub fn draw_cloud_sky(m: &mut Machine, view: u32, ot: u32, packets: u32) -> u32 {
    m.push_frame(0x18);
    let packets = draw_cloud_layer(m, view, ot, packets);
    let result = draw_cloud_sky_bands(m, ot, packets);
    m.pop_frame(0x18);
    result
}

/// 800962B8: banded sky of background type 2 (like 800957A4 with the
/// 80095FA0 band set-up, four bands and 80096184 drawing).
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
    let flags = m.gw(0x41c);
    m.sw(base + 4, a1 + 8);
    m.sw(flags + 0x5c8, 0);
    let result = m.lw(base + 4);
    m.pop_frame(0x28);
    result
}

/// 80095FA0: band colours and the four band edges of background type 2,
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
    let (upper_width, _) = div(m, times200(upper), distance, 0x8009_6088); // t3
    lower = sra(lower, 1);
    let (lower_width, _) = div(m, times200(lower), distance, 0x8009_60cc); // t2
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

// JP-REVIEW(R0556): near function: PAL 800947CC -> JP 80096184, imm:1, 1 instructions differ (whole routine)
/// 80096184: draw the four bands of background type 2 (or one far tile
/// when none is visible and the ground colour's top byte is set).
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::draw_cloud_bands` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_cloud_bands(m: &mut Machine, base: u32, sky: u32) {
    crate::jp::res_p3_terrain::draw_cloud_bands(m, base, sky)
}

/// 800963F8: 1 when the screen bounding box of four SXY words is at least
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

/// 80096518: midpoint of two cloud vertices (0x14 bytes: position, code,
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

// JP-REVIEW(R0557): near function: PAL 80094C1C -> JP 800965D4, imm:2, 2 instructions differ (whole routine)
/// 800965D4: outcode of the screen point at `point` against the 320x256
/// screen (1 left, 2 right, 4 above, 8 below).
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::screen_outcode` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn screen_outcode(m: &mut Machine, point: u32) -> u32 {
    crate::jp::res_p3_terrain::screen_outcode(m, point)
}

/// 80096630: subdivide a cloud quad (four vertex pointers at `quad`) into
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

/// 8009693C..80096A00: one POLY_GT4 of four cloud vertices (pointers at
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

/// Cloud colour index lookup of 80096A48: nibble `(column - bias) & 7` of
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

/// One projected cloud vertex of 80096A48: FLAG, SXY2 and SZ3 of the last
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

/// 80096A48: the cloud layer of background type 2 over the visible grid of
/// `view` (row masks, bounds and camera position from 80061614). Clouds are
/// 2048-unit cells at height -0xC00..-0x800 coloured from the row patterns
/// at 800B45F4; edge rows/columns fade through the +0x8C/+0x90 states. Near
/// cells are subdivided (80096630). Packets are chained forward and linked
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

/// 800974D4..80097518: link the forward chain into `ot`.
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

/// 80096BF0..800974B8: one grid row of the cloud layer.
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
            let entry = ((s7.wrapping_add(edge).wrapping_sub(6) & 0xf) << 3) + 0x800b_45f4;
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
        // the delay slot of its jump to 8009749C).
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

/// 80096D48..80097498: one grid cell of a cloud row (column `s6`). `s5`
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
        // Subdivide the cell through 80096630 with corner uvs.
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

// ---- background type 3: lit dome (80097EB4) ------------------------------------------------

// JP-REVIEW(R0558): near function: PAL 800964FC -> JP 80097EB4, imm:2, 2 instructions differ (whole routine)
/// 80097EB4: background type 3. A black backdrop tile, 256 stars and the lit
/// dome (800976F8), closed by the mask-mode packet linked to `ot`. Returns
/// the primitive pointer.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::draw_dome_sky` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_dome_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    crate::jp::res_p3_terrain::draw_dome_sky(m, ot, packets)
}

/// 8009757C: one dome ring of 24 vertices (+ the closing copy of the first)
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

/// Texture coordinates of a dome quad corner pair (8009795C..800979A8 and
/// 80097B6C..80097BB8): offsets from the uv halfwords of `a`/`b`, the tpage.
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

/// 800976F8: the lit dome of background type 3 in scratchpad
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
        let pair = m.lw((index << 2).wrapping_add(0x800b_7104));
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

// ---- background type 4: night sky with fade overlay (80098044) -------------------------------

// JP-REVIEW(R0560): near function: PAL 8009668C -> JP 80098044, imm:2, 2 instructions differ (whole routine)
/// 80098044: background type 4. A clock-driven fade overlay (8008FAA8), a
/// black backdrop, the horizon dome (80098414) and ground grid (80098850),
/// the mask-mode packets, then the OT run compaction (80084978). Returns the
/// primitive pointer.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::draw_night_sky` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn draw_night_sky(m: &mut Machine, ot: u32, packets: u32) -> u32 {
    crate::jp::res_p3_terrain::draw_night_sky(m, ot, packets)
}

/// 80084978: shorten runs of empty entries in a reverse-linked OT of `count`
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

// JP-REVIEW(R0561): near function: PAL 8008E238 -> JP 8008FAA8, imm:2, 2 instructions differ (whole routine)
/// 8008FAA8: full-screen fade overlay: a mask packet, an optional draw
/// offset (from `environment`, when non-zero), two 256-wide subtractive and
/// additive sprites/tiles in the depth-cued grey of `brightness`, chained
/// and linked to `ot`. Returns the primitive pointer.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::fade_overlay` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn fade_overlay(
    m: &mut Machine,
    ot: u32,
    packets: u32,
    environment: u32,
    brightness: u32,
) -> u32 {
    crate::jp::res_p3_terrain::fade_overlay(m, ot, packets, environment, brightness)
}

/// 8009830C: one horizon-dome ring of 20 vertices (+ the closing copy) at
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

/// 80098414: the horizon dome of background type 4: saves the rotation,
/// sets a clock-dependent projection distance and depth cue (8005E7EC),
/// builds the 20-step circle table and draws 24 rings of POLY_GT4 between
/// consecutive rings (8009830C); restores the rotation and distance.
pub fn night_dome(m: &mut Machine, base: u32, clock: u32) {
    m.push_frame(0x38);
    let s4 = base + 0x1c;
    let rotation: [u32; 5] = std::array::from_fn(|i| m.gte.cfc2(ctrl::R11R12 + i));
    for (i, word) in rotation.into_iter().enumerate() {
        m.sw(s4 + i as u32 * 4, word);
    }
    let table = 0x800b_7106_u32; // s0
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
    crate::dispatch::call(m, 0x8005_e7ec, [0x2710, 0x4e20, distance, 0]);
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

// JP-REVIEW(R0562): near function: PAL 80096D5C -> JP 80098714, imm:3, 3 instructions differ (whole routine)
/// 80098714: one ground-grid row (16 vertices + an end marker) at grid row
/// `row` around the camera cell (`block` +0x20..+0x28), 12-byte records:
/// outcode (or -1), SXY, depth-cued colour.
// JP-PATCH: replaced by `crate::jp::res_p3_terrain::night_ground_row` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn night_ground_row(m: &mut Machine, block: u32, out: u32, row: u32) {
    crate::jp::res_p3_terrain::night_ground_row(m, block, out, row)
}

/// 80098850: the ground grid of background type 4 below the camera: four
/// passes of 16x16 cells (POLY_G4 quads, G3 triangles at the edge),
/// depth cue from 8005E7EC, two discarded rand() calls per quad.
pub fn night_ground(m: &mut Machine, base: u32) {
    m.push_frame(0x40);
    let s8 = base + 0x1c;
    let distance = lh(m, base + 0x1a);
    far_color(m, 0, 0, 0);
    crate::dispatch::call(m, 0x8005_e7ec, [0x7d0, 0x4e20, distance, 0]);
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
