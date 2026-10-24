//! Patches of `render` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! Most differences are the NTSC screen: 240 lines instead of 256, so the
//! screen centre 128 becomes 120 (and the clipping bounds +-128/129 become
//! +-120/121, the frustum's +-0x98 becomes +-0x90), full-screen heights
//! 0x100 become 0xF0 and the vertical scale factors of the loading screens
//! change. Others: the context fields from +0x390 (4 bytes lower), the JP
//! entity kinds, the ribbon fringes (twice as wide), the sun (see
//! [`draw_sun`]) and the replay camera (450 frames per shot, PAL 750).
//! 80033324 (the object systems 80085204 in place of PAL-only 80083B78) is
//! jp-res1's (`res_p1_render`).
//!
//! The private items after the imports are copies of the generated module's
//! (same semantics), the ones these routines use.
use crate::render::{
    chase_camera, debug_camera, debug_orbit_camera, free_camera, lens_flare, loading_tiles,
    pull_away_camera, pull_back_camera, sway_camera, swing_camera,
};
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY},
    dispatch,
    machine::{Fault, Machine, SmallData},
    sdk, shared, widescreen,
};
use ac3_gte::{ctrl, reg};

/// Scratchpad base: the renderers keep their working state there.
const SCRATCH: u32 = 0x1f80_0000;

/// RTPS sf=1.
const RTPS: u32 = 0x018_0001;

/// RTPT sf=1.
const RTPT: u32 = 0x028_0030;

/// NCLIP.
const NCLIP: u32 = 0x140_0006;

/// MVMVA sf=1, rotation matrix x IR vector, no translation.
const MVMVA_RT_IR: u32 = 0x049_e012;

/// INTPL sf=1.
const INTPL: u32 = 0x098_0011;

fn cop2(m: &mut Machine, code: u32) {
    m.gte
        .command(code)
        .expect("translated GTE command words are valid");
}

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

/// `[gp+0x2D8]`: 32-entry table of secondary object pointers.
const OBJECTS: u32 = 0x2d8;

/// `[0x800BD080]`: mission object block (ground targets, +5448 visibility mask).
const MISSION_OBJECTS: u32 = 0x800b_d080;

/// DPCS sf=1.
const DPCS: u32 = 0x078_0010;

/// DPCT sf=1.
const DPCT: u32 = 0x0f8_002a;

/// GPL sf=1.
const GPL: u32 = 0x1a8_003e;

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

/// `[gp+0x53C]`: trail block. +0 is the active mask of 32 smoke trails
/// (2312 bytes each from +4), +0x12124 the mask of 32 ribbon trails (1288
/// bytes each from +0x12128).
const TRAILS: u32 = 0x53c;

/// Screen work slot of 8007CFA4 (24 bytes from 0x1F800000): +0 point
/// record, +4/+8 the two edge SXY, +12 depth, +16 colour, +20 detail shift.
const RIBBON_SLOT: u32 = 24;

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

/// One trail of 8007CFA4.
fn draw_ribbon_trail(m: &mut Machine, view: &RibbonView, trail: u32, primitives: u32) -> u32 {
    let mut packet = primitives;
    let mut count = 0u32;
    let mut near = u32::MAX;
    let mut visible = u32::MAX;
    let [eye_x, eye_y, eye_z] = view.eye;
    // Pass 1: project the midpoint of each point's two edges.
    let mut point_index = m.lw(trail + 4);
    let mut slot = SCRATCH;
    if slt(point_index, m.lw(trail)) {
        loop {
            let ring = point_index & 0x1f;
            let point = trail + (ring << 5) + 264;
            let x = sra(m.lw(point).wrapping_add(m.lw(point + 16)), 1);
            let y = sra(m.lw(point + 4).wrapping_add(m.lw(point + 20)), 1);
            let z = sra(m.lw(point + 8).wrapping_add(m.lw(point + 24)), 1);
            let x = sra(x.wrapping_sub(eye_x), 10);
            let y = sra(y.wrapping_sub(eye_y), 10);
            let z = sra(z.wrapping_sub(eye_z), 10);
            let distance = sra(abs(x).wrapping_add(abs(y)).wrapping_add(abs(z)), 2);
            if !slt(0xb000, distance) {
                let shift = if slt(view.range, distance) {
                    2
                } else {
                    u32::from(slt(sra(view.range, 1), distance))
                };
                let (x, y, z) = (sra(x, shift), sra(y, shift), sra(z, shift));
                m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
                m.gte.mtc2(reg::VZ0, z);
                cop2(m, RTPS);
                m.sw(slot + 20, shift);
                m.sw(slot, point);
                let bit = 1u32 << (count & 31);
                let life = m.lw(trail + (ring << 2) + 136);
                let mut grey = if slt(view.near, distance) {
                    near &= !bit;
                    sra(life, 1)
                } else {
                    life
                };
                if !slt(grey, 33) {
                    grey = 32;
                }
                let sz = m.gte.mfc2(reg::SZ3);
                let sxy = m.gte.mfc2(reg::SXY2);
                let sx = screen_x(sxy);
                let depth = sra(sz, 2u32.wrapping_sub(shift));
                let sy = screen_y(sxy);
                if slt(depth, sra(view.range, 3)) {
                    grey = sra(grey, 1);
                }
                if slt(depth, sra(view.range, 4)) {
                    grey = sra(grey, 1);
                }
                if slt(depth, sra(view.range, 5)) {
                    grey = 0;
                }
                let colour = grey | (grey << 8) | (grey << 16);
                m.sw(slot + 16, colour);
                if slt(view.half_w, abs(sx)) {
                    visible &= !bit;
                }
                if slt(view.half_h, abs(sy)) {
                    visible &= !bit;
                }
                if (abs(depth) as i32) <= 0 {
                    visible &= !bit;
                }
                m.sw(slot + 12, depth);
                count += 1;
                slot += RIBBON_SLOT;
            }
            point_index = point_index.wrapping_add(1);
            if !slt(point_index, m.lw(trail)) {
                break;
            }
        }
    }
    near |= sra(near, 1) | (near << 1);
    visible |= sra(visible, 1) | (visible << 1);
    // Pass 2: project both edges of each visible point.
    if (count as i32) > 0 {
        let hidden = !visible;
        let mut slot = SCRATCH;
        for index in 0..count {
            if hidden & (1 << (index & 31)) == 0 {
                let point = m.lw(slot);
                let shift = m.lw(slot + 20).wrapping_add(10);
                for (i, offset) in [0u32, 16].into_iter().enumerate() {
                    let x = sra(m.lw(point + offset).wrapping_sub(eye_x), shift);
                    let y = sra(m.lw(point + offset + 4).wrapping_sub(eye_y), shift);
                    let z = sra(m.lw(point + offset + 8).wrapping_sub(eye_z), shift);
                    m.gte.mtc2(reg::VXY0 + i * 2, (x & 0xffff) | (y << 16));
                    m.gte.mtc2(reg::VZ0 + i * 2, z);
                }
                cop2(m, RTPT);
                swc2(m, reg::SXY0, slot + 4);
                swc2(m, reg::SXY1, slot + 8);
            }
            slot += RIBBON_SLOT;
        }
    }
    // Pass 3: emit primitives for consecutive visible pairs.
    let mut previous = RibbonPoint::load(m, SCRATCH);
    if slt(1, count) {
        let hidden = !visible;
        let mut slot = SCRATCH + RIBBON_SLOT;
        for index in 1..count {
            let pair = 3u32 << ((index - 1) & 31);
            let current = RibbonPoint::load(m, slot);
            if hidden & pair == 0 && slt(previous.depth, 22528) {
                let ot_slot = view.ot.wrapping_add(sra(previous.depth, 3) << 2);
                if !near & pair != 0 {
                    packet = ribbon_lines(m, &previous, &current, ot_slot, packet);
                } else {
                    packet = ribbon_strips(m, &previous, &current, ot_slot, packet);
                }
            }
            previous = current;
            slot += RIBBON_SLOT;
        }
    }
    packet
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

/// 8007D478..8007D4F4: two LINE_G2 (code 0x52) along the ribbon edges, the
/// second linked first.
fn ribbon_lines(
    m: &mut Machine,
    from: &RibbonPoint,
    to: &RibbonPoint,
    slot: u32,
    packet: u32,
) -> u32 {
    let code = from.colour | 0x5200_0000;
    m.sh(packet + 8, from.x0 as u16);
    m.sh(packet + 10, from.y0 as u16);
    m.sh(packet + 16, to.x0 as u16);
    m.sh(packet + 18, to.y0 as u16);
    m.sh(packet + 28, from.x1 as u16);
    m.sh(packet + 30, from.y1 as u16);
    m.sh(packet + 38, to.y1 as u16);
    m.sh(packet + 36, to.x1 as u16);
    m.sw(packet + 4, code);
    m.sw(packet + 12, to.colour);
    m.sw(packet + 24, code);
    m.sw(packet + 32, to.colour);
    let tag = m.lw(slot) | 0x0400_0000;
    m.sw(packet, tag);
    let first = link(packet);
    m.sw(slot, first);
    m.sw(packet + 20, first | 0x0400_0000);
    m.sw(slot, link(packet + 20));
    packet + 40
}

/// INTPL of the screen point `(x, y)` toward the far colour registers
/// (set by the caller) at factor `ratio`; returns IR1/IR2.
fn feather(m: &mut Machine, ratio: u32, x: u32, y: u32) -> (u32, u32) {
    m.gte.mtc2(reg::IR0, ratio);
    m.gte.mtc2(reg::IR1, x);
    m.gte.mtc2(reg::IR2, y);
    cop2(m, INTPL);
    let x = m.gte.mfc2(reg::IR1);
    let y = m.gte.mfc2(reg::IR2);
    (x, y)
}

/// `[gp+0x41C]`: object system block (+1480 sun/flare state, +1536 the
/// three 896-byte object lists, +4248 ten projectile lists, +13248 the
/// debris list, +14664 the lock-on marker, +14692 the explosion list); the
/// block itself starts with the 16 ambient particle positions.
const OBJECT_SYSTEMS: u32 = 0x41c;

/// `[gp+0x33C]`: sun state (+4 negated sun height).
const SUN_STATE: u32 = 0x33c;

/// Object renderers by object class (800854B8).
const OBJECT_RENDERERS: u32 = 0x800b_439c;

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

/// `mult` + `mfhi`.
fn mul_hi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}

/// SDK RotMatrix (800A7488), by original address (not in `sdk.rs` yet).
fn rot_matrix(m: &mut Machine, angles: u32, matrix: u32) {
    dispatch::call(m, 0x800a_7488, [angles, matrix, 0, 0]);
}

/// SDK ScaleMatrix (800A6F08), by original address (not in `sdk.rs` yet).
fn scale_matrix(m: &mut Machine, matrix: u32, scale: u32) {
    dispatch::call(m, 0x800a_6f08, [matrix, scale, 0, 0]);
}

/// Game context (`[gp+0x2EC]`).
fn game_context(m: &mut Machine) -> u32 {
    m.gw(crate::common::CONTEXT)
}

/// Word at `offset` of the game context.
fn context_word(m: &mut Machine, offset: u32) -> u32 {
    let context = game_context(m);
    m.lw(context + offset)
}

/// Byte at `offset` of the game context.
fn context_byte(m: &mut Machine, offset: u32) -> u8 {
    let context = game_context(m);
    m.lbu(context + offset)
}

/// Entity pointer `index` of `[gp+0x2E8]`.
fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw(table.wrapping_add(index << 2))
}

/// Wrap a halfword angle into -8192..8191 (one correction each way).
fn wrap_angle(m: &mut Machine, address: u32) {
    if !slt(lh(m, address), 8192) {
        let value = lhu(m, address).wrapping_sub(16384);
        m.sh(address, value as u16);
    }
    if slt(lh(m, address), (-8192i32) as u32) {
        let value = lhu(m, address).wrapping_add(16384);
        m.sh(address, value as u16);
    }
}

/// 800793C4: camera mode switch to `mode` on `target` (translated
/// elsewhere; called by original address).
fn switch_camera(m: &mut Machine, camera: u32, mode: u32, target: u32) {
    dispatch::call(m, 0x8007_93c4, [camera, mode, target, 0]);
}

/// 8007B9A4..8007BB2C: attach the camera to its object (+52): position
/// plus offset (+56..+64), angles plus offset (+72..+76), wrapped.
fn follow_object(m: &mut Machine, camera: u32) {
    let object = m.lw(camera + 52);
    for offset in [0u32, 4, 8] {
        let value = m.lw(object + offset);
        m.sw(camera + 4 + offset, value);
    }
    for offset in [0u32, 4, 8] {
        let value = m
            .lw(camera + 4 + offset)
            .wrapping_add(m.lw(camera + 56 + offset));
        m.sw(camera + 4 + offset, value);
    }
    for offset in [0u32, 2, 4] {
        let value = m.lhu(object + 16 + offset);
        m.sh(camera + 20 + offset, value);
    }
    for offset in [0u32, 2, 4] {
        let value = lhu(m, camera + 20 + offset).wrapping_add(lhu(m, camera + 72 + offset));
        m.sh(camera + 20 + offset, value as u16);
    }
    let pitch = camera + 20;
    let bump = |m: &mut Machine, address: u32| {
        let value = lhu(m, address).wrapping_add(8192);
        m.sh(address, value as u16);
    };
    if !slt(lh(m, pitch), 4097) {
        let value = 8192u32.wrapping_sub(lhu(m, pitch));
        m.sh(pitch, value as u16);
        bump(m, camera + 22);
        bump(m, camera + 24);
    }
    if slt(lh(m, pitch), (-4096i32) as u32) {
        let value = (-8192i32 as u32).wrapping_sub(lhu(m, pitch));
        m.sh(pitch, value as u16);
        bump(m, camera + 22);
        bump(m, camera + 24);
    }
    wrap_angle(m, camera + 22);
    wrap_angle(m, camera + 24);
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

/// Sine/cosine table (cos, sin halfword pairs, 4096 steps).
const SINES: u32 = 0x800b_7104;

/// GPF sf=1.
const GPF: u32 = 0x198_003d;

/// MVMVA sf=1, rotation x V0, no translation.
const MVMVA_RT_V0: u32 = 0x048_6012;

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

/// Projectile position relative to the eye in 4096-unit steps, or None when
/// any axis is outside -0x8000..0x7FFF.
fn projectile_offset(m: &mut Machine, projectile: u32, work: u32) -> Option<(u32, u32, u32)> {
    let mut out = [0u32; 3];
    for (i, offset) in [4u32, 8, 12].into_iter().enumerate() {
        let value = sra(
            m.lw(projectile + offset)
                .wrapping_sub(m.lw(work + 4 + offset)),
            12,
        );
        if value.wrapping_add(0x8000) > 0xffff {
            return None;
        }
        out[i] = value;
    }
    Some((out[0], out[1], out[2]))
}

/// OT depth clamp of the projectile renderers: 4094 above, 0 below.
fn clamp_otz(otz: u32) -> u32 {
    if !slt(otz, 4095) {
        4094
    } else if (otz as i32) < 0 {
        0
    } else {
        otz
    }
}

/// Decoders of the packed 11-bit exhaust vertex fields.
fn field_low(word: u32) -> u32 {
    sra(word << 21, 21)
}

fn field_mid(word: u32) -> u32 {
    sra(word << 11, 21)
}

fn field_high(word: u32) -> u32 {
    sra(word, 21)
}

/// One store of a decoded vertex: the halfword offset and its field decoder.
type FieldStore = (u32, fn(u32) -> u32);

/// Store a decoded vertex triple (x at +0, y at +2, z at +4 of `slot`) in
/// the original store order: the field decoders are given per axis.
fn store_exhaust_vertex(m: &mut Machine, slot: u32, word: u32, order: [FieldStore; 3]) {
    for (offset, decode) in order {
        m.sh(slot + offset, decode(word) as u16);
    }
}

/// `lwc2` VXY0..VZ2 from the three decoded vertices at `vertices`.
fn load_exhaust_vertices(m: &mut Machine, vertices: u32) {
    for (i, register) in [
        reg::VXY0,
        reg::VZ0,
        reg::VXY1,
        reg::VZ1,
        reg::VXY2,
        reg::VZ2,
    ]
    .into_iter()
    .enumerate()
    {
        lwc2(m, register, vertices + i as u32 * 4);
    }
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

/// `[gp+0x6C]`: the loading-screen state.
const GP_LOADING: u32 = 0x6c;

/// 800104AC: the 1.14 sine table.
const SINE: u32 = 0x8001_04ac;

/// MVMVA sf=1, rotation x V0 + translation.
const MVMVA_RT_V0_TR: u32 = 0x048_0012;

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

/// Link primitive `prim` (whose word 0 keeps its length byte) at the head
/// of ordering-table slot `ot`.
fn link_prim(m: &mut Machine, ot: u32, prim: u32) {
    let head = m.lw(prim);
    let next = m.lw(ot);
    m.sw(prim, (head & 0xff00_0000) | (next & 0x00ff_ffff));
    let next = m.lw(ot);
    m.sw(ot, (next & 0xff00_0000) | link(prim));
}

/// SQR sf=0.
const SQR0: u32 = 0x0a0_0428;

/// OP sf=0.
const OP0: u32 = 0x170_000c;

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

/// The relative position (>> 12) of a debris piece's world point +4 from
/// the eye (scratch +8), or `None` beyond 16 bits.
fn debris_relative(m: &mut Machine, piece: u32) -> Option<[u32; 3]> {
    let mut relative = [0u32; 3];
    for (axis, value) in relative.iter_mut().enumerate() {
        let offset = axis as u32 * 4;
        *value = sra(
            m.lw(piece + 4 + offset)
                .wrapping_sub(m.lw(SCRATCH + 8 + offset)),
            12,
        );
        if value.wrapping_add(0x8000) > 0xffff {
            return None;
        }
    }
    Some(relative)
}

/// The OT depth of a debris sprite, clamped to 0..0xFFE.
fn debris_depth(depth: u32) -> u32 {
    if !slt(depth, 0xfff) {
        0xffe
    } else if slt(depth, 0) {
        0
    } else {
        depth
    }
}

/// The ray count shift of the debris stars by projected radius: 3 below
/// 5, 2 below 10, 1 below 44, else 0 (32 >> shift rays).
fn ray_shift(radius: u32) -> u32 {
    if slt(radius, 5) {
        3
    } else if slt(radius, 10) {
        2
    } else {
        u32::from(slt(radius, 0x2c))
    }
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

/// A star ray point: the centre work +0x18/+0x1A plus the 32-entry sine
/// table at work +0x24 (cosines 8 entries on) scaled by `radius`.
fn ray_point(m: &mut Machine, work: u32, index: u32, radius: u32) -> (u32, u32) {
    let dx = scale_q12(lh(m, work + 0x24 + index * 2), radius);
    // Widescreen: the ray offset squeezed like the projection.
    let dx = widescreen::squeeze_x(m, dx);
    let x = u32::from(m.lhu(work + 0x18)).wrapping_add(dx);
    let dy = scale_q12(lh(m, work + 0x24 + (index + 8) * 2), radius);
    let y = u32::from(m.lhu(work + 0x1a)).wrapping_add(dy);
    (x, y)
}

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

/// Trail renderer body of 80090BF8, 80090ED0 and 80091370 (JP): each visible
/// segment (both ends beyond a quarter of the projection distance) becomes a
/// POLY_GT3 quad strip piece built by 8008DE18 from the two screen ends and
/// their projected widths; culled by the y bounds 121/-120 (PAL 129/-128).
fn draw_trails(m: &mut Machine, list: u32, ot: u32, primitives: u32, style: &TrailStyle) -> u32 {
    let mut record = list;
    let mut packet = primitives;
    let mut timer = m.lw(record + 12);
    if (timer as i32) < 0 {
        return packet;
    }
    m.gte.mtc2(reg::RGBC, 0x2e83_8383);
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0);
    }
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    let raw = lhu(m, camera + 36) << 16;
    let width = (style.width)(raw);
    let near = sra(raw, 18);
    loop {
        if (timer as i32) < 0 {
            return packet;
        }
        'segment: {
            if timer == 0 {
                break 'segment;
            }
            let z0 = lh(m, record + 8);
            let z1 = lh(m, record + 10);
            if !slt(near, z0) || !slt(near, z1) {
                break 'segment;
            }
            m.gte
                .mtc2(reg::IR0, fade(timer, style.fade_limit, style.fade_shift));
            let xy0 = m.lw(record);
            let xy1 = m.lw(record + 4);
            cop2(m, DPCS);
            m.sw(SCRATCH + 12, xy0);
            let mut w0 = div_lo(width, z0);
            m.sw(SCRATCH + 8, xy1);
            let mut w1 = div_lo(width, z1);
            if (w0 as i32) <= 0 {
                w0 = 1;
            }
            if (w1 as i32) <= 0 {
                w1 = 1;
            }
            m.sw(SCRATCH + 16, w1);
            m.sw(SCRATCH + 20, w0);
            swc2(m, reg::RGB2, packet + 4);
            let x1 = lh(m, SCRATCH + 8);
            if !slt(x1.wrapping_add(w0), 161) {
                let x0 = lh(m, SCRATCH + 12);
                if !slt(x0.wrapping_add(w1), 161) {
                    break 'segment;
                }
            }
            if slt(x1.wrapping_sub(w0), (-160i32) as u32) {
                let x0 = lh(m, SCRATCH + 12);
                if slt(x0.wrapping_sub(w1), (-160i32) as u32) {
                    break 'segment;
                }
            }
            let y1 = lh(m, SCRATCH + 10);
            if !slt(y1.wrapping_add(w0), 121) {
                let y0 = lh(m, SCRATCH + 14);
                if !slt(y0.wrapping_add(w1), 121) {
                    break 'segment;
                }
            }
            if slt(y1.wrapping_sub(w0), (-120i32) as u32) {
                let y0 = lh(m, SCRATCH + 14);
                if slt(y0.wrapping_sub(w1), (-120i32) as u32) {
                    break 'segment;
                }
            }
            let slot = (sra(z0.wrapping_add(z1), 4) << 2).wrapping_add(ot);
            add_prim(m, slot, packet, 0x0900_0000);
            m.sh(packet + 28, style.half28);
            m.sw(packet + 20, style.word20);
            m.sw(packet + 12, style.word12);
            m.sh(packet + 36, style.half36);
            build_trail_quad(m, packet, SCRATCH);
            packet += 40;
        }
        record += 16;
        timer = m.lw(record + 12);
    }
}

