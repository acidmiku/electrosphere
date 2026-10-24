//! Small resident helpers shared by several subsystems.
use crate::machine::{GP, Machine, SmallData};

/// `[gp+0x2DC]`: resident resource/transition block (PAL 0x800BE168).
pub const RESOURCES: u32 = 0x2dc;
/// `[gp+0x2E0]`: 700-byte gameplay block.
pub const GAMEPLAY: u32 = 0x2e0;
/// `[gp+0x2E8]`: 64-entry entity pointer table.
pub const ENTITIES: u32 = 0x2e8;
/// `[gp+0x2EC]`: game context (PAL 0x800BD158).
pub const CONTEXT: u32 = 0x2ec;

/// Load address shared by every overlay module (states 3, 4, 7, 8, 9 load
/// modules 2, 1, 3, 4, 5 there), so overlay code addresses overlap.
pub const OVERLAY_BASE: u32 = 0x800c_bfe8;

/// Context +0x38: the overlay module currently loaded (`0xFFFFFFFF` while a
/// resident state runs). The main loop's state dispatcher sets it when a
/// module load completes. An overlay module's `dispatch` claims its
/// addresses only while this is its module number.
///
/// Only host code asks this (dispatch gates, the fallback): no original
/// instruction reads the word when a routine is called. So it reads
/// without faults or load tracking, and a word outside aligned RAM is no
/// module. In a randomised test state with a bad context pointer, the
/// routine faults as the original does, not the gate in front of it
/// (found by `jp-res3`: the JP patch dispatchers, asked first, recorded a
/// misaligned fault before the claiming owner ran).
pub fn loaded_module(m: &mut Machine) -> u32 {
    let address = m.peek(GP.wrapping_add(CONTEXT)).wrapping_add(0x38);
    let ram = matches!(address >> 29, 0 | 4 | 5) && address & 0x1fff_ffff < 0x80_0000;
    if ram && address & 3 == 0 {
        m.peek(address)
    } else {
        u32::MAX
    }
}

/// 80032F18: request a resident transition. Resource +0x70/+0x74 receive the
/// two words and gameplay word 0 gains bit 0x20000000.
pub fn request_transition(m: &mut Machine, first: u32, second: u32) {
    let resources = m.gw(RESOURCES);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(resources + 0x70, first);
    m.sw(resources + 0x74, second);
    let flags = m.lw(gameplay);
    m.sw(gameplay, flags | 0x2000_0000);
}

/// 80051824: bytes `gp+0x47C/0x47D` = 0xFF, word `gp+0x480` = 0x100000.
pub fn reset_800501f8(m: &mut Machine) {
    m.sb(crate::GP + 0x47c, 0xff);
    m.sb(crate::GP + 0x47d, 0xff);
    m.set_gw(0x480, 0x10_0000);
}

/// 80030178: seven state bytes at `gp+0x32C..0x332`.
pub fn reset_8002ff34(m: &mut Machine) {
    for (offset, value) in [
        (0x32f, 0xff),
        (0x32e, 0xff),
        (0x32d, 0xff),
        (0x331, 0x54),
        (0x32c, 0),
        (0x330, 0),
        (0x332, 0x55),
    ] {
        m.sb(crate::GP + offset, value);
    }
}
