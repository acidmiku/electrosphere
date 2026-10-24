//! JP overlay 1: title, accounts and saves (memory card), the disc-number prompt.
//!
//! Module 1 is BPB entry 1, loaded whole at [`OVERLAY_BASE`]
//! (0x800CBFE8) in state 4 (JP_PORT_PLAN §2.1). Translate its routines
//! from the JP instructions and claim them in [`dispatch`], which answers
//! only while module 1 is loaded (overlay addresses overlap between
//! modules).
use crate::common::{OVERLAY_BASE, loaded_module};
use crate::machine::Machine;

/// The module number (context +0x38 while it is loaded).
pub const MODULE: u32 = 1;
/// Load address (the end of the resident BSS).
pub const BASE: u32 = OVERLAY_BASE;
/// Size of the module file in bytes.
pub const BYTES: u32 = 72_340;
/// Arena start for this state (state table record +0xC).
pub const ARENA: u32 = 0x800d_da80;
/// The state's callback table (state table record +0).
pub const CALLBACKS: u32 = 0x800d_cb00;

/// The routines of this module by JP address, while it is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = (target, args);
    None
}