/// 8007D574..8007D7E0 (part of 8007CFA4, JP): four POLY_G4 (code 0x3A) across the
/// ribbon: a transparent fringe beyond each edge (INTPL -192 and 4288 along the
/// edge-to-edge direction; PAL -96 and 4192) and the two inner strips (192,
/// 3904; PAL 96, 4000), lit by the point colours.
fn ribbon_strips(
    m: &mut Machine,
    from: &RibbonPoint,
    to: &RibbonPoint,
    slot: u32,
    packet: u32,
) -> u32 {
    let a = packet + 108;
    m.gte.ctc2(ctrl::RFC, from.x1);
    m.gte.ctc2(ctrl::GFC, from.y1);
    let (x, y) = feather(m, (-192i32) as u32, from.x0, from.y0);
    m.sh(a - 100, x as u16);
    m.sh(a - 98, y as u16);
    m.sh(a - 92, from.x0 as u16);
    m.sh(a - 90, from.y0 as u16);
    m.sh(a - 64, from.x0 as u16);
    m.sh(a - 62, from.y0 as u16);
    let (x, y) = feather(m, 192, from.x0, from.y0);
    m.sh(a - 56, x as u16);
    m.sh(a - 54, y as u16);
    let (x, y) = feather(m, 3904, from.x0, from.y0);
    m.sh(a - 28, x as u16);
    m.sh(a - 26, y as u16);
    m.sh(a - 20, from.x1 as u16);
    m.sh(a - 18, from.y1 as u16);
    m.sh(a + 8, from.x1 as u16);
    m.sh(a + 10, from.y1 as u16);
    let (x, y) = feather(m, 4288, from.x0, from.y0);
    m.sh(a + 16, x as u16);
    m.sh(a + 18, y as u16);
    m.gte.ctc2(ctrl::RFC, to.x1);
    m.gte.ctc2(ctrl::GFC, to.y1);
    let (x, y) = feather(m, (-192i32) as u32, to.x0, to.y0);
    m.sh(a - 84, x as u16);
    m.sh(a - 82, y as u16);
    m.sh(a - 76, to.x0 as u16);
    m.sh(a - 74, to.y0 as u16);
    m.sh(a - 48, to.x0 as u16);
    m.sh(a - 46, to.y0 as u16);
    let (x, y) = feather(m, 192, to.x0, to.y0);
    m.sh(a - 40, x as u16);
    m.sh(a - 38, y as u16);
    let (x, y) = feather(m, 3904, to.x0, to.y0);
    m.sh(a - 12, x as u16);
    m.sh(a - 10, y as u16);
    m.sh(a - 2, to.y1 as u16);
    m.sh(a - 4, to.x1 as u16);
    m.sh(a + 26, to.y1 as u16);
    m.sh(a + 24, to.x1 as u16);
    let (x, y) = feather(m, 4288, to.x0, to.y0);
    m.sh(a + 32, x as u16);
    m.sh(a + 34, y as u16);
    let code = 0x3a00_0000u32;
    let lit = from.colour | code;
    m.sw(a - 104, code);
    m.sw(a - 88, 0);
    m.sw(a - 80, to.colour);
    m.sw(a - 60, 0);
    m.sw(a - 52, to.colour);
    m.sw(a - 44, 0);
    m.sw(a - 32, code);
    m.sw(a - 96, from.colour);
    m.sw(a - 68, lit);
    m.sw(a - 24, from.colour);
    m.sw(a - 16, 0);
    m.sw(a - 8, to.colour);
    m.sw(a + 4, lit);
    m.sw(a + 12, 0);
    m.sw(a + 20, to.colour);
    m.sw(a + 28, 0);
    let tag = m.lw(slot) | 0x0800_0000;
    m.sw(packet, tag);
    let mut previous = link(packet);
    m.sw(slot, previous);
    for offset in [36u32, 72, 108] {
        m.sw(packet + offset, previous | 0x0800_0000);
        previous = link(packet + offset);
        m.sw(slot, previous);
    }
    packet + 144
}

/// Load the rotation and the translation of the MATRIX at gameplay +0x4C
/// (the JP epilogue of the sun and moon renderers; PAL's [`restore_view`]
/// cleared the translation instead).
fn restore_view_matrix(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    load_rotation(m, gameplay + 0x4c);
    load_translation(m, gameplay + 0x4c + 0x14);
}

/// `value / 312` as compiled (magic 0xD20D20D3, the product's high word plus
/// the dividend, shift 8).
fn div312(value: u32) -> u32 {
    let high = mul_hi(value, 0xd20d_20d3);
    sra(high.wrapping_add(value), 8).wrapping_sub(sra(value, 31))
}

/// 8008BA8C (JP; PAL 8008A310, changed): the sun, in its 0x38-byte frame. A
/// POLY_FT4 disc at the projected sun direction (state +8, stored at the
/// work area first and loaded from there); a negative GTE flag ends it
/// before the projection is stored (PAL stored it first), and the disc must
/// lie within |x| < 177, |y| < 137 (PAL 145). When the sun's high byte is
/// set: the four-sided corona (state +16..+44), its brightness
/// `(312 - (|x| + |y|)) * 4096 / 312` (PAL 320), and a rotating glow of
/// grey and size from `208 - (|x| + |y|)`: two RotMatrix frames, the
/// glow's three SVECTORs built in the scratchpad at +0x4C..+0x62 and loaded
/// from there (PAL loaded the GTE directly), 80086948 between faces, then
/// the view rotation and translation reloaded from gameplay +0x4C (PAL
/// cleared the translation); all chained at OT slot 4094. Then the screen
/// flash (a 320x240 fill packet from (-160, -120), PAL 256 lines from
/// -128, or the blend value) and the lens flare (8008B7AC), reduced when
/// the sun is below the eye. `state` is object systems +1480.
///
/// Widescreen (off by default): as in the PAL translation, the brightness
/// and glow take |x| unsqueezed, and the glow's centre goes into TRX
/// unsqueezed.
pub fn draw_sun(m: &mut Machine, state: u32, sun: u32, height: u32) {
    m.push_frame(0x38);
    'body: {
        let xy = m.lw(state + 8);
        let z = m.lw(state + 12);
        m.sw(WORK, xy);
        m.sw(WORK + 4, z);
        lwc2(m, reg::VXY0, WORK);
        lwc2(m, reg::VZ0, WORK + 4);
        cop2(m, RTPS);
        let mut packet = m.lw(SCRATCH + 4);
        m.sw(packet + 20, 0x0039_209f);
        m.sh(packet + 28, 0x3f80);
        m.sw(packet + 4, 0x2e80_8080);
        m.sw(packet + 12, 0x4f16_2080);
        m.sh(packet + 36, 0x3f9f);
        let flags = m.gte.cfc2(ctrl::FLAG);
        if (flags as i32) < 0 {
            break 'body;
        }
        swc2(m, reg::SXY2, WORK + 8);
        let ax = abs(lh(m, WORK + 8));
        if !slt(ax, 177) {
            break 'body;
        }
        let ay = abs(lh(m, WORK + 10));
        if !slt(ay, 137) {
            break 'body;
        }
        let offset = widescreen::unsqueeze_x(m, ax).wrapping_add(ay);
        if sun & 0xff00 != 0 {
            for (register, word) in [
                (reg::VXY0, 16u32),
                (reg::VZ0, 20),
                (reg::VXY1, 24),
                (reg::VZ1, 28),
                (reg::VXY2, 32),
                (reg::VZ2, 36),
            ] {
                lwc2(m, register, state + word);
            }
            cop2(m, RTPT);
            let brightness = div312(312u32.wrapping_sub(offset) << 12);
            m.sw(WORK + 12, brightness);
            swc2(m, reg::SXY0, packet + 8);
            swc2(m, reg::SXY1, packet + 16);
            swc2(m, reg::SXY2, packet + 24);
            lwc2(m, reg::VXY0, state + 40);
            lwc2(m, reg::VZ0, state + 44);
            cop2(m, RTPS);
            let next = packet + 40;
            let mut link_word = next << 8;
            m.sw(packet, (link_word >> 8) | 0x0900_0000);
            let ax = abs(lh(m, WORK + 8));
            let ay = abs(lh(m, WORK + 10));
            let glow = 208u32.wrapping_sub(widescreen::unsqueeze_x(m, ax).wrapping_add(ay));
            swc2(m, reg::SXY2, packet + 32);
            packet = next;
            if (glow as i32) > 0 {
                let x = lhu(m, WORK + 8);
                let y = lhu(m, WORK + 10);
                m.sw(WORK + 48, 0);
                m.sh(WORK + 52, ((x << 2).wrapping_add(y << 3)) as u16);
                rot_matrix(m, SCRATCH + 0x4c, SCRATCH + 0x64);
                load_rotation(m, SCRATCH + 0x64);
                let quarter = sra(glow, 2);
                let grey = quarter + 23;
                let size = quarter + 14;
                m.sw(WORK + 16, (grey << 16) | (grey << 8) | grey | 0x2e00_0000);
                m.sh(WORK + 52, 1024);
                rot_matrix(m, SCRATCH + 0x4c, SCRATCH + 0x84);
                let x = lh(m, WORK + 8);
                let x = widescreen::unsqueeze_x(m, x);
                m.gte.ctc2(ctrl::TRX, x);
                let y = lh(m, WORK + 10);
                m.gte.ctc2(ctrl::TRY, y);
                let lod = lhu(m, SCRATCH + 26);
                m.gte.ctc2(ctrl::TRZ, lod);
                // The glow's corners (-s, -s, 0), (0, -s, 0), (-s, 0, 0) as
                // SVECTORs at scratch +0x4C, +0x54, +0x5C (pads untouched).
                let negative = 0u32.wrapping_sub(size) as u16;
                for (offset, value) in [
                    (0x30u32, negative),
                    (0x32, negative),
                    (0x34, 0),
                    (0x38, 0),
                    (0x3a, negative),
                    (0x3c, 0),
                    (0x40, negative),
                    (0x42, 0),
                    (0x44, 0),
                ] {
                    m.sh(WORK + offset, value);
                }
                for (register, address) in [
                    (reg::VXY0, SCRATCH + 0x4c),
                    (reg::VZ0, SCRATCH + 0x50),
                    (reg::VXY1, SCRATCH + 0x54),
                    (reg::VZ1, SCRATCH + 0x58),
                    (reg::VXY2, SCRATCH + 0x5c),
                    (reg::VZ2, SCRATCH + 0x60),
                ] {
                    lwc2(m, register, address);
                }
                link_word = link_word.wrapping_add(10240);
                for face in 0..4u32 {
                    if face != 0 {
                        multiply_rotation(m, WORK + 104);
                    }
                    let centre = m.lw(WORK + 8);
                    m.sw(packet + 32, centre);
                    cop2(m, RTPT);
                    m.sw(packet, (link_word >> 8) | 0x0900_0000);
                    m.sh(packet + 28, 0x3fa0);
                    m.sh(packet + 36, 0x3fbf);
                    m.sw(packet + 12, 0x4f56_20a0);
                    m.sw(packet + 20, 0x0039_20bf);
                    let colour = m.lw(WORK + 16);
                    m.sw(packet + 4, colour);
                    swc2(m, reg::SXY0, packet + 8);
                    if face & 1 != 0 {
                        swc2(m, reg::SXY2, packet + 16);
                        swc2(m, reg::SXY1, packet + 24);
                    } else {
                        swc2(m, reg::SXY1, packet + 16);
                        swc2(m, reg::SXY2, packet + 24);
                    }
                    link_word = link_word.wrapping_add(10240);
                    packet += 40;
                }
                restore_view_matrix(m);
            }
            let first = m.lw(SCRATCH + 4);
            let slot = m.lw(SCRATCH) + 16376;
            insert_chain(m, slot, first, packet - 40);
            m.sw(SCRATCH + 4, packet);
        } else {
            if sun & 0xff == 0 {
                break 'body;
            }
            let brightness = div312(312u32.wrapping_sub(offset) << 12);
            m.sw(WORK + 12, brightness);
        }
        // The screen flash and the lens flare.
        let flash = sun & 0xff;
        if flash == 0 {
            break 'body;
        }
        if !slt(m.lw(WORK + 12), 2049) {
            if flash == 1 {
                let flags = m.lw(state);
                m.sw(state, flags | 1);
                let over = m.lw(WORK + 12).wrapping_sub(2048);
                let scaled = ((over << 2).wrapping_add(over)) << 4;
                let level = if (scaled as i32) < 0 {
                    sra(scaled.wrapping_add(2047), 11)
                } else {
                    sra(scaled, 11)
                };
                let grey = level | (level << 8) | (level << 16);
                let ot = m.lw(SCRATCH);
                let tag = m.lw(ot) | 0x0300_0000;
                let fill = m.lw(SCRATCH + 4);
                m.sw(fill, tag);
                let ot = m.lw(SCRATCH);
                m.sw(ot, link(fill));
                m.sh(fill + 8, (-160i16) as u16);
                m.sw(fill + 4, grey | 0x6200_0000);
                m.sh(fill + 10, (-120i16) as u16);
                m.sw(fill + 12, 0x00f0_0140);
                m.sw(SCRATCH + 4, fill + 16);
            } else {
                let flags = m.lw(state);
                m.sw(state, flags | 2);
                let over = m.lw(WORK + 12).wrapping_sub(2048);
                m.sw(state + 4, over << 1);
            }
            let brightness = m.lw(WORK + 12);
            if !slt(brightness, 3073) {
                let tripled = (brightness << 1).wrapping_add(brightness);
                m.sw(WORK + 12, 12288u32.wrapping_sub(tripled));
            }
        }
        let mut flare = sra(m.lw(WORK + 12).wrapping_sub(512), 5).wrapping_add(24);
        if !slt(flare, 241) {
            flare = 240;
        } else {
            flare = flare.wrapping_add(sdk::rand(m) & 7);
        }
        let sun_y = height.wrapping_add(0xffc8_0000);
        let eye_y = m.lw(SCRATCH + 12);
        if slt(sun_y, eye_y) {
            flare = flare.wrapping_sub(sra(eye_y.wrapping_sub(sun_y), 14));
        }
        if (flare as i32) <= 0 {
            break 'body;
        }
        m.sw(WORK + 16, flare);
        lens_flare(m, WORK, SCRATCH);
    }
    m.pop_frame(0x38);
}

/// 80067144 (JP; PAL 80065974, near, `imm:5`): list the visible entities (flags
/// 0x14000) into 16-byte records, skipping the camera's own entity; the JP
/// entity kinds 43, 35, 36 and 25 (PAL 61, 50, 51 and 19) select the marker
/// bytes. Returns the end of the list (not terminated).
pub fn list_entities(m: &mut Machine, list: u32) -> u32 {
    let mut out = list;
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    let own = m.lw(camera + 196);
    let target = m.lw(camera + 200);
    for index in 0..32u32 {
        let table = m.gw(ENTITIES);
        let entity = m.lw(table + index * 4);
        if m.lw(entity) & 0x14000 != 0x14000 || own == index {
            continue;
        }
        let kind = if m.lbu(entity + 5) & 1 == 0 {
            1
        } else if target != index {
            4
        } else {
            let gameplay = m.gw(GAMEPLAY);
            let camera = m.lw(gameplay + 40);
            if m.lw(camera + 80) & 6 != 0 { 2 } else { 4 }
        };
        m.sb(out + 4, kind);
        let model = m.lbu(entity + 4);
        m.sb(out, model);
        let side = m.lbu(entity + 5);
        m.sb(out + 1, side);
        let id = m.lhu(entity + 6);
        m.sw(out + 8, entity + 8);
        m.sh(out + 2, id);
        let flags = m.lw(entity);
        m.sb(out + 6, 0);
        m.sb(out + 7, 0);
        m.sw(out + 12, flags);
        let shape = m.lh(entity + 72);
        match shape {
            43 => m.sb(out + 6, 3),
            35 => m.sb(out + 6, 4),
            36 => m.sb(out + 6, 5),
            _ => {}
        }
        m.sb(out + 7, if shape == 25 { 2 } else { 1 });
        m.sb(out + 5, u8::from(target == index));
        out += 16;
    }
    let block = m.lw(MISSION_OBJECTS);
    let visible = m.lw(block + 5448);
    if visible != 0 {
        let hidden = !visible;
        let mut offset = 200u32;
        for index in 0..32u32 {
            if hidden & (1 << index) == 0 {
                let block = m.lw(MISSION_OBJECTS);
                m.sb(out + 4, 1);
                let object = block.wrapping_add(offset);
                let model = lbu(m, object + 52);
                if model != 0 {
                    m.sb(out, (model + 23) as u8);
                    m.sb(out + 1, 0);
                    m.sw(out + 8, object + 28);
                    m.sw(out + 12, 0);
                    m.sb(out + 5, 0);
                    m.sb(out + 6, 0);
                    m.sb(out + 7, 0);
                    if m.lbu(object + 53) & 0x10 != 0 {
                        m.sb(out + 6, 1);
                    }
                    if m.lbu(object + 53) & 0x20 != 0 {
                        m.sb(out + 6, 2);
                    }
                    out += 16;
                }
            }
            offset += 164;
        }
    }
    m.sb(out, 255);
    out
}

