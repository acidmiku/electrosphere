//! JP overlay 2, part 6 (routines 800FF95C..8010980C): the shared helpers
//! of the part and its dispatcher.
//!
//! The routines live in the sub-modules:
//! * [`map`](super::ov2_p6_map): the map and background block
//!   (`[0x80112DA8]`, the counterpart of PAL's `ov2_core_map`), 800FF95C ..
//!   801020BC;
//! * [`scene`](super::ov2_p6_scene): the 3D scene block (`[0x80112DAC]`) and
//!   its camera, 801020C0 .. 80102657;
//! * the later sub-modules, see [`dispatch`].
//!
//! Every routine is translated from the JP instructions (the PAL
//! counterparts, where the map has one, only as a guide). The dispatcher
//! answers only while module 2 is loaded (overlay addresses overlap between
//! modules) and claims only this part's addresses; calls into the other
//! overlay-2 parts and the resident go through [`dispatch::call`].
use crate::common::loaded_module;
use crate::dispatch;
use crate::machine::{Fault, Machine};

/// The overlay module of this part.
pub const MODULE: u32 = 2;

/// `[0x80112DA8]`: the map and background block (0x110 bytes, 801020BC).
pub const MAP: u32 = 0x8011_2da8;
/// `[0x80112DAC]`: the 3D scene block (0xA7C bytes, 801020C0).
pub const SCENE: u32 = 0x8011_2dac;
/// `[0x80112DC4]`: the hub block (+0x50 a frame counter, +0x130..+0x140
/// the background voices, +0x16C the current object).
pub const HUB: u32 = 0x8011_2dc4;
/// `[gp+0x2D4]`: the resident display request block (+0x10..+0x12 the
/// background RGB).
pub const DISPLAY: u32 = 0x2d4;

// ---- shared helpers -----------------------------------------------------------------------

/// Original-ABI call of a routine outside this part.
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}

/// Run `body` inside the original stack frame of `size` bytes (its base is
/// the routine's `sp`), so callee frames and passed stack addresses match.
pub(crate) fn framed<R>(
    m: &mut Machine,
    size: u32,
    body: impl FnOnce(&mut Machine, u32) -> R,
) -> R {
    let sp = m.push_frame(size);
    let result = body(m, sp);
    m.pop_frame(size);
    result
}

pub(crate) fn sext16(value: i16) -> u32 {
    value as i32 as u32
}

/// `lwc2 reg, address`.
pub(crate) fn lwc2(m: &mut Machine, reg: usize, address: u32) {
    let value = m.lw(address);
    m.gte.mtc2(reg, value);
}

/// `swc2 reg, address` (the `mfc2` read with its storage side effect).
pub(crate) fn swc2(m: &mut Machine, reg: usize, address: u32) {
    let value = m.gte.mfc2(reg);
    m.sw(address, value);
}

/// The inlined libgte SetRotMatrix/SetTransMatrix pair: control registers
/// 0..7 from the MATRIX at `matrix` (rotation words, then translation).
pub(crate) fn load_rot_trans(m: &mut Machine, matrix: u32) {
    for index in 0..8u32 {
        let value = m.lw(matrix + index * 4);
        m.gte.ctc2(index as usize, value);
    }
}

/// The compiler's signed division by a constant: high word of the product
/// with `magic`, arithmetic shift, minus the dividend's sign.
pub(crate) fn magic_quotient(value: i32, magic: u32, shift: u32) -> i32 {
    let high = ((i64::from(value) * i64::from(magic as i32)) >> 32) as i32;
    (high >> shift).wrapping_sub(value >> 31)
}

/// Truncated signed division by `1 << bits` (bias, `sra`).
pub(crate) fn divide_pow2(value: i32, bits: u32) -> i32 {
    let biased = if value < 0 {
        value.wrapping_add((1 << bits) - 1)
    } else {
        value
    };
    biased >> bits
}

/// Truncated signed remainder by `1 << bits`.
pub(crate) fn remainder_pow2(value: i32, bits: u32) -> i32 {
    value.wrapping_sub(divide_pow2(value, bits) << bits)
}

