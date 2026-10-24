//! Overlay 2's empty routines that no wave part owns: 800E11E4 and
//! 800E11EC, each `jr ra; nop`. The JP map makes them routines of their
//! own; 800E11E4 is the default entry of the handler table 0x8010AEC4 of
//! the display-list interpreter 800E22E4, whose caller ignores `v0`.
//! [`dispatch`] answers while module 2 is loaded.
use crate::common::loaded_module;
use crate::machine::Machine;

/// The module number (context +0x38 while it is loaded).
const MODULE: u32 = 2;

/// 800E11E4 / 800E11EC (overlay 2, JP only): `jr ra; nop`. Writes nothing
/// and leaves the caller's `v0`.
pub fn empty_handler(_m: &mut Machine) {}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let _ = args;
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800e_11e4 | 0x800e_11ec => {
            empty_handler(m);
            0
        }
        _ => return None,
    })
}