/// 80067380 (JP; PAL 80065BB0, near, `imm:3`): append the visible secondary
/// objects (`[gp+0x2D8]`, flags 0x14000), the JP kinds 43, 35, 36 (PAL 61, 50,
/// 51) marked, and terminate the list with 0xFF; returns the terminator.
pub fn list_objects(m: &mut Machine, list: u32) -> u32 {
    let mut out = list;
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    let own = m.lw(camera + 196).wrapping_sub(32);
    let target = m.lw(camera + 200);
    for index in 0..32u32 {
        let table = m.gw(OBJECTS);
        let object = m.lw(table + index * 4);
        if m.lw(object) & 0x14000 != 0x14000 || own == index {
            continue;
        }
        let kind = if m.lbu(object + 5) & 2 != 0 { 8 } else { 16 };
        m.sb(out + 4, kind);
        let model = m.lbu(object + 4);
        m.sb(out, model);
        let side = m.lbu(object + 5);
        m.sb(out + 1, side);
        let id = m.lhu(object + 6);
        m.sw(out + 8, object + 8);
        m.sh(out + 2, id);
        let flags = m.lw(object);
        m.sb(out + 6, 0);
        m.sb(out + 7, 0);
        m.sw(out + 12, flags);
        match m.lh(object + 72) {
            43 => m.sb(out + 6, 3),
            35 => m.sb(out + 6, 4),
            36 => m.sb(out + 6, 5),
            _ => {}
        }
        m.sb(out + 5, u8::from(target == index));
        out += 16;
    }
    m.sb(out, 255);
    out
}

/// 80090A74 (JP; PAL 8008F204, near, `imm:4`): depth-cued gouraud lines
/// (LINE_G2, code 0x52) for segments beyond a quarter of the projection
/// distance, culled when both ends are off the same screen edge (y bounds
/// 121/-120, PAL 129/-128); returns the new primitive cursor.
pub fn draw_effect_lines(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    let mut record = list;
    let mut timer = m.lw(record + 12);
    if (timer as i32) < 0 {
        return primitives;
    }
    let mut packet = primitives;
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0);
    }
    m.gte.mtc2(reg::RGBC, 0x5200_0000);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    let near = sra(lhu(m, camera + 36) << 16, 18);
    loop {
        if timer != 0 {
            let z0 = lh(m, record + 8);
            if slt(near, z0) {
                m.gte.mtc2(reg::IR0, fade(timer, 28, 7));
                m.gte.mtc2(reg::RGB0, 0x0006_433d);
                m.gte.mtc2(reg::RGB1, 0x0014_cfbf);
                let xy0 = m.lw(record);
                let xy1 = m.lw(record + 4);
                cop2(m, DPCT);
                let (y0, y1) = (screen_y(xy0), screen_y(xy1));
                let (x0, x1) = (screen_x(xy0), screen_x(xy1));
                let off = (!slt(y0, 121) && !slt(y1, 121))
                    || (slt(y0, (-120i32) as u32) && slt(y1, (-120i32) as u32))
                    || (!slt(x0, 161) && !slt(x1, 161))
                    || (slt(x0, (-160i32) as u32) && slt(x1, (-160i32) as u32));
                if off {
                    m.gte.mfc2(reg::MAC1);
                } else {
                    m.sw(packet + 8, xy0);
                    m.sw(packet + 16, xy1);
                    let z1 = lh(m, record + 10);
                    let slot = (sra(z0.wrapping_add(z1), 4) << 2).wrapping_add(ot);
                    add_prim(m, slot, packet, 0x0400_0000);
                    swc2(m, reg::RGB0, packet + 4);
                    swc2(m, reg::RGB1, packet + 12);
                    packet += 20;
                }
            }
        }
        record += 16;
        timer = m.lw(record + 12);
        if (timer as i32) < 0 {
            break;
        }
    }
    packet
}

/// 80090BF8 (JP; PAL 8008F388, near, `imm:4`): thin trails (half-width 2x the projection distance over depth).
pub fn draw_thin_trails(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    let style = TrailStyle {
        width: |raw| sra(raw, 15),
        fade_limit: 28,
        fade_shift: 7,
        word12: 0x4496_60b0,
        word20: 0x0038_60bf,
        half28: 0x9fb0,
        half36: 0x9fbf,
    };
    draw_trails(m, list, ot, primitives, &style)
}

/// 80090ED0 (JP; PAL 8008F660, near, `imm:4`): medium trails (3x).
pub fn draw_medium_trails(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    let style = TrailStyle {
        width: |raw| {
            let lod = sra(raw, 16);
            (lod << 1).wrapping_add(lod)
        },
        fade_limit: 28,
        fade_shift: 7,
        word12: 0x44d6_60c0,
        word20: 0x0038_60cf,
        half28: 0x9fc0,
        half36: 0x9fcf,
    };
    draw_trails(m, list, ot, primitives, &style)
}

/// 80091370 (JP; PAL 8008FB00, near, `imm:4`): wide trails (6x), fading over 14 frames.
pub fn draw_wide_trails(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    let style = TrailStyle {
        width: |raw| {
            let lod = sra(raw, 16);
            ((lod << 1).wrapping_add(lod)) << 1
        },
        fade_limit: 14,
        fade_shift: 8,
        word12: 0x4556_60d0,
        word20: 0x0038_60df,
        half28: 0x9fd0,
        half36: 0x9fdf,
    };
    draw_trails(m, list, ot, primitives, &style)
}

/// 800911B0 (JP; PAL 8008F940, near, `imm:1`): sparks: a depth-cued square
/// sprite (POLY_GT3 corner layout) at the far end of each segment within the
/// screen (|x| < 171, |y| < 131; PAL 139), sized by 20x the projection distance
/// over depth (at least 2).
pub fn draw_sparks(m: &mut Machine, list: u32, ot: u32, primitives: u32) -> u32 {
    let mut timer = m.lw(list + 12);
    if (timer as i32) < 0 {
        return primitives;
    }
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 40);
    let raw = lhu(m, camera + 36) << 16;
    let lod = sra(raw, 16);
    let five = (lod << 2).wrapping_add(lod);
    let near = sra(raw, 17);
    let size_scale = five << 2;
    m.gte.mtc2(reg::RGBC, 0x2e83_8383);
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0);
    }
    let mut record = list;
    let mut packet = primitives;
    loop {
        'segment: {
            if timer == 0 {
                break 'segment;
            }
            let z = lh(m, record + 10);
            let xy = m.lw(record + 4);
            let y = screen_y(xy);
            if !slt(near, z) {
                break 'segment;
            }
            let x = screen_x(xy);
            if !slt(abs(x), 171) || !slt(abs(y), 131) {
                break 'segment;
            }
            m.gte.mtc2(reg::IR0, fade(timer, 28, 7));
            let slot = (sra(z, 3) << 2).wrapping_add(ot);
            add_prim(m, slot, packet, 0x0900_0000);
            cop2(m, DPCS);
            let mut size = div_lo(size_scale, z);
            if slt(size, 2) {
                size = 2;
            }
            let half = sra(size, 1);
            // Widescreen: the width squeezed like the projection.
            let width = widescreen::squeeze_x(m, size);
            let x = x.wrapping_sub(sra(width, 1));
            let y = y.wrapping_sub(half);
            let right = x.wrapping_add(width) as u16;
            m.sh(packet + 32, right);
            m.sh(packet + 16, right);
            let bottom = y.wrapping_add(size) as u16;
            m.sh(packet + 34, bottom);
            m.sh(packet + 26, bottom);
            m.sh(packet + 28, 0x7fe0);
            m.sh(packet + 24, x as u16);
            m.sh(packet + 8, x as u16);
            m.sh(packet + 18, y as u16);
            m.sh(packet + 10, y as u16);
            m.sw(packet + 12, 0x4516_60e0);
            m.sw(packet + 20, 0x0038_60ff);
            m.sh(packet + 36, 0x7fff);
            swc2(m, reg::RGB2, packet + 4);
            packet += 40;
        }
        record += 16;
        timer = m.lw(record + 12);
        if (timer as i32) < 0 {
            break;
        }
    }
    packet
}

/// 8007CFA4 (JP; PAL 8007B928, near, `imm:8`): draw the 32 ribbon trails (two
/// edge positions per point, 32-byte records). The midpoint decides the detail
/// level and the grey level (halved with distance); consecutive visible pairs
/// become two gouraud lines (distant) or four POLY_G4 strips with feathered
/// edges (INTPL extrapolation by -192/192/3904/4288; PAL -96/96/4000/4192)
/// (near). Returns the new primitive cursor.
pub fn draw_ribbon_trails(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    let frame = m.push_frame(0x60);
    m.sw(frame + 96, ot);
    let mut packet = primitives;
    let block = m.gw(TRAILS);
    let active = m.lw(block.wrapping_add(0x1_2124));
    let mut trail = block.wrapping_add(0x1_2128);
    if active != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay + 40);
        let eye = [m.lw(camera + 4), m.lw(camera + 8), m.lw(camera + 12)];
        let half_w = sra(lhu(m, camera + 32) << 16, 17);
        let half_h = sra(lhu(m, camera + 34) << 16, 17);
        let lod = lh(m, camera + 36);
        let view = RibbonView {
            eye,
            half_w,
            half_h,
            range: (lod << 2).wrapping_add(lod << 3),
            near: lod << 3,
            ot,
        };
        for index in 0..32u32 {
            if active & (1 << index) != 0 {
                packet = draw_ribbon_trail(m, &view, trail, packet);
            }
            trail = trail.wrapping_add(1288);
        }
    }
    m.pop_frame(0x60);
    packet
}

/// 8008E038 (JP; PAL 8008C7C8, imm:1): ambient particles (renderer type 1): the 16 positions of the
/// object system block wrap around the camera in a 0x4000 cube; each is a
/// streak stretched by the camera velocity (8008DE18), fading in with
/// speed. The projection distance is narrowed with speed while drawing.
///
/// JP: the screen bound |y| < 201 (PAL 209).
pub fn draw_ambient_particles(m: &mut Machine) {
    m.push_frame(0x48);
    'body: {
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay + 40);
        let mut vx = sra(m.lw(camera + 156), 7);
        let mut vy = sra(m.lw(camera + 160), 7);
        let mut vz = sra(m.lw(camera + 164), 7);
        let speed = abs(vx).wrapping_add(abs(vy)).wrapping_add(abs(vz));
        m.sh(WORK, vx as u16);
        m.sh(WORK + 2, vy as u16);
        m.sh(WORK + 4, vz as u16);
        if speed.wrapping_sub(512) >= 11777 {
            break 'body;
        }
        let mut colour = 0x2e0f_0f0fu32;
        for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
            m.gte.ctc2(register, 0);
        }
        m.gte.mtc2(reg::RGBC, colour);
        let fade = 2048u32.wrapping_sub(speed) << 2;
        if (fade as i32) > 0 {
            m.gte.mtc2(reg::IR0, fade);
            cop2(m, DPCS);
            colour = m.gte.mfc2(reg::RGB2);
            m.gte.mtc2(reg::RGBC, colour);
        }
        load_rotation(m, gameplay + 44);
        m.gte.mtc2(reg::VXY0, 0);
        m.gte.mtc2(reg::VZ0, 4096);
        let mut packet = m.lw(SCRATCH + 4);
        let mut position = m.gw(OBJECT_SYSTEMS);
        // MVMVA sf=1, rotation x V0, no translation: the view axis.
        cop2(m, 0x048_6012);
        vx = 0u32.wrapping_sub(sra(m.lw(SCRATCH + 8), 9));
        vy = 0u32.wrapping_sub(sra(m.lw(SCRATCH + 12), 9));
        vz = 0u32.wrapping_sub(sra(m.lw(SCRATCH + 16), 9));
        let lod = lh(m, SCRATCH + 26);
        let width = ((((lod << 3).wrapping_add(lod)) << 2).wrapping_sub(lod)) << 2;
        vx = vx.wrapping_sub(m.gte.mfc2(reg::MAC1));
        vy = vy.wrapping_sub(m.gte.mfc2(reg::MAC2));
        vz = vz.wrapping_sub(m.gte.mfc2(reg::MAC3));
        load_rotation(m, gameplay + 76);
        m.gte.ctc2(ctrl::TRZ, 4096);
        let narrow = sra(speed.wrapping_sub(2048), 8);
        let narrow = if (narrow as i32) < 0 {
            0
        } else if slt(narrow, 11) {
            narrow
        } else {
            10
        };
        m.gte.ctc2(ctrl::H, 160u32.wrapping_sub(narrow));
        for index in 0..16u32 {
            let wrap =
                |base: u32, offset: u32| (base.wrapping_add(offset) & 0x3fff).wrapping_sub(8192);
            let x = wrap(lhu(m, position), vx);
            m.sh(WORK + 8, x as u16);
            let y = wrap(lhu(m, position + 2), vy);
            m.sh(WORK + 10, y as u16);
            let z = wrap(lhu(m, position + 4), vz);
            m.sh(WORK + 12, z as u16);
            lwc2(m, reg::VXY0, WORK + 8);
            lwc2(m, reg::VZ0, WORK + 12);
            let sub = |m: &mut Machine, a: u32, b: u32| lhu(m, a).wrapping_sub(lhu(m, b));
            let add = |m: &mut Machine, a: u32, b: u32| lhu(m, a).wrapping_add(lhu(m, b));
            let v = sub(m, WORK + 8, WORK);
            m.sh(WORK + 16, v as u16);
            let v = sub(m, WORK + 10, WORK + 2);
            m.sh(WORK + 18, v as u16);
            let v = sub(m, WORK + 12, WORK + 4);
            m.sh(WORK + 20, v as u16);
            let v = add(m, WORK + 8, WORK);
            m.sh(WORK + 24, v as u16);
            let v = add(m, WORK + 10, WORK + 2);
            let w = add(m, WORK + 12, WORK + 4);
            m.sh(WORK + 26, v as u16);
            m.sh(WORK + 28, w as u16);
            lwc2(m, reg::VXY1, WORK + 16);
            lwc2(m, reg::VZ1, WORK + 20);
            lwc2(m, reg::VXY2, WORK + 24);
            lwc2(m, reg::VZ2, WORK + 28);
            m.sw(packet, link(packet + 40) | 0x0900_0000);
            cop2(m, RTPT);
            let u = (index << 1) & 0xe0;
            let u_end = u + 63;
            m.sw(packet + 20, u_end | 0x0037_c000);
            m.sw(packet + 12, u | 0x40d6_c000);
            m.sh(packet + 28, (u | 0xff00) as u16);
            m.sh(packet + 36, (u_end | 0xff00) as u16);
            if (m.gte.cfc2(ctrl::FLAG) as i32) >= 0 {
                swc2(m, reg::SXY1, WORK + 40);
                if slt(abs(lh(m, WORK + 40)), 241) && slt(abs(lh(m, WORK + 42)), 201) {
                    m.sw(packet + 4, colour);
                    swc2(m, reg::SXY2, WORK + 44);
                    let z1 = m.gte.mfc2(reg::SZ2);
                    let w1 = if z1 != 0 { div_lo(width, z1) } else { 0 };
                    m.sw(WORK + 48, w1);
                    let z2 = m.gte.mfc2(reg::SZ3);
                    let w2 = if z2 != 0 { div_lo(width, z2) } else { 0 };
                    m.sw(WORK + 52, w2);
                    build_trail_quad(m, packet, WORK + 32);
                    packet += 40;
                }
            }
            position += 8;
        }
        let first = m.lw(SCRATCH + 4);
        if first != packet {
            let slot = m.lw(SCRATCH) + 112;
            insert_chain(m, slot, first, packet - 40);
            m.sw(SCRATCH + 4, packet);
        }
        let h = lhu(m, SCRATCH + 26);
        m.gte.ctc2(ctrl::H, h);
        m.gte.ctc2(ctrl::TRZ, 0);
    }
    m.pop_frame(0x48);
}

