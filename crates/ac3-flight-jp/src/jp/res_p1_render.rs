//! Patch of a `render` routine for resident part 1 (jp-res1): the lit
//! object systems 80033324, which the flight frame 800339E0 (`res_p1_frame`)
//! calls (REVIEW.md §3b). Listed in [`REPLACES`](super::REPLACES).
use crate::render::draw_trail_systems;
use crate::{
    common::GAMEPLAY,
    dispatch,
    machine::{Machine, SmallData},
    sdk,
};

/// 80085204 (JP; the counterpart of PAL 80083B78, which the map leaves
/// PAL-only; claimed by `res_p3_sky`): draw the object systems (ot,
/// primitives) -> the next primitive.
const OBJECT_SYSTEMS: u32 = 0x8008_5204;

/// 80033324 (JP; PAL 8003308C, exact but for the object-systems call: JP
/// calls 80085204, PAL 80083B78): load the view matrix, back colour and
/// colour matrix, then draw the trails (8007DB00) and the object systems
/// (80085204, by address), storing each new primitive cursor in gameplay
/// +0x14; returns the last one.
pub fn draw_lit_systems(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let gameplay = m.gw(GAMEPLAY);
    sdk::set_rot_matrix(m, gameplay + 0x4c);
    let gameplay = m.gw(GAMEPLAY);
    sdk::set_trans_matrix(m, gameplay + 0x4c);
    let gameplay = m.gw(GAMEPLAY);
    let (r, g, b) = (
        m.lw(gameplay + 0xac),
        m.lw(gameplay + 0xb0),
        m.lw(gameplay + 0xb4),
    );
    sdk::set_back_color(m, r, g, b);
    let gameplay = m.gw(GAMEPLAY);
    sdk::set_color_matrix(m, gameplay + 0x8c);
    let gameplay = m.gw(GAMEPLAY);
    let ot = m.lw(gameplay + 0x10) + 4;
    let primitives = m.lw(gameplay + 0x14);
    let next = draw_trail_systems(m, ot, primitives);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
    sdk::set_rot_matrix(m, gameplay + 0x4c);
    let gameplay = m.gw(GAMEPLAY);
    sdk::set_trans_matrix(m, gameplay + 0x4c);
    let gameplay = m.gw(GAMEPLAY);
    let ot = m.lw(gameplay + 0x10) + 4;
    let primitives = m.lw(gameplay + 0x14);
    let next = dispatch::call(m, OBJECT_SYSTEMS, [ot, primitives, 0, 0]);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
    m.pop_frame(0x18);
    next
}
