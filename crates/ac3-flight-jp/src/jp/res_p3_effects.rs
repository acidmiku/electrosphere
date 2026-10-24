//! Patch of the `effects` routine 800829BC whose JP instructions differ
//! from PAL (REVIEW.md §2), part 3 of the resident wave. Listed in
//! [`REPLACES`](super::REPLACES); the function keeps the generated name.
use crate::common::CONTEXT;
use crate::effects::{command_8007d720, fragment_80081198};
use crate::machine::{Machine, SmallData};

/// 800829BC (JP; PAL 80081374, near, `imm:1`): fragments command 6 with
/// byte 2 = `kind` (kind 1 becomes 0xA unless the mission halfword
/// context +0x38C, PAL +0x390, is 0x2A), then the fragment particle
/// 800827E0.
pub fn fragments_80081374(m: &mut Machine, position: u32, velocity: u32, kind: u32) {
    let mut kind = kind;
    if kind == 1 {
        let context = m.gw(CONTEXT);
        if m.lh(context.wrapping_add(0x38c)) != 0x2a {
            kind = 0xa;
        }
    }
    let command = command_8007d720(m, 6, position, velocity);
    m.sb(command.wrapping_add(2), kind as u8);
    fragment_80081198(m, position, velocity, kind);
}
