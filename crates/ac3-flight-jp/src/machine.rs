//! The machine of the PAL translation: the version-independent
//! [`ac3_machine::machine`] model (RAM, scratchpad, GTE, faults, host calls;
//! re-exported here whole) plus this executable's small-data base [`GP`]
//! and its accessors ([`SmallData`]: `m.gw(offset)`, `m.set_gw(offset,
//! value)`).
//!
//! A version crate generated from this one (the JP crate) keeps every
//! `use crate::machine::...` line and changes only [`GP`].
pub use ac3_machine::machine::*;

/// Resident `$gp` (PAL), fixed by the executable (small-data base).
pub const GP: u32 = 0x800b_cb74;

/// Small-data globals at `$gp + offset` ([`GP`]): the original's
/// `lw/sw offset($gp)`. A trait, so the shared [`Machine`] stays free of any
/// version's addresses while `GP` stays a compile-time constant.
pub trait SmallData {
    /// The word at `GP + offset`.
    fn gw(&mut self, offset: u32) -> u32;
    /// Store `value` at `GP + offset`.
    fn set_gw(&mut self, offset: u32, value: u32);
}

impl SmallData for Machine {
    #[inline]
    fn gw(&mut self, offset: u32) -> u32 {
        self.lw(GP.wrapping_add(offset))
    }
    #[inline]
    fn set_gw(&mut self, offset: u32, value: u32) {
        self.sw(GP.wrapping_add(offset), value);
    }
}