/// The compiled `div` with its traps (`div` at `pc`; `break 7` at `pc +
/// 0xC` for a zero divisor, `break 6` at `pc + 0x24` for -2^31 / -1): the
/// trap is raised as `Fault::Trap` (the native convention) and the
/// quotient is the hardware's.
pub(crate) fn div_trap(m: &mut Machine, numerator: u32, divisor: u32, pc: u32) -> u32 {
    let (n, d) = (numerator as i32, divisor as i32);
    if d == 0 {
        m.raise(Fault::Trap {
            address: pc + 0xc,
            code: 7,
        });
        return if n < 0 { 1 } else { u32::MAX };
    }
    if n == i32::MIN && d == -1 {
        m.raise(Fault::Trap {
            address: pc + 0x24,
            code: 6,
        });
        return numerator;
    }
    n.wrapping_div(d) as u32
}

/// `(value + (value >>> 31)) >> 1`: the compiled signed halving.
pub(crate) fn half(value: i32) -> i32 {
    value.wrapping_add(((value as u32) >> 31) as i32) >> 1
}

/// The masked-or `addPrim` idiom: `p[0] = (p[0] & 0xFF000000) | ([ot] &
/// 0xFFFFFF); [ot] = ([ot] & 0xFF000000) | (p & 0xFFFFFF)`. Returns the
/// entry's new word (the last value the original computes).
pub(crate) fn add_prim_masked(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    let tag = m.lw(prim);
    let link = m.lw(ot);
    m.sw(prim, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    let word = (link & 0xff00_0000) | (prim & 0x00ff_ffff);
    m.sw(ot, word);
    word
}

/// `lwl rt, address` (little-endian).
pub(crate) fn lwl(m: &mut Machine, rt: u32, address: u32) -> u32 {
    let shift = (address & 3) * 8;
    let word = m.lw(address & !3);
    (rt & 0x00ff_ffffu32.wrapping_shr(shift)) | word.wrapping_shl(24 - shift)
}

/// `swl rt, address` (little-endian).
pub(crate) fn swl(m: &mut Machine, rt: u32, address: u32) {
    let shift = (address & 3) * 8;
    let aligned = address & !3;
    let word = m.lw(aligned);
    let keep = if shift == 24 {
        0
    } else {
        0xffff_ff00u32 << shift
    };
    m.sw(aligned, (word & keep) | (rt >> (24 - shift)));
}

/// The libgpu `addPrim` idiom of compiled code (`lwl a, 2(ot); swl a,
/// 2(prim); swl prim<<8, 2(ot)`): the primitive takes over the ordering
/// table entry's link and the entry points at the primitive.
pub(crate) fn add_prim(m: &mut Machine, ot: u32, prim: u32) {
    let link = lwl(m, 0, ot.wrapping_add(2));
    swl(m, link, prim.wrapping_add(2));
    swl(m, prim << 8, ot.wrapping_add(2));
}

/// Link a primitive of `words` words into ordering-table bucket `bucket`
/// of `ot` (clamped at 255; negative buckets index below the table as
/// compiled): `p[0] = [entry] | words << 24; [entry] = p & 0xFFFFFF`.
pub(crate) fn link_bucket(m: &mut Machine, ot: u32, bucket: i32, prim: u32, words: u32) {
    let bucket = if bucket >= 0x100 { 0xff } else { bucket };
    let entry = ot.wrapping_add((bucket as u32) << 2);
    let link = m.lw(entry);
    m.sw(prim, link | (words << 24));
    m.sw(entry, (prim << 8) >> 8);
}

// ---- dispatch -----------------------------------------------------------------------------

/// This part's routines by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    use super::ov2_p6_map as map;
    use super::ov2_p6_models as models;
    use super::ov2_p6_scene as scene;
    use super::ov2_p6_story as story;
    use super::ov2_p6_terrain as terrain;
    use super::ov2_p6_viewer as viewer;
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800f_f95c => map::draw_rings(m, a0, a1, a2),
        0x8010_0304 => map::draw_tunnel(m, a0, a1, a2),
        0x8010_084c => map::draw_layer(m, a0, a1, a2, a3),
        0x8010_097c => map::stars_appear(m),
        0x8010_0994 => map::stars_disappear(m),
        0x8010_09ac => {
            map::scatter_stars(m, a0);
            0
        }
        0x8010_0b94 => map::draw_stars(m, a0, a1, a2),
        0x8010_0fac => map::ribbons_appear(m),
        0x8010_0fc4 => map::ribbons_disappear(m),
        0x8010_0fdc => {
            map::reset_trail(m, a0, a1);
            0
        }
        0x8010_1514 => map::ribbon_mode(m, a0, a1, a2),
        0x8010_1570 => map::reset_trails(m, a0),
        0x8010_16a4 => map::ribbon_camera(m, a0, a1),
        0x8010_17a8 => map::project_anchors(m, a0, a1, a2),
        0x8010_18d8 => map::ribbon_quads(m, a0, a1, a2, a3),
        0x8010_1a0c => map::ribbon_visible(m, a0),
        0x8010_1ad4 => map::draw_ribbons(m, a0, a1, a2),
        0x8010_1e4c => map::draw_ribbon_layer(m, a0, a1, a2),
        0x8010_1ee4 => map::initialize_map(m),
        0x8010_1fec => map::draw_map(m, a0),
        0x8010_20c0 => scene::allocate_scene(m),
        0x8010_20f4 => scene::reset_pan(m),
        0x8010_2120 => scene::halve_rows(m, a0),
        0x8010_2184 => scene::update_camera(m),
        0x8010_2564 => scene::set_colors(m),
        0x8010_2658 => scene::command_place(m, a0),
        0x8010_2aa8 => scene::command_move(m),
        0x8010_2bfc => scene::command_label(m),
        0x8010_2d1c => scene::command_gauge(m),
        0x8010_2e78 => scene::command_pan(m),
        0x8010_2f5c => scene::command_look(m),
        0x8010_2f9c => scene::command_marker(m),
        0x8010_2ff0 => scene::tick_objects(m),
        0x8010_30e0 => scene::step_objects(m),
        0x8010_3100 => scene::draw_objects(m),
        0x8010_3834 => scene::step_draw_objects(m),
        0x8010_3854 => scene::draw_labels(m),
        0x8010_417c => terrain::height_at(m, a0, a1, a2),
        0x8010_4218 => terrain::shade_quad(m, a0, a1, a2, a3),
        0x8010_48f0 => terrain::draw_terrain(m, a0),
        0x8010_5c20 => story::draw_marker(m, a0),
        0x8010_5d54 => story::tick_pan(m),
        0x8010_5e20 => story::run_script(m, a2, a3),
        0x8010_5f04 => story::allocate_objects(m),
        0x8010_5f78 => story::allocate_labels(m),
        0x8010_5fd8 => story::free_scene(m),
        0x8010_6000 => story::phase_open(m),
        0x8010_606c => story::phase_opening(m),
        0x8010_62b8 => story::phase_setup(m),
        0x8010_655c => story::phase_start(m),
        0x8010_65c8 => story::phase_script(m),
        0x8010_673c => story::phase_close(m),
        0x8010_6780 => story::phase_idle(m),
        0x8010_67b0 => story::load_scene(m),
        0x8010_6a34 => story::story_frame(m, a1, a2, a3),
        0x8010_71b8 => viewer::open_step(m),
        0x8010_7200 => viewer::fade_wait(m),
        0x8010_7270 => viewer::fade_step(m),
        0x8010_7358 => viewer::leave_hub(m),
        0x8010_73b0 => models::list_entities(m, a0),
        0x8010_7518 => models::build_lists(m, a0, a1, a2, a3),
        0x8010_817c => models::model_pass(m, a0, a1, a2, a3),
        0x8010_8298 => viewer::load_model(m),
        0x8010_8334 => viewer::finish_model(m),
        0x8010_8478 => viewer::upload_image(m, a0),
        0x8010_84d0 => viewer::turn_model(m),
        0x8010_85f8 => viewer::place_model(m, a0),
        0x8010_8874 => viewer::draw_viewer(m, a0),
        0x8010_8b7c => viewer::open_viewer(m),
        0x8010_8d90 => viewer::viewer_loading(m),
        0x8010_8e88 => viewer::viewer_select(m),
        0x8010_92bc => viewer::viewer_leave(m),
        0x8010_93e8 => viewer::viewer_scene(m),
        0x8010_9588 => viewer::viewer_idle(m),
        0x8010_9660 => viewer::viewer_back(m),
        _ => return None,
    })
}
