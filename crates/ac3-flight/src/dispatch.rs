//! Indirect calls (`jalr` through stored code addresses). Each module owns
//! `dispatch(m, target, args) -> Option<u32>` for the routines it translates;
//! an address nobody claims raises `Fault::Untranslated` and returns 0.
//!
//! Every original routine has one implementation, so exactly one module
//! claims each address (`tests/dispatch.rs` scans every `dispatch` and lists
//! the remaining duplicates, in a module still being extended:
//! 80026F5C, 8002A8AC, 8002A968, 8002A9E4 in `boot.rs`/`mainloop.rs`).
//!
//! Overlay modules all load at [`OVERLAY_BASE`](crate::common::OVERLAY_BASE),
//! so their addresses overlap: an overlay module (`ov<N>*.rs`) claims a
//! target only while `common::loaded_module` is its module number, and the
//! one-owner rule holds per overlay.
//!
//! With the development feature `dev-fallback` (never in the shipped
//! binary), a call nobody claims goes to the interpreter fallback
//! ([`crate::fallback`]), and `--interpret` can force translated routines
//! through it first.
use crate::machine::{Fault, Machine};

/// A module's claim on indirect-call targets.
pub type Owner = fn(&mut Machine, u32, [u32; 4]) -> Option<u32>;

/// Every module's claim, in the order they are asked. Only the pending
/// duplicates listed in the module docs depend on it: `boot.rs` answers the
/// CD resource routines (its 8002A9E4 translates CdPosToInt).
pub const OWNERS: [Owner; 36] = [
    crate::init::dispatch,
    crate::session::dispatch,
    crate::replay::dispatch,
    crate::frame::dispatch,
    crate::init_objects::dispatch,
    crate::init_tail::dispatch,
    crate::init_39a68::dispatch,
    crate::init_38818::dispatch,
    crate::script::dispatch,
    crate::shared::dispatch,
    crate::sdk::dispatch,
    crate::loading::dispatch,
    crate::boot::dispatch,
    crate::mainloop::dispatch,
    crate::cdstream::dispatch,
    crate::player::dispatch,
    crate::entities::dispatch,
    crate::render::dispatch,
    crate::terrain::dispatch,
    crate::hud::dispatch,
    crate::effects::dispatch,
    crate::ai::dispatch,
    crate::ov2_aircraft::dispatch,
    crate::ov2_hub::dispatch,
    crate::ov2_briefing::dispatch,
    crate::ov2_core::dispatch,
    crate::states::dispatch,
    crate::ov5::dispatch,
    crate::commands::dispatch,
    crate::ov1::dispatch,
    crate::ov3::dispatch,
    crate::ov4::dispatch,
    crate::arena::dispatch,
    crate::ov2_options::dispatch,
    crate::bios::dispatch,
    crate::ov2_misc::dispatch,
];

pub fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    #[cfg(feature = "dev-fallback")]
    if let Some(result) = crate::fallback::forced(m, target, args) {
        return result;
    }
    for owner in OWNERS {
        if let Some(result) = owner(m, target, args) {
            return result;
        }
    }
    #[cfg(feature = "dev-fallback")]
    if let Some(result) = crate::fallback::miss(m, target, args) {
        return result;
    }
    m.raise(Fault::Untranslated { address: target });
    0
}
