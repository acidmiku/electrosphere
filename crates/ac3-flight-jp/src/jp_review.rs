//! Faults for the sites the PAL -> JP maps do not cover. Generated with the
//! crate by `tools/jp/readdress.py`; every id is an entry of `REVIEW.md`,
//! and every site also carries a `// JP-REVIEW(Rn)` marker. No site keeps a
//! silent PAL address:
//!
//! * In code, [`unmapped`] panics when it runs: a todo-style fault.
//! * A const or a match pattern cannot panic at run time, so it holds a
//!   [`poison`] value instead: an address outside RAM and the scratchpad.
//!   Every access through it records `Fault::Address`, a dispatch to it
//!   `Fault::Untranslated`, and a pattern holding it never matches.
//! * The body of a PAL-only routine that something still calls is
//!   [`pal_only`].

/// Base of the poison values (`0xDEAD0000 | review number`).
pub const POISON_BASE: u32 = 0xdead_0000;

/// The poison value of review `number` (`R<number>` in REVIEW.md).
pub const fn poison(number: u32) -> u32 {
    POISON_BASE | number
}

/// A PAL address (or gp offset) with no JP counterpart, at review `id`.
#[track_caller]
pub fn unmapped(pal: u32, id: &'static str) -> u32 {
    panic!(
        "JP-REVIEW {id}: PAL value {pal:#010x} has no JP counterpart (see crates/ac3-flight-jp/REVIEW.md)"
    )
}

/// A PAL-only routine (no JP counterpart) that translated code still calls.
#[track_caller]
pub fn pal_only(what: &'static str) -> ! {
    panic!("JP-REVIEW: {what} is a PAL-only routine (see crates/ac3-flight-jp/REVIEW.md)")
}

/// The review number a poison value carries, if it is one.
pub fn poison_number(value: u32) -> Option<u32> {
    (value & 0xffff_0000 == POISON_BASE).then_some(value & 0xffff)
}