/// 800854B8 (JP; PAL 80083D48, imm:1): the three object lists (bit masks of 28-byte objects, 32 per
/// 896-byte list) are projected; visible ones (within 0x8000 grid units in
/// each axis, nearer than the near plane in projected size) get a screen
/// record at work +100 (SXY, OT depth, size) and a bit in work +40; then
/// each visible object is drawn by its class renderer (table 800B439C)
/// with the view rotation saved at work +56.
///
/// JP: the screen bound `size + 120` (PAL 128).
pub fn draw_objects(m: &mut Machine, lists: u32) {
    let frame = m.push_frame(0x40);
    let mut count = 0u32;
    let mut mask_word = lists;
    let mut base = 12u32;
    let mut visible_slot = WORK;
    for _ in 0..3 {
        let mut mask = m.lw(mask_word);
        m.sw(visible_slot + 40, 0);
        if mask != 0 {
            let mut object = lists + base;
            let mut bit = 0u32;
            let mut record = WORK + (count << 3);
            loop {
                'object: {
                    if mask & 1 == 0 || (lh(m, object) as i32) < 0 {
                        break 'object;
                    }
                    let in_range = |v: u32| v.wrapping_add(0x8000) <= 0xffff;
                    let x = sra(m.lw(object + 4).wrapping_sub(m.lw(SCRATCH + 8)), 12);
                    if !in_range(x) {
                        break 'object;
                    }
                    let y = sra(m.lw(object + 8).wrapping_sub(m.lw(SCRATCH + 12)), 12);
                    if !in_range(y) {
                        break 'object;
                    }
                    let z = sra(m.lw(object + 12).wrapping_sub(m.lw(SCRATCH + 16)), 12);
                    if !in_range(z) {
                        break 'object;
                    }
                    let raw_size = m.lhu(object + 26);
                    let size = u32::from(raw_size) << 16;
                    let size = sra(size, 16);
                    m.sh(record + 106, raw_size);
                    let (xy, vz, shift) = if slt(size, 256)
                        && slt(abs(x).wrapping_add(abs(y)).wrapping_add(abs(z)), 1024)
                    {
                        (((x << 17) >> 16) | (y << 17), z << 1, 1)
                    } else {
                        ((x & 0xffff) | (y << 16), z, 0)
                    };
                    m.gte.mtc2(reg::VXY0, xy);
                    m.gte.mtc2(reg::VZ0, vz);
                    let lod = lh(m, SCRATCH + 26);
                    cop2(m, RTPS);
                    let scaled = mul_lo(size, lod);
                    let depth = m.gte.mfc2(reg::MAC3);
                    if slt(depth, lod) {
                        break 'object;
                    }
                    let depth = sra(depth, shift);
                    let projected = div_lo(scaled, depth);
                    if projected == 0 {
                        break 'object;
                    }
                    let sxy = m.gte.mfc2(reg::SXY2);
                    m.sh(record + 106, projected as u16);
                    if slt(projected.wrapping_add(160), abs(screen_x(sxy))) {
                        break 'object;
                    }
                    if slt(projected.wrapping_add(120), abs(screen_y(sxy))) {
                        break 'object;
                    }
                    let mut otz = sra(depth, 3).wrapping_sub(1);
                    m.sw(record + 100, sxy);
                    if !slt(otz, 4095) {
                        otz = 4094;
                    } else if (otz as i32) < 0 {
                        otz = 0;
                    }
                    m.sh(record + 104, otz as u16);
                    record += 8;
                    count += 1;
                    let visible = m.lw(visible_slot + 40);
                    m.sw(visible_slot + 40, visible | (1 << (bit & 31)));
                }
                bit += 1;
                mask >>= 1;
                object += 28;
                if mask == 0 {
                    break;
                }
            }
        }
        visible_slot += 4;
        base += 896;
        mask_word += 4;
    }
    if count != 0 {
        save_rotation(m, WORK + 56);
        m.gte.mtc2(reg::VZ0, 0);
        let ot = m.lw(SCRATCH);
        let cursor = m.lw(SCRATCH + 4);
        m.sw(WORK + 88, u32::MAX);
        m.sw(WORK, ot);
        m.sw(WORK + 4, cursor);
        let gameplay = m.gw(GAMEPLAY);
        let colours = m.lw(gameplay + 616);
        let fog = m.lw(colours + 16);
        m.sw(WORK + 96, link(fog) | 0x2e00_0000);
        let mut drawn = 0u32;
        let mut base = 12u32;
        let mut visible_slot = WORK;
        for _ in 0..2 {
            let mut mask = m.lw(visible_slot + 40);
            if mask != 0 {
                let mut object = lists + base;
                let mut record = (drawn << 3) + 100;
                loop {
                    if mask & 1 != 0 {
                        let pitch = lh(m, SCRATCH + 24);
                        m.sw(WORK + 16, 0u32.wrapping_sub(pitch));
                        let raw = lhu(m, object + 24) << 16;
                        let kind_word = sra(raw, 16);
                        let mut class = (kind_word >> 4) & 0xf;
                        let screen = WORK + record;
                        if !slt(class, 4) {
                            class = 3;
                        }
                        record += 8;
                        drawn += 1;
                        m.sw(WORK + 8, sra(raw, 24) & 3);
                        m.sw(WORK + 12, kind_word & 0xf);
                        let renderer = m.lw(OBJECT_RENDERERS + (class << 2));
                        // The original spills the list base to its stack
                        // word +16 around the call.
                        m.sw(frame + 16, lists);
                        dispatch::call(m, renderer, [WORK, screen, object, 0]);
                    }
                    mask >>= 1;
                    object += 28;
                    if mask == 0 {
                        break;
                    }
                }
            }
            base += 896;
            visible_slot += 4;
        }
        let cursor = m.lw(WORK + 4);
        m.sw(SCRATCH + 4, cursor);
        load_rotation(m, WORK + 56);
    }
    m.pop_frame(0x40);
}

/// The prologue of 80086320 and 80086A0C: in their 0x30-byte frame on the
/// scratchpad stack (their only caller 8008700C moves `sp` to 0x1F8003F8,
/// so the frame is 0x1F8003C8 whatever stack the host runs them on) they
/// save s0..s5 at +0x10..+0x24 and `ra` at +0x28, the registers of 8008700C:
/// s0 the projectile, s1 its group's mask word shifted right once per
/// projectile before this one, s2 WORK, s3 the mask word's address (the
/// lists + 4 * group), s4 the group's first projectile offset (0x28 +
/// 0x380 * group), s5 the group, and `ra` the return address of the call
/// (`ra`). The lists are the object systems +0x1098 (8008542C), the
/// projectiles 28 bytes each.
fn spill_projectile_caller(m: &mut Machine, projectile: u32, ra: u32) {
    let lists = m.gw(OBJECT_SYSTEMS).wrapping_add(0x1098);
    let offset = projectile.wrapping_sub(lists).wrapping_sub(0x28);
    let group = offset / 0x380;
    let index = offset % 0x380 / 0x1c;
    let mask_word = lists.wrapping_add(group << 2);
    let mask = m.lw(mask_word) >> (index & 31);
    let frame = SCRATCH + 0x3f8 - 0x30;
    let base = group.wrapping_mul(0x380).wrapping_add(0x28);
    for (slot, value) in [projectile, mask, WORK, mask_word, base, group]
        .into_iter()
        .enumerate()
    {
        m.sw(frame + 0x10 + 4 * slot as u32, value);
    }
    m.sw(frame + 0x28, ra);
}

/// 80086320 (JP; PAL 80084BB0, imm:1): projectile type 0 (tracer): projected; tiny ones become a
/// TILE_1 dot (bright or dim by the noise bits), larger ones a gouraud
/// triangle (POLY_F3, or POLY_FT3 above size 6) of three points spun by
/// the projectile's noise word, coloured by facing.
///
/// JP: the screen bound `size + 120` (PAL 128); it saves its caller's
/// registers in its frame itself ([`spill_projectile_caller`]).
pub fn draw_tracer(m: &mut Machine, projectile: u32, work: u32, saved: u32) {
    spill_projectile_caller(m, projectile, 0x8008_70c8);
    let Some((x, y, z)) = projectile_offset(m, projectile, work) else {
        return;
    };
    load_rotation(m, saved);
    for register in [ctrl::TRX, ctrl::TRY, ctrl::TRZ] {
        m.gte.ctc2(register, 0);
    }
    m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
    m.gte.mtc2(reg::VZ0, z);
    let systems = m.gw(OBJECT_SYSTEMS);
    let noise = m.lw((projectile & 0x1fc).wrapping_add(systems));
    cop2(m, RTPS);
    m.sw(saved + 64, noise);
    if m.lhu(projectile + 22) & 0x100 != 0 {
        let spin = lhu(m, projectile) << 8;
        m.sh(saved + 68, spin as u16);
    } else {
        m.sh(saved + 68, 0);
    }
    let radius = lh(m, projectile + 26);
    m.sw(saved + 112, radius);
    let raw_lod = lhu(m, work + 26) << 16;
    let scaled = mul_lo(radius, sra(raw_lod, 16));
    let depth = m.gte.mfc2(reg::SZ3);
    if (depth as i32) <= 0 {
        return;
    }
    let size = div_lo(scaled, depth);
    let near = depth.wrapping_sub(size);
    if slt(near, sra(raw_lod, 20)) {
        return;
    }
    let sxy = m.gte.mfc2(reg::SXY2);
    if slt(size.wrapping_add(120), abs(screen_y(sxy))) {
        return;
    }
    if slt(size.wrapping_add(160), abs(screen_x(sxy))) {
        return;
    }
    if slt(size, 3) {
        let otz = clamp_otz(sra(depth, 3));
        let packet = m.lw(work + 4);
        let ot = m.lw(work);
        let tag = m.lw((otz << 2).wrapping_add(ot)) | 0x0200_0000;
        m.sw(packet, tag);
        let ot = m.lw(work);
        m.sw((otz << 2).wrapping_add(ot), link(packet));
        let noise = m.lw(saved + 64);
        let phase = lh(m, saved + 68)
            .wrapping_add(sra(noise, 16))
            .wrapping_add(noise);
        let colour = if phase & 0x800 != 0 {
            0x6863_6363
        } else {
            0x6818_1818
        };
        m.sw(packet + 4, colour);
        m.sw(packet + 8, sxy);
        m.sw(work + 4, packet + 12);
        return;
    }
    m.sw(saved + 116, size);
    m.sw(saved + 104, depth);
    if slt(near, lh(m, work + 26)) {
        m.sw(saved + 108, 4);
        let radius = m.lw(saved + 112);
        m.sw(saved + 112, radius << 4);
    } else {
        m.sw(saved + 108, 0);
    }
    let mut tx = m.gte.mfc2(reg::MAC1);
    let mut ty = m.gte.mfc2(reg::MAC2);
    let mut tz = m.gte.mfc2(reg::MAC3);
    let shift = m.lw(saved + 108);
    if shift != 0 {
        tx = sllv(tx, shift);
        ty = sllv(ty, shift);
        tz = sllv(tz, shift);
    }
    let scale = m.lw(saved + 112);
    angle_matrix(m, saved + 64, scale, saved + 32);
    multiply_rotation(m, saved + 32);
    m.gte.ctc2(ctrl::TRX, tx);
    m.gte.ctc2(ctrl::TRY, ty);
    m.gte.ctc2(ctrl::TRZ, tz);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, (-2048i32) as u32);
    m.gte.mtc2(reg::VXY1, 0x0400_0000);
    m.gte.mtc2(reg::VZ1, 512);
    m.gte.mtc2(reg::VXY2, 0xfd00_0000);
    let tail = (lhu(m, saved + 64) & 7) << 9;
    m.gte.mtc2(reg::VZ2, tail);
    let size = m.lw(saved + 116);
    let depth = m.lw(saved + 104);
    cop2(m, RTPT);
    let otz = clamp_otz(sra(depth, 3));
    let (code, words) = if !slt(size, 7) {
        let ot = m.lw(work);
        let variant = lh(m, saved + 64);
        let packet = m.lw(work + 4);
        let tag = m.lw((otz << 2).wrapping_add(ot)) | 0x0700_0000;
        m.sw(packet, tag);
        let ot = m.lw(work);
        m.sw((otz << 2).wrapping_add(ot), link(packet));
        let clut = if variant & 1 != 0 {
            0x45d6_10e0
        } else {
            0x45d6_1fef
        };
        m.sw(packet + 12, clut);
        m.sw(packet + 20, 0x0038_10ef);
        m.sh(packet + 28, 0x1fe0);
        swc2(m, reg::SXY0, packet + 8);
        swc2(m, reg::SXY1, packet + 16);
        swc2(m, reg::SXY2, packet + 24);
        (0x2400_0000u32, 8u32)
    } else {
        let ot = m.lw(work);
        let packet = m.lw(work + 4);
        let tag = m.lw((otz << 2).wrapping_add(ot)) | 0x0400_0000;
        m.sw(packet, tag);
        let ot = m.lw(work);
        let cursor = m.lw(work + 4);
        m.sw((otz << 2).wrapping_add(ot), link(cursor));
        swc2(m, reg::SXY0, packet + 8);
        swc2(m, reg::SXY1, packet + 12);
        swc2(m, reg::SXY2, packet + 16);
        (0x2000_0000, 5)
    };
    cop2(m, NCLIP);
    let facing = m.gte.mfc2(reg::MAC0);
    let colour = if (facing as i32) >= 0 {
        0x0018_1818
    } else {
        0x0063_6363
    };
    let packet = m.lw(work + 4);
    m.sw(packet + 4, code | colour);
    let packet = m.lw(work + 4);
    m.sw(work + 4, packet.wrapping_add(words << 2));
}

/// 80086A0C (JP; PAL 8008529C, imm:1): projectile types 1.. (missiles): an exhaust flame of up to 15
/// POLY_G4 segments built from packed random vertices (object system block
/// +1056 and noise table), depth-cued by distance from the type colour
/// table 800B43AC, scaled by the projectile's size; chained into the OT at
/// its depth.
///
/// JP: the screen bound `size + 120` (PAL 128); it saves its caller's
/// registers in its frame itself ([`spill_projectile_caller`]).
pub fn draw_exhaust(m: &mut Machine, projectile: u32, work: u32, saved: u32) {
    spill_projectile_caller(m, projectile, 0x8008_70dc);
    let Some((x, y, z)) = projectile_offset(m, projectile, work) else {
        return;
    };
    load_rotation(m, saved);
    for register in [ctrl::TRX, ctrl::TRY, ctrl::TRZ] {
        m.gte.ctc2(register, 0);
    }
    m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
    m.gte.mtc2(reg::VZ0, z);
    let systems = m.gw(OBJECT_SYSTEMS);
    let mut shapes = systems + 1056;
    cop2(m, RTPS);
    let noise = m.lw((projectile & 0x1fc).wrapping_add(systems));
    m.sw(saved + 64, noise);
    let age = lh(m, projectile);
    if m.lhu(projectile + 22) & 0x100 != 0 {
        m.sh(saved + 68, (age << 6) as u16);
    } else {
        m.sh(saved + 68, 0);
    }
    shapes += sra(noise, 4) & 0x1c;
    let mut jitter = systems + ((noise & 0x3f) << 2);
    let mut life = lh(m, projectile + 2);
    let radius = lh(m, projectile + 26);
    if life == 0 {
        life = 1;
    }
    let progress = div_lo(age << 12, life);
    m.sw(saved + 144, progress);
    if !slt(progress, 2731) {
        jitter += (age << 1) & 0xc;
    }
    let sine = lh(m, (sra(progress, 2) << 2) + SINES);
    let product = mul_lo(radius, sine);
    let radius = if (product as i32) < 0 {
        sra(product.wrapping_add(4095), 12)
    } else {
        sra(product, 12)
    };
    let depth = m.gte.mfc2(reg::SZ3);
    m.sw(saved + 112, radius);
    if (depth as i32) <= 0 {
        return;
    }
    let raw_lod = lhu(m, work + 26) << 16;
    let size = div_lo(mul_lo(radius, sra(raw_lod, 16)), depth);
    let near = depth.wrapping_sub(size);
    if slt(near, sra(raw_lod, 20)) {
        return;
    }
    let sxy = m.gte.mfc2(reg::SXY2);
    if slt(size.wrapping_add(120), abs(screen_y(sxy))) {
        return;
    }
    if slt(size.wrapping_add(160), abs(screen_x(sxy))) {
        return;
    }
    let fog = sra(depth.wrapping_sub(1024), 2);
    if !slt(fog, 4096) {
        return;
    }
    let fog = if (fog as i32) < 0 { 0 } else { fog };
    m.gte.mtc2(reg::IR0, fog);
    store_mac123(m, saved + 88);
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 48);
    }
    m.gte.mtc2(reg::RGBC, 0x6a00_0000);
    let kind_word = lhu(m, projectile + 24);
    let mut kind = kind_word & 0xff;
    if !slt(kind, 7) {
        kind = 6;
    }
    let colours = (((kind << 1) + kind) << 2) + 0x800b_43ac;
    lwc2(m, reg::RGB0, colours);
    lwc2(m, reg::RGB1, colours + 4);
    lwc2(m, reg::RGB2, colours + 8);
    let mut segments = sra(kind_word << 16, 24) & 0x1f;
    cop2(m, DPCT);
    if slt(near, lh(m, work + 26)) {
        let radius = m.lw(saved + 112);
        m.sw(saved + 108, 4);
        m.sw(saved + 112, radius << 4);
    } else {
        m.sw(saved + 108, 0);
    }
    if segments & 0x10 != 0 {
        let mut limit = sra(size.wrapping_sub(4), 1);
        segments &= 0xf;
        if (limit as i32) <= 0 {
            limit = 1;
        }
        if slt(limit, segments) {
            segments = limit;
        }
    }
    let otz = clamp_otz(sra(depth, 3));
    m.sw(saved + 104, otz);
    swc2(m, reg::RGB0, saved + 148);
    swc2(m, reg::RGB1, saved + 152);
    swc2(m, reg::RGB2, saved + 156);
    let scale = m.lw(saved + 112) << 2;
    angle_matrix(m, saved + 64, scale, saved + 32);
    multiply_rotation(m, saved + 32);
    let mut tx = m.lw(saved + 88);
    let mut ty = m.lw(saved + 92);
    let shift = m.lw(saved + 108);
    let mut tz = m.lw(saved + 96);
    if shift != 0 {
        tx = sllv(tx, shift);
        ty = sllv(ty, shift);
        tz = sllv(tz, shift);
    }
    m.gte.ctc2(ctrl::TRX, tx);
    m.gte.ctc2(ctrl::TRY, ty);
    m.gte.ctc2(ctrl::TRZ, tz);
    jitter += 4;
    let base = m.lw(saved + 144).wrapping_sub(512);
    let mut packet = m.lw(work + 4);
    let mut fade = base.wrapping_add(tx & 0x3ff);
    let vertices = saved + 120;
    while (segments as i32) > 0 {
        m.gte.mtc2(reg::IR0, fade);
        lwc2(m, reg::RGB0, saved + 148);
        lwc2(m, reg::RGB1, saved + 152);
        lwc2(m, reg::RGB2, saved + 156);
        let word = m.lw(shapes);
        shapes += 4;
        cop2(m, DPCT);
        store_exhaust_vertex(
            m,
            vertices,
            word,
            [(2, field_low), (4, field_high), (0, field_mid)],
        );
        let word = m.lw(shapes);
        shapes += 4;
        store_exhaust_vertex(
            m,
            vertices + 8,
            word,
            [(0, field_low), (2, field_high), (4, field_mid)],
        );
        let word = m.lw(shapes);
        shapes += 4;
        store_exhaust_vertex(
            m,
            vertices + 16,
            word,
            [(0, field_high), (2, field_mid), (4, field_low)],
        );
        for (register, offset) in [
            (reg::RGB0, 4u32),
            (reg::RGB0, 12),
            (reg::RGB1, 20),
            (reg::RGB1, 28),
            (reg::RGB2, 36),
            (reg::RGB2, 44),
        ] {
            swc2(m, register, packet + offset);
        }
        load_exhaust_vertices(m, vertices);
        let word = m.lw(shapes);
        shapes += 4;
        cop2(m, RTPT);
        store_exhaust_vertex(
            m,
            vertices,
            word,
            [(2, field_mid), (4, field_low), (0, field_high)],
        );
        let word = m.lw(shapes);
        shapes += 4;
        store_exhaust_vertex(
            m,
            vertices + 8,
            word,
            [(0, field_mid), (2, field_low), (4, field_high)],
        );
        let word = m.lw(shapes);
        shapes += 4;
        store_exhaust_vertex(
            m,
            vertices + 16,
            word,
            [(0, field_low), (2, field_high), (4, field_mid)],
        );
        swc2(m, reg::SXY0, packet + 8);
        swc2(m, reg::SXY1, packet + 16);
        swc2(m, reg::SXY2, packet + 24);
        load_exhaust_vertices(m, vertices);
        let next = packet + 52;
        m.sw(packet, link(next) | 0x0c00_0000);
        cop2(m, RTPT);
        let step = m.lw(jitter);
        jitter += 4;
        fade = base.wrapping_add(step & 0x3ff);
        swc2(m, reg::SXY0, packet + 32);
        swc2(m, reg::SXY1, packet + 40);
        swc2(m, reg::SXY2, packet + 48);
        segments = segments.wrapping_sub(1);
        packet = next;
    }
    let ot = m.lw(work);
    let slot = (m.lw(saved + 104) << 2).wrapping_add(ot);
    let first = m.lw(work + 4);
    insert_chain(m, slot, first, packet.wrapping_sub(52));
    m.sw(work + 4, packet);
}

