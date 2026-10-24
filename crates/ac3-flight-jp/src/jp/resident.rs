//! JP-only resident routines (class `new` in `jp_pal_map.tsv`, REVIEW.md §6),
//! translated from the JP instructions. Claim each in [`dispatch`] by its JP
//! address; the patch layer's dispatchers are asked before the generated
//! ones.
//!
//! Example (REVIEW.md §6): `Krom2RawAdd` (JP 800AF438, BIOS B0h:51h) is
//! statically unreachable; a stub would be the arm
//! `0x800a_f438 => Some(u32::MAX)`.
use crate::machine::Machine;

/// The routines of this module by JP address (`None`: not claimed).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let _ = (m, target, args);
    None
}
