//! Patches of `init_39a68` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3). Each is listed in [`REPLACES`](super::REPLACES).
use crate::machine::{GP, Machine};

/// 800409AC (JP; PAL 800400F4, near, `imm:1`): halfwords `gp+0x358` = 0xA0,
/// `gp+0x35A` = 0x78 (PAL 0x80: the screen centre of the 240-line NTSC
/// display), `gp+0x356` = 0x40; bytes `gp+0x354/0x355` = 0. Reproduces the
/// JP capture `flight-002` #77 (`tests/captures.rs`).
pub fn reset_gp_344(m: &mut Machine) {
    m.sh(GP + 0x358, 0xa0);
    m.sh(GP + 0x35a, 0x78);
    m.sb(GP + 0x354, 0);
    m.sb(GP + 0x355, 0);
    m.sh(GP + 0x356, 0x40);
}