/// 8008B310 (JP; PAL 80089BA0, imm:5 ins:3 op:2 reg:1): the moon (or second sun): projected from state +8 (pulled in
/// when far below), a 16-segment disc of POLY_G4 quads (colour `colour`
/// depth-cued by height) rotated by 80030778 and scaled (ScaleMatrix),
/// wobbling with the frame counter; chained with draw-mode packets at OT
/// slot 30. `state` is object systems +1480.
///
/// JP: the screen bound `size + 120` (PAL 128) and the view rotation and translation reloaded from gameplay +0x4C at the end (PAL cleared the translation).
pub fn draw_moon(m: &mut Machine, state: u32, colour: u32) {
    m.push_frame(0x28);
    'body: {
        let eye_y = m.lw(SCRATCH + 12);
        if slt(eye_y, 0xf880_0000) {
            break 'body;
        }
        let up = 0u32.wrapping_sub(sra(eye_y, 12));
        let xy = m.lw(state + 8);
        let z = m.lw(state + 12);
        m.sw(WORK, xy);
        m.sw(WORK + 4, z);
        let mut height = up.wrapping_sub(lh(m, WORK + 2));
        if slt(0x7fff, height) {
            let x = lhu(m, WORK);
            m.sh(WORK + 2, sra(height, 1) as u16);
            let z = lhu(m, WORK + 4);
            m.sh(WORK, sra(x << 16, 17) as u16);
            m.sh(WORK + 4, sra(z << 16, 17) as u16);
        } else {
            m.sh(WORK + 2, height as u16);
        }
        lwc2(m, reg::VXY0, WORK);
        lwc2(m, reg::VZ0, WORK + 4);
        cop2(m, RTPS);
        let lod = lh(m, SCRATCH + 26);
        let five = (lod << 2).wrapping_add(lod);
        let scaled = ((five << 4).wrapping_sub(five)) << 4;
        let radius = if (scaled as i32) < 0 {
            sra(scaled.wrapping_add(4095), 12)
        } else {
            sra(scaled, 12)
        };
        m.sw(WORK + 16, radius);
        if !slt(height, 8193) {
            m.sw(WORK + 12, radius);
            if !slt(height, 16385) {
                height = 16384;
            }
        } else {
            let third = mul_hi(radius, 0x5555_5556).wrapping_sub(sra(scaled, 31));
            let mut v = mul_lo(radius.wrapping_sub(third), height);
            if (v as i32) < 0 {
                v = v.wrapping_add(8191);
            }
            m.sw(WORK + 12, sra(v, 13).wrapping_add(third));
        }
        if (m.gte.cfc2(ctrl::FLAG) as i32) < 0 {
            break 'body;
        }
        swc2(m, reg::SXY2, WORK + 8);
        let size = m.lw(WORK + 16);
        if slt(size.wrapping_add(160), abs(lh(m, WORK + 8))) {
            break 'body;
        }
        if slt(size.wrapping_add(120), abs(lh(m, WORK + 10))) {
            break 'body;
        }
        for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
            m.gte.ctc2(register, 0);
        }
        m.gte.mtc2(reg::RGBC, colour);
        m.gte.mtc2(reg::IR0, sra(height, 3));
        let mode = m.lw(SCRATCH + 4);
        let mut packet = mode + 8;
        cop2(m, DPCS);
        m.sw(mode, link(packet) | 0x0100_0000);
        m.sw(mode + 4, 0xe600_0002);
        let lit = m.gte.mfc2(reg::RGB2);
        let pitch = lhu(m, SCRATCH + 24);
        m.sw(WORK, 0);
        m.sh(WORK + 4, 0u32.wrapping_sub(pitch) as u16);
        shared::f_80030534(m, [WORK, WORK + 28, 0, 0]);
        m.sw(WORK + 20, 0);
        scale_matrix(m, WORK + 28, WORK + 12);
        load_rotation(m, WORK + 28);
        let x = lh(m, WORK + 8);
        let moon_x = x;
        let y = lh(m, WORK + 10);
        m.sw(WORK + 56, 0);
        m.sw(WORK + 48, x);
        m.sw(WORK + 52, y);
        load_translation(m, WORK + 48);
        let table = 0x800b_7104u32;
        let context = m.gw(crate::common::CONTEXT);
        let frame = m.lw(context + 36) << 6;
        let mut angle = frame & 0xfff;
        let wobble = lhu(m, (angle << 2) + table + 2);
        angle = (angle << 1) & 0xfff;
        let x = lhu(m, WORK + 8).wrapping_add(sra(wobble << 16, 25));
        m.sh(WORK + 8, x as u16);
        let start = lhu(m, table + (angle << 2));
        m.sh(WORK + 2, 4096);
        m.sh(WORK + 4, 0);
        m.sh(WORK, sra(start << 16, 20) as u16);
        for segment in 0..16u32 {
            lwc2(m, reg::VXY0, WORK);
            lwc2(m, reg::VZ0, WORK + 4);
            // MVMVA sf=1, rotation x V0 + translation.
            cop2(m, 0x048_0012);
            angle = (angle + 1365) & 0xfff;
            let point = m.lw(((segment + 1) << 10) + table);
            let jitter = lhu(m, (angle << 2) + table) << 16;
            m.sw(WORK, point);
            let x = lhu(m, WORK).wrapping_add(sra(jitter, 19));
            m.sh(WORK, x as u16);
            let out = WORK + 60 + segment * 4;
            let ir1 = m.gte.mfc2(reg::IR1);
            // Widescreen: the disc offsets squeezed like the projection.
            let ir1 = widescreen::squeeze_about(m, moon_x, ir1.wrapping_sub(moon_x));
            let ir2 = m.gte.mfc2(reg::IR2);
            let ir3 = m.gte.mfc2(reg::IR3);
            m.sh(out, ir1 as u16);
            m.sh(out + 2, ir2 as u16);
            m.sh(out + 4, ir3 as u16);
        }
        let first = m.lw(WORK + 60);
        m.sw(WORK + 124, first);
        for segment in (0..16u32).step_by(2) {
            let next = packet + 36;
            m.sw(packet, link(next) | 0x0800_0000);
            m.sw(packet + 4, 0x3a03_0303);
            m.sw(packet + 12, lit);
            m.sw(packet + 20, 0x0003_0303);
            m.sw(packet + 28, 0x0003_0303);
            let a = m.lw(WORK + 60 + segment * 4);
            m.sw(packet + 8, a);
            let centre = m.lw(WORK + 8);
            m.sw(packet + 16, centre);
            let b = m.lw(WORK + 60 + (segment + 1) * 4);
            m.sw(packet + 24, b);
            let c = m.lw(WORK + 60 + (segment + 2) * 4);
            m.sw(packet + 32, c);
            packet = next;
        }
        m.sw(packet, 0x0100_0000);
        m.sw(packet + 4, 0xe600_0000);
        let slot = m.lw(SCRATCH) + 120;
        let first = m.lw(SCRATCH + 4);
        insert_chain(m, slot, first, packet);
        m.sw(SCRATCH + 4, packet + 8);
        restore_view_matrix(m);
    }
    m.pop_frame(0x28);
}

/// 8007B438 (JP; PAL 80079DA4, del:6 imm:18): per-frame camera update. Runs the handler of the camera mode
/// (camera +80; only mode 1, the cockpit/chase view 80077A30, is
/// translated so far), follows the attached object (camera flag 0x100:
/// position and angles of the object at +52 plus the offsets at +56/+72),
/// applies the ground clamp (flag 0x800), and records the frame's velocity
/// and angular velocity at +156/+172 (zeroed once after a cut, flag 0x400).
///
/// JP: the view byte context +0x39F (PAL +0x3A3); in the replay mode the manual look runs always (PAL not in replay kind 3) and context byte +0x3A5 (PAL +0x3A9) keeps the ground clamp off.
pub fn update_camera(m: &mut Machine, camera: u32) {
    m.push_frame(0x38);
    let old = [m.lw(camera + 4), m.lw(camera + 8), m.lw(camera + 12)];
    let old_angles = [m.lhu(camera + 20), m.lhu(camera + 22), m.lhu(camera + 24)];
    let still = [180u32, 182, 184]
        .into_iter()
        .all(|offset| slt(abs(lh(m, camera + offset)), 49));
    let flags = m.lw(camera);
    m.sw(camera, if still { flags & !0x200 } else { flags | 0x200 });
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    if flags & 0x40 != 0 {
        m.sw(gameplay, flags & !0x40);
        let context = game_context(m);
        let a = m.lw(context + 296);
        let b = m.lw(context + 304);
        m.sw(context + 296, a & 0xfff0_ffff);
        let c = m.lw(context + 300);
        m.sw(context + 304, b & 0xfff0_ffff);
        m.sw(context + 300, c & 0xfff0_ffff);
    }
    let mode = m.lw(camera + 80);
    match mode {
        1 => {
            cockpit_camera(m, camera);
            if context_word(m, 300) & 0x4000 != 0 {
                let index = m.lw(camera) & 0x3f;
                let target = entity(m, index);
                switch_camera(m, camera, 2, target);
                let context = game_context(m);
                m.sb(context + 927, mode as u8);
            }
        }
        2 => {
            look_camera(m, camera);
            if context_word(m, 300) & 0x4000 != 0 {
                let target = entity(m, 0);
                switch_camera(m, camera, 1, target);
                let context = game_context(m);
                m.sb(context + 927, 0);
            }
        }
        4 => sway_camera(m, camera),
        16 => pull_away_camera(m, camera),
        32 => orbit_camera(m, camera),
        64 => pull_back_camera(m, camera),
        256 => swing_camera(m, camera),
        512 => {
            replay_camera(m, camera);
            manual_look(m, camera);
            if context_byte(m, 933) == 0 {
                let flags = m.lw(camera);
                m.sw(camera, flags | 0x800);
            }
        }
        1024 => cockpit_camera(m, camera),
        2048 => {
            let target = m.lw(camera + 0xc0);
            chase_camera(m, camera, target);
            let context = game_context(m);
            let held = m.lw(context + 300) & 0x0060_f800;
            if !m.lw(context) & 0x2000_0000 != 0 {
                let timer = m.lw(camera + 208).wrapping_add(1);
                m.sw(camera + 208, timer);
                if !slt(timer, 181) || held != 0 {
                    let view = if m.lbu(context + 927) == 0 { 1 } else { 2 };
                    let target = entity(m, 0);
                    switch_camera(m, camera, view, target);
                    dispatch::call(m, 0x8003_5740, [1, 0, 0, 0]);
                }
            }
        }
        0x2000_0000 => {
            let context = game_context(m);
            if m.lhu(context + 94) & 0x8000 != 0 {
                let buttons = u32::from(m.lhu(context + 94));
                debug_orbit_camera(m, camera, buttons);
            } else {
                look_camera(m, camera);
            }
            if context_word(m, 300) & 0x4000 != 0 {
                let mut index = (m.lw(camera) & 0x3f) + 1;
                while slt(index, 64) {
                    let target = entity(m, index);
                    if !m.lw(target) & 0x4000 == 0 {
                        let mode = m.lw(camera + 80);
                        switch_camera(m, camera, mode, target);
                        break;
                    }
                    index += 1;
                }
                if index == 64 {
                    let target = entity(m, 0);
                    switch_camera(m, camera, 1, target);
                    dispatch::call(m, 0x8003_5740, [1, 0, 0, 0]);
                }
            }
        }
        0x4000_0000 => {
            let context = game_context(m);
            let buttons = u32::from(m.lhu(context + 94));
            free_camera(m, camera + 4, camera + 0x64, camera + 0x54, buttons);
            if context_word(m, 300) & 0x4000 != 0 {
                let mut index = (m.lw(camera) & 0x3f) + 1;
                while slt(index, 64) {
                    let target = entity(m, index);
                    if !m.lw(target) & 0x4000 == 0 {
                        switch_camera(m, camera, 0x2000_0000, target);
                        break;
                    }
                    index += 1;
                }
            }
        }
        _ => {}
    }
    if m.lw(camera) & 0x100 != 0 {
        follow_object(m, camera);
    }
    let flags = m.lw(camera);
    if flags & 0x800 != 0 && !flags & 0x1000 != 0 {
        m.sw(camera, flags & !0x800);
        let gameplay = m.gw(GAMEPLAY);
        if !m.lw(gameplay) & 0x10 != 0 {
            let lift = ground_clearance(m, camera + 4);
            let y = m.lw(camera + 8);
            m.sw(camera + 8, y.wrapping_add(lift));
        } else {
            clamp_to_box(m, camera + 0x7c, camera + 0x8c, camera + 4);
        }
    }
    let flags = m.lw(camera);
    if !flags & 0x400 != 0 {
        for (i, offset) in [4u32, 8, 12].into_iter().enumerate() {
            let value = m.lw(camera + offset).wrapping_sub(old[i]);
            m.sw(camera + 156 + i as u32 * 4, value);
        }
        for (i, offset) in [20u32, 22, 24].into_iter().enumerate() {
            let value = m.lhu(camera + offset).wrapping_sub(old_angles[i]);
            m.sh(camera + 172 + i as u32 * 2, value);
        }
    } else {
        m.sw(camera, flags & !0x400);
        m.sw(camera + 156, 0);
        m.sw(camera + 160, 0);
        m.sw(camera + 164, 0);
        m.sh(camera + 172, 0);
        m.sh(camera + 174, 0);
        m.sh(camera + 176, 0);
    }
    if context_word(m, 0) & 1 != 0 {
        debug_camera(m, camera);
    }
    m.pop_frame(0x38);
}

/// 8007AFFC (JP; PAL 8007998C, changed): camera mode 512, the replay camera. The shot block at
/// `sp+0x30` names the subject (+0x10C or +0x134 objects, swapped when the
/// camera's side +0x15C allows it), the reference (+0x124 or +0x14C) and
/// the camera's object (+0x2C). When no shot runs (+0xCC = 0) a new one
/// starts: the camera is marked cut (8007786C), its pose saved at +0xF4,
/// and a script chosen from the tables 800B40AC/800B40DC/800B40EC/`gp+0x2B4`
/// by the held buttons or the random sequence 800B4104; `[gp+0x534]` keeps
/// it. Each frame the script (`jalr`, state at +0x160) poses +0x164, which
/// is used directly or eased into +0xF4 (8007AE40) and copied to the pose.
///
/// JP: shots last 450 frames (0x1C2; PAL 750), held and pressed buttons always count (PAL ignored them in replay kind 3), and the swap of subject and reference needs context +0x24 bit 0x100 (PAL 0x200).
pub fn replay_camera(m: &mut Machine, camera: u32) {
    let frame = m.push_frame(0x58);
    let shot = frame + 0x10;
    let context = game_context(m);
    let held = m.lw(context + 0x12c);
    let pressed = m.lw(context + 0x128);
    m.sw(shot, camera + 0x10c);
    m.sw(shot + 8, camera + 0x124);
    m.sw(shot + 4, camera + 0x134);
    m.sw(shot + 0xc, camera + 0x14c);
    m.sw(shot + 0x1c, 0x1c2);
    m.sw(shot + 0x10, u32::from(held & 0x0020_9b80 != 0));
    m.sw(shot + 0x18, u32::MAX);
    if pressed & 0x0020_9b00 != 0 {
        m.sw(shot + 0x1c, 0);
    }
    for (bit, pick) in [
        (0x100u32, 0u32),
        (0x200, 3),
        (0x800, 1),
        (0x1000, 2),
        (0x8000, 4),
        (0x0020_0000, 5),
    ] {
        if pressed & bit != 0 {
            m.sw(shot + 0x18, pick);
        }
    }
    let swap = lh(m, camera + 0x15c) != 0 && m.lw(context + 0x24) & 0x100 != 0;
    m.sw(shot + 0x14, u32::from(swap));
    if lh(m, camera + 0x15c) == 2 {
        m.sw(shot + 0x14, 0);
    }
    m.sw(frame + 0x3c, camera + 0x2c);
    let roles: Option<[u32; 3]> = match m.lw(shot + 0x14) {
        0 => Some([0, 8, 4]),
        1 => Some([4, 0xc, 0]),
        _ => None,
    };
    if let Some([first, third, second]) = roles {
        let value = m.lw(shot + first);
        m.sw(frame + 0x30, value);
        let value = m.lw(shot + third);
        m.sw(frame + 0x38, value);
        let value = m.lw(shot + second);
        m.sw(frame + 0x34, value);
    }
    let state = m.lw(camera + 0xcc);
    if state == 0 {
        mark_camera_cut(m, camera);
        let position = [m.lw(camera + 4), m.lw(camera + 8), m.lw(camera + 12)];
        let angles = [m.lhu(camera + 20), m.lhu(camera + 22), m.lhu(camera + 24)];
        m.sw(camera + 0x160, 0);
        for (i, value) in (0u32..).zip(position) {
            m.sw(camera + 0xf4 + i * 4, value);
        }
        for (i, value) in (0u32..).zip(angles) {
            m.sh(camera + 0x104 + i * 2, value);
        }
        let mut pick = m.lw(shot + 0x18);
        if slt(pick, 0) {
            let counter = m.lw(camera + 0xf0);
            pick = lbu(m, 0x800b_4104 + (counter & 0x1f));
            m.sw(camera + 0xf0, counter.wrapping_add(1));
        }
        m.sw(camera + 0xec, pick);
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(gameplay);
        let side = lh(m, camera + 0x15c);
        let pick = m.lw(camera + 0xec) as i32;
        let script = if !flags & 0x10 != 0 {
            if side == 0 {
                0x800b_40ac + ((pick.wrapping_rem(6) as u32) << 3)
            } else {
                0x800b_40dc + ((pick.wrapping_rem(2) as u32) << 3)
            }
        } else if side == 0 {
            0x800b_40ec + ((pick.wrapping_rem(3) as u32) << 3)
        } else {
            crate::GP + 0x2b4
        };
        m.set_gw(0x534, script);
        m.sw(camera + 0xcc, 1);
    }
    if state <= 1 {
        let script = m.gw(0x534);
        let mut limit = m.lw(shot + 0x1c);
        let cap = lh(m, script + 4);
        if slt(cap, limit) {
            limit = cap;
        }
        let target = m.lw(script);
        let finished = dispatch::call(m, target, [frame + 0x30, camera + 0x160, limit, script]);
        let script = m.gw(0x534);
        if lh(m, script + 6) == 1 {
            ease_pose(m, camera + 0x164, camera + 0xf4);
            m.sw(camera + 0x34, camera + 0xf4);
        } else {
            m.sw(camera + 0x34, camera + 0x164);
        }
        if finished != 0 || m.lw(shot + 0x10) != 0 || lh(m, camera + 0x15e) != 0 {
            m.sh(camera + 0x15e, 0);
            m.sw(shot + 0x10, 0);
            m.sw(camera + 0xcc, 0);
        }
    }
    let pose = m.lw(camera + 0x34);
    for offset in [0u32, 4, 8, 12, 16, 20] {
        let value = m.lw(pose + offset);
        m.sw(camera + 4 + offset, value);
    }
    m.pop_frame(0x58);
}

/// 8008FFAC (JP; PAL 8008E73C, imm:3): screen overlay toggles (context +946 bits, buttons at +96):
/// bit 0 draws the two-tile full-screen blend (8008F950); bits 1 and 2
/// raise the flare state and bit 1 also draws the noise overlay
/// (8008488C).
///
/// JP: the toggles at context half +0x3AE (PAL +0x3B2).
pub fn draw_overlays(m: &mut Machine, work: u32) {
    m.push_frame(0x20);
    let context = game_context(m);
    let mut flags = lh(m, context + 942);
    let buttons = lhu(m, context + 96);
    if flags & 4 != 0 {
        let systems = m.gw(OBJECT_SYSTEMS);
        let state = m.lw(systems + 1480);
        flags &= !4;
        m.sw(systems + 1480, state | 1);
        m.sh(context + 942, flags as u16);
    }
    if buttons & 0x300 != 0 {
        if buttons & 0x200 != 0 {
            flags ^= 1;
        }
        if buttons & 0x100 != 0 {
            if flags & 2 != 0 {
                flags |= 4;
            }
            flags ^= 2;
        }
        let context = game_context(m);
        m.sh(context + 942, flags as u16);
    }
    if flags & 1 != 0 {
        let ot = m.lw(work);
        let cursor = m.lw(work + 4);
        let next = screen_blend(m, ot.wrapping_sub(4), cursor, 0);
        m.sw(work + 4, next);
    }
    if flags & 6 != 0 {
        let systems = m.gw(OBJECT_SYSTEMS);
        let state = m.lw(systems + 1480);
        m.sw(systems + 1480, state | 1);
        if flags & 2 != 0 {
            let ot = m.lw(work);
            let cursor = m.lw(work + 4);
            let next = noise_overlay(m, ot, cursor, 1024);
            m.sw(work + 4, next);
        }
    }
    m.pop_frame(0x20);
}

/// 8008F950 (JP; PAL 8008E0E0, imm:1): full-screen blend: a draw-mode packet, an optional offset
/// (from `rect`), two 256-wide semi-transparent sprites (colour 0x838383,
/// subtractive) over the display buffer, chained into `slot`. Returns the
/// new cursor.
///
/// JP: sprites 256x240 (PAL 256x256).
pub fn screen_blend(m: &mut Machine, slot: u32, packet: u32, rect: u32) -> u32 {
    let mut cursor = packet + 8;
    m.sw(packet, link(cursor) | 0x0100_0000);
    m.sw(packet + 4, 0xe600_0001);
    let context = game_context(m);
    let page = u32::from(m.lw(context + 4) == 0) << 8;
    if rect != 0 {
        let next = packet + 20;
        let other = (page + 256) & 0x100;
        m.sw(packet + 8, link(next) | 0x0200_0000);
        m.sw(cursor + 4, (other << 11) | 0xe500_0000);
        m.sw(cursor + 8, 0);
        cursor = next;
    }
    let tpage = ((page & 0x100) >> 4) | ((page & 0x200) << 2) | 0xe100_0200;
    let mut x = 0u32;
    while slt(x, 320) {
        let next = cursor + 24;
        m.sw(cursor, link(next) | 0x0500_0000);
        m.sw(cursor + 4, sra(x & 0x3ff, 6) | 0x100 | tpage);
        m.sw(cursor + 8, 0x6683_8383);
        m.sw(cursor + 12, x);
        m.sh(cursor + 16, 0);
        m.sw(cursor + 20, 0x00f0_0100);
        x += 256;
        cursor = next;
    }
    let last;
    if rect != 0 {
        let offset = m.lw(rect);
        last = cursor;
        m.sw(cursor, 0x0200_0000);
        m.sw(cursor + 8, 0);
        let ox = offset & 0x7ff;
        let oy = (offset << 5) >> 21;
        m.sw(cursor + 4, ox | (oy << 11) | 0xe500_0000);
        cursor += 12;
    } else {
        last = cursor - 24;
    }
    insert_chain(m, slot, packet, last);
    cursor
}

/// 8008488C (JP; PAL 80083244, imm:2): a full-screen noise layer of `level` (0: nothing; negative:
/// subtractive blend mode 2, else additive mode 1): a DR_MODE packet
/// (texture page 0x218 with the blend mode, texture window 0xE20C4318),
/// the noise tiles of 80084318 (owned by the loader; colour 0x668300
/// faded by 4096 - |level| through RGBC/IR0, CLUT 18134, 320x256 from
/// (-160, -128)) and a closing DR_MODE, chained in at `ot`. Returns the
/// packet cursor after it.
///
/// JP: the noise covers 320x240 from (-160, -120) (PAL 256 lines from -128).
pub fn noise_overlay(m: &mut Machine, ot: u32, cursor: u32, level: u32) -> u32 {
    m.push_frame(0x20);
    if level == 0 {
        m.pop_frame(0x20);
        return cursor;
    }
    let (mode, level) = if slt(level, 0) {
        (2u32, 0u32.wrapping_sub(level))
    } else {
        (1, level)
    };
    m.sw(cursor, link(cursor + 16) | 0x0300_0000);
    m.sw(cursor + 4, (mode << 5) | 0xe100_0218);
    m.sw(cursor + 8, 0xe20c_4318);
    m.sw(cursor + 12, 0);
    m.gte.mtc2(reg::RGBC, 0x6600_8300);
    m.gte.mtc2(reg::IR0, 4096u32.wrapping_sub(level));
    let end = dispatch::call(
        m,
        0x8008_4318,
        [cursor + 16, 18134, 0xff88_ff60, 0x00f0_0140],
    );
    m.sw(end + 4, 0xe100_0220);
    m.sw(end, 0x0300_0000);
    m.sw(end + 8, 0xe200_0000);
    m.sw(end + 12, 0);
    insert_chain(m, ot, cursor, end);
    m.pop_frame(0x20);
    end + 16
}

/// 8008C644 (JP; PAL 8008AE18, imm:2) (work, record): the heat shimmer of a lit object (callback
/// 8008C0A0, record +0x10 mask): up to 16 points interpolated from the
/// record's +0x14 offset toward its far colour by a decaying spread,
/// jittered by the noise halfwords at `[gp+0x41C]`, each a VRAM strip copy
/// of the frame below it and a 16x5 sprite drawn back over it; chained
/// between texture page packets at the record's depth slot.
///
/// JP: the screen bound |y| < 0x77 (PAL 0x7F) and the 120-line centre (PAL 128).
pub fn heat_shimmer(m: &mut Machine, work: u32, record: u32) {
    let start = m.lw(work + 4);
    let noise = m.gw(OBJECT_SYSTEMS);
    let mut packet = start + 0xc;
    m.sw(start, link(packet) | 0x0200_0000);
    m.sw(start + 4, 0xe600_0001);
    m.sw(start + 8, 0xe100_0217);
    let mut phase = sra(m.lw(work + 0x3fc), 3).wrapping_sub(m.lw(record + 4) << 1) & 0xff;
    let colour = lh(m, record + 0x1c);
    m.gte.ctc2(ctrl::RFC, colour);
    let colour = lh(m, record + 0x1e);
    m.gte.ctc2(ctrl::GFC, colour);
    let colour = lh(m, record + 0x20);
    m.gte.ctc2(ctrl::BFC, colour);
    let mut spread = lh(m, record + 0x4e);
    let jitter = lhu(m, noise + (phase << 1)) & 0x7f;
    let mut mask = m.lw(record + 0x10);
    let context = game_context(m);
    let mut level = lh(m, record + 0x4c).wrapping_sub(jitter);
    let page = u32::from(m.lw(context + 4) != 0) << 8;
    for _ in 0..16 {
        if mask == 0 {
            break;
        }
        level = level.wrapping_add(spread);
        spread = spread.wrapping_sub(sra(spread, 6));
        phase = phase.wrapping_add(2);
        if mask & 1 != 0 {
            m.gte.mtc2(reg::IR0, level);
            for (index, offset) in [0x14u32, 0x16, 0x18].into_iter().enumerate() {
                let value = lh(m, record + offset);
                m.gte.mtc2(reg::IR1 + index, value);
            }
            cop2(m, INTPL);
            let range = lh(m, record + 0x48);
            let dx = if range != 0 {
                let value = lh(m, noise + ((phase & 0xff) << 1));
                let rest = (value as i32).wrapping_rem(range as i32) as u32;
                sra(mul_lo(rest, level.wrapping_add(0x200)), 11)
            } else {
                0
            };
            m.sh(record + 0x54, dx as u16);
            let range = lh(m, record + 0x4a);
            let dy = if range != 0 {
                let value = lh(m, noise + ((phase.wrapping_add(1) & 0xff) << 1));
                let rest = (value as i32).wrapping_rem(range as i32) as u32;
                rest.wrapping_add(sra(mul_lo(rest, level), 12))
            } else {
                0
            };
            m.sh(record + 0x56, dy as u16);
            let mac = m.gte.mfc2(reg::MAC1);
            let value = lhu(m, record + 0x54).wrapping_add(mac);
            m.sh(record + 0x54, value as u16);
            let mac = m.gte.mfc2(reg::MAC2);
            let value = lhu(m, record + 0x56).wrapping_add(mac);
            m.sh(record + 0x56, value as u16);
            lwc2(m, reg::VXY0, record + 0x54);
            let mac = m.gte.mfc2(reg::MAC3);
            m.gte.mtc2(reg::VZ0, mac);
            let next = packet + 0x28;
            m.sw(packet, link(next) | 0x0900_0000);
            m.sw(packet + 4, 0x0100_0000);
            m.sw(packet + 8, 0x8000_0000);
            cop2(m, RTPS);
            m.sw(packet + 0x14, 0x0001_0010);
            m.sw(packet + 0x18, 0x6700_0000);
            m.sw(packet + 0x10, 0x012b_0160);
            m.sh(packet + 0x22, 0x4ad6);
            m.sw(packet + 0x24, 0x0005_0010);
            swc2(m, reg::SXY2, record + 0x5c);
            let x = lhu(m, record + 0x5c);
            if slt(abs(sext16(x)), 0x99) && slt(abs(lh(m, record + 0x5e)), 0x77) {
                m.sh(record + 0x5c, x.wrapping_sub(8) as u16);
                m.sh(packet + 0xc, (x.wrapping_add(0x98) & 0x3ff) as u16);
                let y = lhu(m, record + 0x5e);
                m.sh(packet + 0xe, (y.wrapping_add(page + 0x78) & 0x1ff) as u16);
                let y = lhu(m, record + 0x5e);
                m.sh(record + 0x5e, y.wrapping_sub(2) as u16);
                let row = sra(phase, 1) & 0xf;
                let screen = m.lw(record + 0x5c);
                m.sb(packet + 0x20, 0);
                m.sb(packet + 0x21, row as u8);
                m.sw(packet + 0x1c, screen);
                packet = next;
            }
        }
        mask >>= 2;
    }
    if packet != start + 0xc {
        m.sw(packet, 0x0200_0000);
        m.sw(packet + 4, 0xe600_0000);
        m.sw(packet + 8, 0xe100_0220);
        let mut depth = m.lw(record + 0x44).wrapping_sub(2);
        if slt(depth, (-0x13_i32) as u32) {
            depth = (-0x13_i32) as u32;
        }
        let slot = (depth << 2).wrapping_add(m.lw(work + 0x3f8));
        let first = m.lw(work + 4);
        insert_chain(m, slot, first, packet);
        m.sw(work + 4, packet + 0xc);
    }
}

/// 80092168 (JP; PAL 800908F8, imm:2) (a, b, near, planes) -> 1 when the view-space segment a..b is
/// at least partly visible, clipped in place to z >= `near` and to the
/// four side planes of the frustum (corners (+-0xB8, +-0x98, H) at
/// `planes`, the current plane normal at `planes + 0x40`); 0 when it is
/// outside.
///
/// JP: the frustum corners +-0xB8, +-0x90 (PAL +-0x98).
pub fn clip_segment(m: &mut Machine, a: u32, b: u32, near: u32, planes: u32) -> u32 {
    m.push_frame(0x30);
    let visible = 'clip: {
        let za = m.lw(a + 8);
        let zb = m.lw(b + 8);
        let (a_behind, b_behind) = (slt(za, near), slt(zb, near));
        if a_behind && b_behind {
            break 'clip 0;
        }
        if a_behind || b_behind {
            let t = div_checked(
                m,
                near.wrapping_sub(za) << 12,
                zb.wrapping_sub(za),
                0x8009_21d4,
            );
            let out = if slt(za, zb) { a } else { b };
            interpolate_point(m, a, b, out, t);
        }
        let h = m.gte.cfc2(ctrl::H);
        // Widescreen: the side planes 1/k wider (0xB8 / k).
        let side = widescreen::unsqueeze_x(m, 0xb8);
        let corners: [(u32, u32, u32); 4] = [
            (0, side, 0x90),
            (0x10, side.wrapping_neg(), 0x90),
            (0x20, side.wrapping_neg(), (-0x90_i32) as u32),
            (0x30, side, (-0x90_i32) as u32),
        ];
        for (offset, x, y) in corners {
            m.sw(planes + offset, x);
            m.sw(planes + offset + 4, y);
            m.sw(planes + offset + 8, h);
        }
        let normal = planes + 0x40;
        for side in 0..4u32 {
            let next = planes + (((side + 1) & 3) << 4);
            outer_product0(m, planes + (side << 4), next, normal);
            let da = plane_distance(m, normal, a);
            let db = plane_distance(m, normal, b);
            if slt(da, 0) {
                if slt(db, 0) {
                    break 'clip 0;
                }
            } else if !slt(db, 0) {
                continue;
            }
            let t = div_checked(m, da << 12, da.wrapping_sub(db), 0x8009_22dc);
            let out = if slt(da, db) { a } else { b };
            interpolate_point(m, a, b, out, t);
        }
        1
    };
    m.pop_frame(0x30);
    visible
}

/// 80089F44 (JP; PAL 800887D4, imm:1) (piece, work): debris kind 0, a fading spark star: projected
/// from the eye, grey by the remaining life over its second half (faded
/// beyond depth 10000), the radius by the piece size +0x16, the projection
/// distance and the sine of its age; a dot below radius 2, else a fan of
/// gouraud triangles (below 25) or a ring of quads around a hole, chained
/// with the draw-mode packet work +0x78 at its depth slot.
///
/// JP: the screen bound `radius + 0x78` (PAL 0x80).
pub fn draw_debris_star(m: &mut Machine, piece: u32, work: u32) {
    m.push_frame(0x28);
    'body: {
        for axis in 0..3u32 {
            let offset = axis * 4;
            let value = sra(
                m.lw(piece + 4 + offset)
                    .wrapping_sub(m.lw(SCRATCH + 8 + offset)),
                12,
            );
            m.sh(work + 0x10 + axis * 2, value as u16);
            if slt(0x7fff, abs(value)) {
                break 'body;
            }
        }
        lwc2(m, reg::VXY0, work + 0x10);
        lwc2(m, reg::VZ0, work + 0x14);
        cop2(m, RTPS);
        let mut life = lh(m, piece + 2);
        let age = lh(m, piece);
        if !slt(0, life) {
            life = 1;
        }
        let grey = if !slt(sra(life, 1), age) {
            0x24
        } else {
            let left = life.wrapping_sub(age);
            div_checked(m, left.wrapping_mul(9) << 3, life, 0x8008_a03c)
        };
        m.sw(work, grey);
        if (m.gte.cfc2(ctrl::FLAG) as i32) < 0 {
            break 'body;
        }
        let mut depth = m.gte.mfc2(reg::SZ3);
        swc2(m, reg::SXY2, work + 0x18);
        if depth == 0 {
            depth = 1;
        }
        let phase = div_checked(m, age << 10, life, 0x8008_a094);
        let distance = sext16(u32::from(m.lhu(SCRATCH + 0x1a)));
        let size = mul_lo(lh(m, piece + 0x16), distance);
        let size = div_checked(m, size, depth, 0x8008_a0e0);
        let sine = sdk::rsin(m, phase);
        let radius = scale_q12(sine, size);
        if slt(radius.wrapping_add(0xa0), abs(lh(m, work + 0x18))) {
            break 'body;
        }
        if slt(radius.wrapping_add(0x78), abs(lh(m, work + 0x1a))) {
            break 'body;
        }
        m.sh(work + 4, radius as u16);
        let slot = debris_depth(sra(depth, 3).wrapping_sub(0xc));
        let ot = m.lw(SCRATCH).wrapping_add(slot << 2);
        let mut level = m.lw(work);
        m.sw(work + 0xc, ot);
        if !slt(depth, 0x2711) {
            let product = mul_lo(level, 0xb3b0u32.wrapping_sub(depth));
            let scaled = mul_hi(product, 0xb65c_6d59).wrapping_add(product);
            level = sra(scaled, 15).wrapping_sub(sra(product, 31)) & 0xff;
        }
        let grey = level | (level << 8) | (level << 16);
        m.sw(work, grey);
        if slt(lh(m, work + 4), 2) {
            let slot = m.lw(work + 0xc);
            let dot = m.lw(SCRATCH + 4);
            let tag = m.lw(slot) | 0x0200_0000;
            m.sw(dot, tag);
            let slot = m.lw(work + 0xc);
            m.sw(slot, link(dot));
            let colour = m.lw(work) | 0x6a00_0000;
            m.sw(dot + 4, colour);
            let xy = m.lw(work + 0x18);
            m.sw(dot + 8, xy);
            m.sw(SCRATCH + 4, dot + 0xc);
            break 'body;
        }
        let first = m.lw(SCRATCH + 4);
        m.sw(first, link(first + 8) | 0x0100_0000);
        m.sw(first + 4, 0xe600_0001);
        let shift = ray_shift(lh(m, work + 4));
        let rays = sra(0x20, shift);
        let mut packet = first + 8;
        let end = if slt(lh(m, work + 4), 0x19) {
            let last = (rays * 28).wrapping_add(packet).wrapping_sub(0x1c);
            let x = m.lhu(work + 0x18);
            m.sh(last + 0x18, x);
            m.sh(packet + 0x10, x);
            let y = m.lhu(work + 0x1a).wrapping_add(m.lhu(work + 4));
            m.sh(last + 0x1a, y);
            m.sh(packet + 0x12, y);
            for ray in 1..rays {
                let next = packet + 0x1c;
                m.sw(packet, link(next) | 0x0600_0000);
                m.sw(packet + 4, 0x3200_0000);
                let colour = m.lw(work);
                m.sw(packet + 0x14, colour);
                m.sw(packet + 0xc, colour);
                let centre = m.lw(work + 0x18);
                m.sw(packet + 8, centre);
                let radius = lh(m, work + 4);
                let (x, y) = ray_point(m, work, ray << shift, radius);
                m.sh(next + 0x10, x as u16);
                m.sh(packet + 0x18, x as u16);
                m.sh(next + 0x12, y as u16);
                m.sh(packet + 0x1a, y as u16);
                packet = next;
            }
            let end = packet + 0x1c;
            m.sw(packet, link(end) | 0x0600_0000);
            m.sw(packet + 4, 0x3200_0000);
            let colour = m.lw(work);
            m.sw(packet + 0x14, colour);
            m.sw(packet + 0xc, colour);
            let centre = m.lw(work + 0x18);
            m.sw(packet + 8, centre);
            end
        } else {
            let inner = m.lhu(work + 4).wrapping_sub(0x18);
            let last = (rays * 36).wrapping_add(packet).wrapping_sub(0x24);
            m.sh(work + 6, inner);
            let x = m.lhu(work + 0x18);
            for at in [last + 0x20, last + 0x18, packet + 0x10, packet + 8] {
                m.sh(at, x);
            }
            let y = m.lhu(work + 0x1a).wrapping_add(m.lhu(work + 6));
            m.sh(last + 0x1a, y);
            m.sh(packet + 0xa, y);
            let y = m.lhu(work + 0x1a).wrapping_add(m.lhu(work + 4));
            m.sh(last + 0x22, y);
            m.sh(packet + 0x12, y);
            for ray in 1..rays {
                let next = packet + 0x24;
                m.sw(packet, link(next) | 0x0800_0000);
                m.sw(packet + 4, 0x3a00_0000);
                m.sw(packet + 0x14, 0);
                let colour = m.lw(work);
                m.sw(packet + 0x1c, colour);
                m.sw(packet + 0xc, colour);
                let index = ray << shift;
                let inner = lh(m, work + 6);
                let (x, y) = ray_point(m, work, index, inner);
                m.sh(next + 8, x as u16);
                m.sh(packet + 0x18, x as u16);
                m.sh(next + 0xa, y as u16);
                m.sh(packet + 0x1a, y as u16);
                let outer = lh(m, work + 4);
                let (x, y) = ray_point(m, work, index, outer);
                m.sh(next + 0x10, x as u16);
                m.sh(packet + 0x20, x as u16);
                m.sh(next + 0x12, y as u16);
                m.sh(packet + 0x22, y as u16);
                packet = next;
            }
            let end = packet + 0x24;
            m.sw(packet, link(end) | 0x0800_0000);
            m.sw(packet + 4, 0x3a00_0000);
            m.sw(packet + 0x14, 0);
            let colour = m.lw(work);
            m.sw(packet + 0x1c, colour);
            m.sw(packet + 0xc, colour);
            end
        };
        m.sw(end, 0x0100_0000);
        let mode = m.lw(work + 0x78);
        m.sw(end + 4, mode);
        let slot = m.lw(work + 0xc);
        let first = m.lw(SCRATCH + 4);
        insert_chain(m, slot, first, end);
        m.sw(SCRATCH + 4, end + 8);
    }
    m.pop_frame(0x28);
}

/// 8008ABF0 (JP; PAL 80089480, imm:1) (piece, work): debris kind 2, a glowing ember: projected from
/// the eye, the radius by the piece size, the projection distance and the
/// sine of a quarter of its age over its life (at most 180), skipped off
/// screen; the colour 0x6F2FAF depth-cued toward 0x30 then by the age; a
/// fan of gouraud triangles between texture-page packets at its depth slot.
///
/// JP: the screen bound 0x79 (PAL 0x81).
pub fn draw_debris_ember(m: &mut Machine, piece: u32, work: u32) {
    m.push_frame(0x28);
    'body: {
        let Some([x, y, z]) = debris_relative(m, piece) else {
            break 'body;
        };
        m.gte.mtc2(reg::VXY0, (x & 0xffff) | (y << 16));
        m.gte.mtc2(reg::VZ0, z);
        let age = lh(m, piece);
        let mut life = lh(m, piece + 2);
        cop2(m, RTPS);
        if !slt(0, life) {
            life = 1;
        }
        let phase = div_checked(m, age << 12, life, 0x8008_acac);
        let distance = sext16(u32::from(m.lhu(SCRATCH + 0x1a)));
        let size = mul_lo(lh(m, piece + 0x16), distance);
        let quarter = sra(
            if slt(phase, 0) {
                phase.wrapping_add(3)
            } else {
                phase
            },
            2,
        );
        let sine = sdk::rsin(m, quarter);
        let size = scale_q12(size, sine);
        if (m.gte.cfc2(ctrl::FLAG) as i32) < 0 {
            break 'body;
        }
        let mut depth = m.gte.mfc2(reg::SZ3);
        let screen = m.gte.mfc2(reg::SXY2);
        if depth == 0 {
            depth = 1;
        }
        let mut radius = div_checked(m, size, depth, 0x8008_ad40);
        if !slt(0, radius) {
            break 'body;
        }
        if !slt(radius, 0xb5) {
            radius = 0xb4;
        }
        if !slt(abs(sra(screen, 16)).wrapping_sub(radius), 0x79) {
            break 'body;
        }
        if !slt(abs(sext16(screen)).wrapping_sub(radius), 0xa1) {
            break 'body;
        }
        for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
            m.gte.ctc2(register, 0x30);
        }
        m.gte.mtc2(reg::RGBC, 0x326f_2faf);
        let slot = sra(depth, 3).wrapping_sub(0xc);
        cop2(m, DPCS);
        let slot = debris_depth(sra(slot, 3).wrapping_sub(0xc));
        let ot = m.lw(SCRATCH).wrapping_add(slot << 2);
        m.sw(work + 0xc, ot);
        let cued = m.gte.mfc2(reg::RGB2);
        m.gte.mtc2(reg::IR0, phase);
        m.gte.mtc2(reg::RGBC, cued);
        m.sw(work + 0x18, screen);
        m.sh(work + 4, radius as u16);
        cop2(m, DPCS);
        let shift = ray_shift(radius);
        let rays = sra(0x20, shift);
        let colour = m.gte.mfc2(reg::RGB2);
        let first = m.lw(SCRATCH + 4);
        let mut packet = first + 0x10;
        m.sw(first, link(first + 8) | 0x0100_0000);
        m.sw(first + 8, link(packet) | 0x0100_0000);
        let last = (rays * 28).wrapping_add(packet).wrapping_sub(0x1c);
        m.sw(first + 4, 0xe600_0001);
        m.sw(first + 0xc, 0xe100_0240);
        let x = m.lhu(work + 0x18);
        m.sh(last + 0x18, x);
        m.sh(packet + 0x10, x);
        let y = m.lhu(work + 0x1a).wrapping_add(m.lhu(work + 4));
        m.sh(last + 0x1a, y);
        m.sh(packet + 0x12, y);
        for ray in 1..rays {
            let next = packet + 0x1c;
            m.sw(packet, link(next) | 0x0600_0000);
            m.sw(packet + 4, colour);
            m.sw(packet + 0x14, 0x0003_0303);
            m.sw(packet + 0xc, 0x0003_0303);
            m.sw(packet + 8, screen);
            let radius = lh(m, work + 4);
            let (x, y) = ray_point(m, work, ray << shift, radius);
            m.sh(next + 0x10, x as u16);
            m.sh(packet + 0x18, x as u16);
            m.sh(next + 0x12, y as u16);
            m.sh(packet + 0x1a, y as u16);
            packet = next;
        }
        let page = packet + 0x1c;
        let end = packet + 0x24;
        m.sw(packet, link(page) | 0x0600_0000);
        m.sw(packet + 4, colour);
        m.sw(packet + 0x14, 0x0003_0303);
        m.sw(packet + 0xc, 0x0003_0303);
        m.sw(packet + 8, screen);
        m.sw(page, link(end) | 0x0100_0000);
        m.sw(page + 4, 0xe100_0220);
        m.sw(end, 0x0100_0000);
        let mode = m.lw(work + 0x78);
        m.sw(end + 4, mode);
        let slot = m.lw(work + 0xc);
        let first = m.lw(SCRATCH + 4);
        insert_chain(m, slot, first, end);
        m.sw(SCRATCH + 4, packet + 0x2c);
    }
    m.pop_frame(0x28);
}

/// 8008E4F8 (JP; PAL 8008CC88, imm:2): ambient type 4, rain: a grey fog TILE over the screen when
/// below the water/cloud height (`[gp+0x33C]` +4), and 64 blocks of three
/// LINE_G2 streaks: 192 points of the 0x2000-wide wrapped cube around the
/// eye (>> 10), depth-cued by their distance from the cube centre, each
/// from its projection this frame to its projection in the previous view
/// (camera motion >> 9), chained at OT slot 0xFFE.
///
/// JP: the fog TILE covers 320x240 from (-160, -120) (PAL 256 lines from -128).
pub fn draw_rain(m: &mut Machine) {
    m.push_frame(0x30);
    save_rotation(m, WORK);
    let eye_y = m.lw(SCRATCH + 0xc);
    let water = m.gw(SUN_STATE);
    let height = m.lw(water + 4);
    let base = ambient_base(m, 10, 0);
    let mut fog = sra(eye_y.wrapping_add(height), 14);
    if slt(fog, 0) {
        fog = 0;
    } else if !slt(fog, 0x1001) {
        fog = 0x1000;
    }
    if slt(0, fog) {
        m.gte.ctc2(ctrl::RFC, 0x30);
        m.gte.ctc2(ctrl::GFC, 0);
        m.gte.ctc2(ctrl::BFC, 0);
        m.gte.mtc2(reg::RGBC, 0x6203_135f);
        let mut level = 0x1080u32.wrapping_sub(fog << 1);
        if slt(level, 0) {
            level = 0;
        }
        m.gte.mtc2(reg::IR0, level);
        let tile = m.lw(SCRATCH + 4);
        m.sw(SCRATCH + 4, tile + 0x10);
        cop2(m, DPCS);
        let ot = m.lw(SCRATCH);
        let head = m.lw(ot + 4) | 0x0300_0000;
        m.sw(tile, head);
        let ot = m.lw(SCRATCH);
        m.sw(ot + 4, link(tile));
        m.sw(tile + 8, 0xff88_ff60);
        m.sw(tile + 0xc, 0x00f0_0140);
        swc2(m, reg::RGB2, tile + 4);
    }
    m.gte.mtc2(reg::RGBC, 0x5253_4333);
    m.gte.ctc2(ctrl::RFC, 0xc30);
    m.gte.ctc2(ctrl::GFC, 0xb30);
    m.gte.ctc2(ctrl::BFC, 0x1f0);
    m.gte.mtc2(reg::IR0, fog);
    let first = m.lw(SCRATCH + 4);
    cop2(m, DPCS);
    ambient_point(m, WORK + 0x20, base, 0);
    let colour = m.gte.mfc2(reg::RGB2);
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0x30);
    }
    m.gte.mtc2(reg::RGBC, colour);
    ambient_lines(m, first, base, 0x40);
    ambient_motion(m, 9, 0);
    let packet = ambient_tails(m, base, 0x40);
    let first = m.lw(SCRATCH + 4);
    if first != packet {
        let slot = m.lw(SCRATCH) + 0x3ff8;
        insert_chain(m, slot, first, packet - 0x34);
        m.sw(SCRATCH + 4, packet);
    }
    load_rotation(m, WORK);
    m.gte.ctc2(ctrl::TRX, 0);
    m.gte.ctc2(ctrl::TRY, 0);
    m.gte.ctc2(ctrl::TRZ, 0);
    m.pop_frame(0x30);
}

/// 8008F1B8 (JP; PAL 8008D948, imm:1): ambient type 3, drifting motes: up to `weather +0x1D`
/// textured sprites (weather record gameplay +0x268, +0x1C fade, +8
/// ceiling) in the 0x4000-wide wrapped cube around the eye (>> 8, lifted
/// by context +0x28; offsets from the random words of `[gp+0x41C]` >> 9),
/// grey 0x838383 faded by the fade and the height (fewer motes near the
/// ceiling), depth-cued toward 0x100 away from depth 0x1C00 and sized by
/// depth; chained at OT slot 0x1C.
///
/// JP: the screen bound 0x79 (PAL 0x81).
pub fn draw_motes(m: &mut Machine) {
    'body: {
        let gameplay = m.gw(GAMEPLAY);
        let weather = m.lw(gameplay + 0x268);
        let mut count = lbu(m, weather + 0x1d);
        let fade = m.lw(weather + 0x1c);
        if count == 0 {
            break 'body;
        }
        for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
            m.gte.ctc2(register, 0);
        }
        m.gte.mtc2(reg::RGBC, 0x2e83_8383);
        let fade = sra(fade, 16);
        m.gte.mtc2(reg::IR0, fade);
        if !slt(fade, 0x1000) {
            break 'body;
        }
        cop2(m, DPCS);
        let level = lh(m, weather + 8).wrapping_sub(sra(m.lw(SCRATCH + 0xc), 12));
        let colour = m.gte.mfc2(reg::RGB2);
        if !slt(level, 0x1000) {
            break 'body;
        }
        m.gte.mtc2(reg::RGBC, colour);
        m.gte.mtc2(reg::IR0, level);
        if slt(0, level) {
            cop2(m, DPCS);
            count = count.wrapping_sub(scale_q12(count, level));
            let colour = m.gte.mfc2(reg::RGB2);
            if !slt(0, count) {
                break 'body;
            }
            m.gte.mtc2(reg::RGBC, colour);
        }
        for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
            m.gte.ctc2(register, 0x100);
        }
        save_rotation(m, WORK);
        let gameplay = m.gw(GAMEPLAY);
        load_rotation(m, gameplay + 0x2c);
        m.gte.mtc2(reg::VXY0, 0);
        m.gte.mtc2(reg::VZ0, 0x3800);
        cop2(m, MVMVA_RT_V0);
        let context = m.gw(crate::common::CONTEXT);
        let lift = m.lw(context + 0x28) << 7;
        let mut base = [0u32; 3];
        for (axis, value) in base.iter_mut().enumerate() {
            *value = sra(m.lw(SCRATCH + 8 + axis as u32 * 4).wrapping_neg(), 8);
        }
        base[1] = base[1].wrapping_add(lift);
        for (axis, value) in base.iter_mut().enumerate() {
            *value = value.wrapping_sub(m.gte.mfc2(reg::MAC1 + axis));
        }
        load_rotation(m, WORK);
        m.gte.ctc2(ctrl::TRZ, 0x3800);
        let wrap =
            |base: u32, offset: u32| (base.wrapping_add(offset) & 0x3fff).wrapping_sub(0x2000);
        let point = WORK + 0x20;
        m.sh(point, wrap(base[0], 0) as u16);
        let mut packet = m.lw(SCRATCH + 4);
        m.sh(point + 2, wrap(base[1], 0) as u16);
        let mut seeds = m.gw(OBJECT_SYSTEMS);
        m.sh(point + 4, wrap(base[2], 0) as u16);
        while slt(0, count) {
            lwc2(m, reg::VXY0, point);
            lwc2(m, reg::VZ0, point + 4);
            let seed = m.lw(seeds);
            seeds += 4;
            cop2(m, RTPS);
            let x = wrap(base[0], seed);
            let seed = sra(seed, 9);
            let y = wrap(base[1], seed);
            let seed = sra(seed, 9);
            m.sh(point, x as u16);
            m.sh(point + 4, wrap(base[2], seed) as u16);
            m.sh(point + 2, y as u16);
            m.sh(packet + 0x1c, 0x1ff0);
            m.sw(packet + 0xc, 0x4616_10f0);
            m.sw(packet + 0x14, 0x0038_10ff);
            m.sh(packet + 0x24, 0x1fff);
            let depth = m.gte.mfc2(reg::SZ3);
            swc2(m, reg::SXY2, WORK + 0x70);
            let mut distance = sra(depth.wrapping_sub(0x1c00), 2);
            if slt(depth.wrapping_sub(0x1c00), 0) {
                distance = sra(0x1c00u32.wrapping_sub(depth), 2);
            }
            if !slt(distance, 0x1001) {
                distance = 0x1000;
            }
            m.gte.mtc2(reg::IR0, distance);
            let mut size = sra(0x5c86u32.wrapping_sub(depth), 12);
            cop2(m, DPCS);
            if slt(size, 2) {
                size = 2;
            }
            let visible = slt(abs(lh(m, WORK + 0x70)).wrapping_sub(size), 0xa1)
                && slt(abs(lh(m, WORK + 0x72)).wrapping_sub(size), 0x79);
            if visible {
                let next = packet + 0x28;
                m.sw(packet, link(next) | 0x0900_0000);
                let half = sra(size, 1);
                // Widescreen: the width squeezed like the projection.
                let width = widescreen::squeeze_x(m, size);
                let left = lh(m, WORK + 0x70).wrapping_sub(sra(width, 1));
                let right = left.wrapping_add(width);
                m.sh(packet + 0x18, left as u16);
                m.sh(packet + 8, left as u16);
                m.sh(packet + 0x20, right as u16);
                m.sh(packet + 0x10, right as u16);
                let top = lh(m, WORK + 0x72).wrapping_sub(half);
                let bottom = top.wrapping_add(size);
                m.sh(packet + 0x12, top as u16);
                m.sh(packet + 0xa, top as u16);
                m.sh(packet + 0x22, bottom as u16);
                m.sh(packet + 0x1a, bottom as u16);
                swc2(m, reg::RGB2, packet + 4);
                packet = next;
            }
            count = count.wrapping_sub(1);
        }
        let first = m.lw(SCRATCH + 4);
        if first != packet {
            let slot = m.lw(SCRATCH) + 0x70;
            insert_chain(m, slot, first, packet - 0x28);
            m.sw(SCRATCH + 4, packet);
        }
        load_rotation(m, WORK);
        m.gte.ctc2(ctrl::TRZ, 0);
    }
}

/// 8008F580 (JP; PAL 8008DD10, imm:1): ambient type 5, falling leaves: `weather +0x1C` (at least
/// one) textured quads in the 0x4000-wide wrapped cube around the eye
/// (>> 12, the height not wrapped), each a square of 0x320 plus a random
/// byte per side in x and z, mirrored by the random bits, depth-cued grey
/// 0x838383 near and far, sorted by depth.
///
/// JP: the screen bound |y| < 0xA1 (PAL 0xA9).
pub fn draw_leaves(m: &mut Machine) {
    m.push_frame(8);
    m.gte.mtc2(reg::RGBC, 0x2e83_8383);
    for register in [ctrl::RFC, ctrl::GFC, ctrl::BFC] {
        m.gte.ctc2(register, 0x30);
    }
    save_rotation(m, WORK);
    let gameplay = m.gw(GAMEPLAY);
    load_rotation(m, gameplay + 0x2c);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0x2400);
    let mut seeds = m.gw(OBJECT_SYSTEMS);
    let mut packet = m.lw(SCRATCH + 4);
    cop2(m, MVMVA_RT_V0);
    let mut bx = sra(m.lw(SCRATCH + 8).wrapping_neg(), 12);
    let mut bz = sra(m.lw(SCRATCH + 0x10).wrapping_neg(), 12);
    let weather = m.lw(gameplay + 0x268);
    let mut count = lbu(m, weather + 0x1c);
    let by = sra(m.lw(SCRATCH + 0xc).wrapping_neg(), 12);
    if !slt(0, count) {
        count = 1;
    }
    bx = bx.wrapping_sub(m.gte.mfc2(reg::MAC1));
    let height = by.wrapping_sub(m.gte.mfc2(reg::MAC2));
    bz = bz.wrapping_sub(m.gte.mfc2(reg::MAC3));
    load_rotation(m, WORK);
    m.gte.ctc2(ctrl::TRZ, 0x2400);
    let wrap = |base: u32, offset: u32| (base.wrapping_add(offset) & 0x3fff).wrapping_sub(0x2000);
    m.sh(WORK + 0x20, wrap(bx, 0) as u16);
    for at in [WORK + 0x32, WORK + 0x2a, WORK + 0x22] {
        m.sh(at, height as u16);
    }
    m.sh(WORK + 0x24, wrap(bz, 0) as u16);
    while slt(0, count) {
        lwc2(m, reg::VXY0, WORK + 0x20);
        lwc2(m, reg::VZ0, WORK + 0x24);
        let seed = m.lw(seeds);
        seeds += 4;
        cop2(m, RTPS);
        let x = wrap(bx, seed);
        let depth_side = (sra(seed, 16) & 0xff) + 0x320;
        let left = u32::from(m.lhu(WORK + 0x20));
        let near = u32::from(m.lhu(WORK + 0x24));
        m.sh(WORK + 0x20, x as u16);
        let width = (seed & 0xff) + 0x320;
        let bits = sra(seed, 9);
        let right = left.wrapping_add(width);
        m.sh(WORK + 0x34, near.wrapping_add(depth_side) as u16);
        let z = wrap(bz, bits);
        m.sh(WORK + 0x28, left as u16);
        m.sh(WORK + 0x2c, near as u16);
        m.sh(WORK + 0x30, right as u16);
        m.sh(WORK + 0x24, z as u16);
        let depth = m.gte.mfc2(reg::SZ3);
        swc2(m, reg::SXY2, WORK + 0x70);
        'leaf: {
            if !slt(0, depth) {
                break 'leaf;
            }
            if !slt(abs(lh(m, WORK + 0x72)), 0xa1) || !slt(abs(lh(m, WORK + 0x70)), 0xfb) {
                break 'leaf;
            }
            let level = if slt(depth, 0x890) {
                0x890u32.wrapping_sub(depth) << 1
            } else {
                sra(depth.wrapping_sub(0x113f), 2)
            };
            if !slt(level, 0x1001) {
                break 'leaf;
            }
            lwc2(m, reg::VZ0, WORK + 0x34);
            lwc2(m, reg::VXY1, WORK + 0x30);
            lwc2(m, reg::VZ1, WORK + 0x2c);
            lwc2(m, reg::VXY2, WORK + 0x30);
            lwc2(m, reg::VZ2, WORK + 0x34);
            let first = m.lw(WORK + 0x70);
            m.sw(packet + 8, first);
            cop2(m, RTPT);
            if !slt(0, level) {
                m.sw(packet + 4, 0x2e83_8383);
            }
            let (u0, u1) = if bits & 1 != 0 { (0, 0x3f) } else { (0x3f, 0) };
            m.sb(packet + 0x1c, u0);
            m.sb(packet + 0xc, u0);
            m.sb(packet + 0x24, u1);
            m.sb(packet + 0x14, u1);
            let (v0, v1) = if bits & 2 != 0 {
                (0x80, 0xbf)
            } else {
                (0xbf, 0x80)
            };
            m.sb(packet + 0x15, v0);
            m.sb(packet + 0xd, v0);
            m.sb(packet + 0x25, v1);
            m.sb(packet + 0x1d, v1);
            let mut slot = sra(depth, 2);
            m.sh(packet + 0xe, 0x4ed6);
            m.sh(packet + 0x16, 0x39);
            if !slt(slot, 0xfff) {
                slot = 0xffe;
            }
            if !slt(0, m.gte.mfc2(reg::SZ3)) {
                break 'leaf;
            }
            let at = (slot << 2).wrapping_add(m.lw(SCRATCH));
            let tag = m.lw(at) | 0x0900_0000;
            m.sw(packet, tag);
            let at = (slot << 2).wrapping_add(m.lw(SCRATCH));
            m.sw(at, link(packet));
            swc2(m, reg::SXY0, packet + 0x18);
            swc2(m, reg::SXY1, packet + 0x10);
            swc2(m, reg::SXY2, packet + 0x20);
            if slt(0, level) {
                m.gte.mtc2(reg::IR0, level);
                cop2(m, DPCS);
                swc2(m, reg::RGB2, packet + 4);
            }
            packet += 0x28;
        }
        count = count.wrapping_sub(1);
    }
    m.sw(SCRATCH + 4, packet);
    load_rotation(m, WORK);
    m.gte.ctc2(ctrl::TRZ, 0);
    m.pop_frame(8);
}

// ==== loading screens =================================================================

/// 80036440 (JP; PAL 800360FC, near, `del:1 imm:1 ins:2 op:1`) (ot, prim) ->
/// next primitive: the loading screen copies the displayed 320x240 (PAL
/// 320x256) half of the frame buffer to the other half (context +4 selects
/// which; the halves stay at y 0 and 256), and sets 4 in `[[gp+0x2D4]]`.
pub fn copy_frame(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let control = m.gw(0x2d4);
    let word = m.lw(control);
    m.sw(control, word | 4);
    let context = m.gw(CONTEXT);
    let (y, source_y) = if m.lw(context + 4) != 0 {
        (0x100, 0)
    } else {
        (0, 0x100)
    };
    m.sw(frame + 0x10, 0);
    m.sw(frame + 0x14, source_y);
    m.sw(frame + 0x18, 0x140);
    m.sw(frame + 0x1c, 0xf0);
    let next = dispatch::call(m, 0x8003_63b0, [ot, prim, 0, y]);
    m.pop_frame(0x28);
    next
}

/// 800365FC (JP; PAL 800362B4, near, `del:5 imm:1 ins:8 op:1 reg:2`) (ot,
/// prim, level, color; second color as the caller's stack word +0x10) ->
/// next primitive: four gouraud quads closing a frame border of width
/// `level * 160 / 4096` from the left/right and `level * 120 / 4096` (PAL
/// `level / 32`) from the top/bottom edges of the 320x240 (PAL 320x256)
/// screen; both rounded toward zero.
pub fn border_quads(m: &mut Machine, ot: u32, prim: u32, level: u32, color: u32) -> u32 {
    let frame = m.push_frame(8);
    let inner = m.lw(frame + 0x18);
    let width = div_pow2(level.wrapping_mul(160), 12);
    let right = 0x140u32.wrapping_sub(width);
    let top = div_pow2(level.wrapping_mul(120), 12);
    let bottom = 0xf0u32.wrapping_sub(top);
    let head = color | 0x3800_0000;
    let quads = [
        [(0, 0), (0x140, 0), (width, top), (right, top)],
        [(0x140, 0xf0), (0, 0xf0), (right, bottom), (width, bottom)],
        [(0, 0xf0), (0, 0), (width, bottom), (width, top)],
        [(0x140, 0), (0x140, 0xf0), (right, top), (right, bottom)],
    ];
    for (index, quad) in quads.iter().enumerate() {
        let packet = prim + index as u32 * 0x24;
        m.sb(packet + 3, 8);
        m.sb(packet + 7, 0x38);
        m.sw(packet + 4, head);
        m.sw(packet + 0xc, color);
        m.sw(packet + 0x14, inner);
        m.sw(packet + 0x1c, inner);
        for (vertex, &(x, y)) in quad.iter().enumerate() {
            let at = packet + 8 + vertex as u32 * 8;
            m.sh(at, x as u16);
            m.sh(at + 2, y as u16);
        }
        link_prim(m, ot, packet);
    }
    m.pop_frame(8);
    prim + 0x90
}

/// 80036A84 (JP; PAL 80036724, near, `imm:3 ins:1 op:2 reg:4`) (ot, prim) ->
/// next primitive: loading visual mode 0, the fade between the briefing and
/// flight: texts, the state's fade stages by level (+2 for mode 1, full for
/// mode 2), the tiles (unless context byte +0x39E, PAL +0x3A2), the opening
/// aperture (800847A8; its height is `sine * 240 >> 12`, PAL `sine >> 4`,
/// centred on 240 lines) and the closing border.
pub fn fade_screen(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let state = m.gw(GP_LOADING);
    let mode = lbu(m, state + 6);
    let mut level = if mode == 1 {
        lh(m, state + 2)
    } else {
        u32::from(mode == 2) << 12
    };
    let mut prim = dispatch::call(m, 0x8003_6824, [ot, prim, 0, 0]);
    if !slt(level, 0x1001) {
        level = 0x1000;
    }
    let rest = 0x1000u32.wrapping_sub(level);
    let state = m.gw(GP_LOADING);
    m.sb(state + 0x29, div_pow2(level.wrapping_mul(0xff), 12) as u8);
    let state = m.gw(GP_LOADING);
    m.sb(state + 0x2d, div_pow2(level.wrapping_mul(0x83), 12) as u8);
    let state = m.gw(GP_LOADING);
    if slt(m.lw(state + 0x38), 0xa00) {
        m.sw(state + 0xc, 0);
    }
    if !slt(level, 0x401) {
        let state = m.gw(GP_LOADING);
        let flags = m.lw(state + 0xc);
        m.sw(state + 0xc, flags | 0x80);
    }
    if !slt(level, 0x501) {
        let state = m.gw(GP_LOADING);
        let flags = m.lw(state + 0xc);
        m.sw(state + 0x14, rest.wrapping_mul(5) << 9);
        m.sw(state + 0xc, flags | 1);
    }
    if !slt(level, 0x601) {
        let state = m.gw(GP_LOADING);
        let flags = m.lw(state + 0xc);
        let drift = (rest << 15).wrapping_neg();
        m.sw(state + 0x10, drift);
        m.sw(state + 0xc, flags | 2);
        if slt(0, drift) {
            m.sw(state + 0x10, 0);
        }
    }
    if !slt(level, 0x701) {
        let scale = rest.wrapping_mul(181_170);
        let state = m.gw(GP_LOADING);
        let flags = m.lw(state + 0xc);
        m.sw(state + 0xc, flags | 4);
        m.sw(state + 0x34, div_pow2(scale, 12));
    }
    if !slt(level, 0x801) {
        let state = m.gw(GP_LOADING);
        let flags = m.lw(state + 0xc);
        m.sw(state + 0xc, flags | 0x40);
    }
    if !slt(level, 0x901) {
        let state = m.gw(GP_LOADING);
        let flags = m.lw(state + 0xc);
        m.sw(state + 0x18, level);
        m.sw(state + 0xc, flags | 0x18);
        m.sw(state + 0x1c, div_pow2(rest.wrapping_mul(199), 12));
        m.sw(state + 0x20, div_pow2(rest, 10));
        let fifth = sra(mul_hi(level, 0x6666_6667), 1).wrapping_sub(sra(level, 31));
        let remainder = level.wrapping_sub(fifth.wrapping_mul(5));
        m.sw(state + 0x24, remainder);
        let cap = m.lw(state + 0x20);
        if slt(cap, remainder) {
            m.sw(state + 0x24, cap);
        }
    }
    let context = m.gw(CONTEXT);
    if lbu(m, context + 0x39e) == 0 {
        let state = m.gw(GP_LOADING);
        prim = loading_tiles(m, state + 0xc, ot, prim);
    }
    let scaled = level << 12;
    let mut size = sra(mul_hi(scaled, 0x2aaa_aaab), 7).wrapping_sub(sra(scaled, 31));
    if !slt(size, 0x1001) {
        size = 0x1000;
    }
    let sine = sra(lhu(m, SINE + ((size & 0x3fff) << 1)) << 16, 18);
    let width = div_pow2(sine.wrapping_mul(0x500), 12);
    m.sh(frame + 0x1c, width as u16);
    if !slt(sext16(width), 0x141) {
        m.sh(frame + 0x1c, 0x140);
    }
    let height = div_pow2(sine.wrapping_mul(0xf0), 12);
    m.sh(frame + 0x1e, height as u16);
    let top = sra(0xf0u32.wrapping_sub(sext16(height)), 1);
    m.sh(frame + 0x1a, top as u16);
    let width = lh(m, frame + 0x1c);
    m.sh(frame + 0x18, sra(0x140u32.wrapping_sub(width), 1) as u16);
    prim = dispatch::call(m, 0x8008_47a8, [ot, prim, frame + 0x18, rest << 1]);
    let state = m.gw(GP_LOADING);
    let mut fade = lh(m, state) << 6;
    if !slt(fade, 0x1001) {
        fade = 0x1000;
    }
    let dark = 0x1000u32.wrapping_sub(fade);
    let color = (dark.wrapping_mul(0x83) >> 4) & 0xff00;
    m.sw(frame + 0x10, sra(color, 1) & 0xff00);
    let end = border_quads(m, ot, prim, fade, color);
    let state = m.gw(GP_LOADING);
    let frames = m.lw(state + 0x38).wrapping_add(1);
    m.sw(state + 0x38, frames);
    m.pop_frame(0x30);
    end
}

/// 80083FE0 (JP; PAL 80082998, near, `imm:2`) (ot, prim) -> next
/// primitive: the loading starfield: a dark 320x240 (PAL 320x256) backdrop
/// tile and 512 depth-cued one-pixel stars (fixed seed 0x2FD, the RNG state
/// restored afterwards) around the camera direction; stars below screen y
/// 0xF1 (PAL 0x101, unsigned) and x 0x141 are kept.
pub fn starfield(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x38);
    let saved = SCRATCH + 8;
    let r0 = m.gte.cfc2(ctrl::R11R12);
    let r1 = m.gte.cfc2(ctrl::R13R21);
    m.sw(saved, r0);
    m.sw(saved + 4, r1);
    let r2 = m.gte.cfc2(ctrl::R22R23);
    let r3 = m.gte.cfc2(ctrl::R31R32);
    let r4 = m.gte.cfc2(ctrl::R33);
    m.sw(saved + 8, r2);
    m.sw(saved + 0xc, r3);
    m.sw(saved + 0x10, r4);
    m.gte.mtc2(reg::VXY0, 0);
    m.gte.mtc2(reg::VZ0, 0x1c00);
    cop2(m, MVMVA_RT_V0_TR);
    let page = prim + 0x14;
    let mut group = prim + 0x1c;
    m.sw(prim, link(page) | 0x0400_0000);
    m.sw(prim + 4, 0xe100_0240);
    m.sw(prim + 8, 0x620f_1f1f);
    m.sw(prim + 0xc, 0);
    m.sw(prim + 0x10, 0x00f0_0140);
    m.sw(prim + 0x14, link(group) | 0x0100_0000);
    m.sw(page + 4, 0xe100_0220);
    let seed = sdk::rand(m);
    m.sw(SCRATCH + 0x28, seed);
    sdk::srand(m, 0x2fd);
    sdk::transpose_matrix(m, saved, saved);
    let mx = m.gte.mfc2(reg::MAC1).wrapping_neg();
    let my = m.gte.mfc2(reg::MAC2).wrapping_neg();
    let mz = m.gte.mfc2(reg::MAC3).wrapping_neg();
    m.sh(SCRATCH, (mx & 0x1fff).wrapping_sub(0x1000) as u16);
    m.sh(SCRATCH + 2, (my & 0x1fff).wrapping_sub(0x1000) as u16);
    m.sw(SCRATCH + 0x1c, mx);
    m.sw(SCRATCH + 0x20, my);
    m.sw(SCRATCH + 0x24, mz);
    m.sh(SCRATCH + 4, (mz & 0x1fff).wrapping_sub(0x1000) as u16);
    load_rotation(m, saved);
    m.gte.ctc2(ctrl::TRX, 0);
    m.gte.ctc2(ctrl::TRY, 0);
    m.gte.ctc2(ctrl::TRZ, 0x1c00);
    m.gte.ctc2(ctrl::RFC, 0x360);
    m.gte.ctc2(ctrl::GFC, 0x370);
    m.gte.ctc2(ctrl::BFC, 0x380);
    m.gte.mtc2(reg::RGBC, 0x6ac0_b0a0);
    let mut count = 0u32;
    for _ in 0..0x200 {
        lwc2(m, reg::VXY0, SCRATCH);
        lwc2(m, reg::VZ0, SCRATCH + 4);
        let base_x = m.lw(SCRATCH + 0x1c);
        let base_y = m.lw(SCRATCH + 0x20);
        cop2(m, RTPS);
        let random = sdk::rand(m);
        m.sh(
            SCRATCH,
            (base_x.wrapping_add(random) & 0x1fff).wrapping_sub(0x1000) as u16,
        );
        let random = sdk::rand(m);
        m.sh(
            SCRATCH + 2,
            (base_y.wrapping_add(random) & 0x1fff).wrapping_sub(0x1000) as u16,
        );
        let random = sdk::rand(m);
        let z = lhu(m, SCRATCH + 0x24).wrapping_add(random);
        m.sh(SCRATCH + 4, (z & 0x1fff).wrapping_sub(0x1000) as u16);
        let screen = m.gte.mfc2(reg::SXY2);
        let depth = m.gte.mfc2(reg::SZ3);
        if sra(screen, 16) >= 0xf1 || sext16(screen) >= 0x141 {
            continue;
        }
        let depth = abs(depth.wrapping_sub(0x1c00));
        if !slt(depth, 0x1000) {
            continue;
        }
        m.gte.mtc2(reg::IR0, depth);
        let star = group.wrapping_add(4 + (count << 3));
        cop2(m, DPCS);
        count += 1;
        m.sw(star + 4, screen);
        if count == 7 {
            let next = group + 0x3c;
            m.sw(group, link(next) | 0x0e00_0000);
            group = next;
            count = 0;
        }
        swc2(m, reg::RGB2, star);
    }
    let end = if count != 0 {
        let end = group.wrapping_add(count << 3).wrapping_add(4);
        m.sw(group, count << 25);
        end
    } else {
        let end = group;
        group = if page + 8 == end { page } else { end - 0x3c };
        end
    };
    insert_chain(m, ot, prim, group);
    let seed = m.lw(SCRATCH + 0x28);
    sdk::srand(m, seed);
    let _ = frame;
    m.pop_frame(0x38);
    end
}

/// 800364C4 (JP; PAL 8003617C, near, `imm:1`) (ot, prim) -> next primitive:
/// loading visual mode 2, the starfield seen from a slowly rolling camera
/// (context +0x20 timer), with the geometry offset (160, 120) (PAL (160,
/// 128)).
pub fn star_screen(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x58);
    let context = m.gw(CONTEXT);
    let timer = m.lw(context + 0x20);
    let angle = lhu(m, SINE + ((timer << 2) & 0x7ffc));
    m.sh(frame + 0x10, angle as u16);
    m.sh(frame + 0x12, angle as u16);
    m.sh(frame + 0x14, angle as u16);
    shared::rotation_yxz(m, frame + 0x10, frame + 0x18);
    load_rotation(m, frame + 0x18);
    let x = lhu(m, SINE + ((timer << 4) & 0x7ff0));
    let turn = timer << 4;
    let z = lhu(m, SINE + ((turn & 0x3fff) << 1));
    let y = sra(angle << 16, 17);
    m.sh(frame + 0x12, y as u16);
    let x = sra(x << 16, 17);
    let z = sra(z << 16, 17);
    m.sh(frame + 0x10, x as u16);
    m.sh(frame + 0x14, z as u16);
    m.gte.ctc2(ctrl::TRX, x);
    m.gte.ctc2(ctrl::TRY, y);
    m.gte.ctc2(ctrl::TRZ, z);
    let cosine = sdk::rcos(m, turn);
    m.gte.ctc2(ctrl::H, sra(cosine, 6).wrapping_add(0x12c));
    sdk::set_geom_offset(m, 0xa0, 0x78);
    let next = starfield(m, ot, prim);
    m.pop_frame(0x58);
    next
}

/// 8007961C (JP; PAL 80077FAC, near, `imm:1`) (flags): the starting camera
/// mode (0x400 for the scripted setups 0x6300, else 2 with context byte
/// +0x39F (PAL +0x3A3) or 1) on the player, then 80035740(1).
pub fn start_camera(m: &mut Machine, flags: u32) {
    let frame = m.push_frame(0x18);
    let _ = frame;
    let mode = if flags & 0x6300 != 0 {
        0x400
    } else {
        let context = m.gw(CONTEXT);
        if lbu(m, context + 0x39f) != 0 { 2 } else { 1 }
    };
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(ENTITIES);
    let camera = m.lw(gameplay + 0x28);
    let player = m.lw(table);
    dispatch::call(m, 0x8007_93c4, [camera, mode, player, 0]);
    dispatch::call(m, 0x8003_5740, [1, 0, 0, 0]);
    m.pop_frame(0x18);
}
