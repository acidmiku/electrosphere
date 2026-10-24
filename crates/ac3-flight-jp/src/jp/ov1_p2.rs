//! JP overlay 1 (module 1: title, accounts, saves, the disc prompt), part 2:
//! the routines `800D4D54`..`800DCAA4`, translated from the JP instructions
//! (`~/ac3-jp/analysis/jp_overlay_001`, loaded at
//! [`OVERLAY_BASE`](crate::common::OVERLAY_BASE)).
//!
//! The parts, each with its own `dispatch` (only while module 1 is loaded):
//! - `ov1_p2_save.rs` (`800D4D54`..`800D75B4`): the save block, account
//!   records, card events, the card probe and the card file head;
//! - `ov1_p2_text.rs` (`800D75E4`..`800D8AA0`): the text block, the glyph
//!   tables and the text drawing;
//! - `ov1_p2_menu.rs` (`800D8AF0`..`800DA8BC`): screen helpers;
//! - `ov1_p2_visual.rs` (`800DA8E0`..`800DCAA4`): the visual block and its
//!   draw-script interpreter.
//!
//! This file holds only the shared helpers and globals. Calls to routines
//! of the first half of the module (`800CC5D8`..`800D4808`, another part)
//! and to resident routines without a native function here go through
//! [`dispatch::call`] with their JP addresses.
use crate::{
    common::{CONTEXT, loaded_module},
    dispatch,
    machine::{Fault, Machine, SmallData},
};

/// The module number the part's dispatchers answer for.
pub const MODULE: u32 = 1;

/// `[0x800DDA48]`: the save block (0x360 bytes, `ov1_p2_save.rs`).
pub const SAVE: u32 = 0x800d_da48;
/// `[0x800DDA44]`: the card file head (0x280 bytes).
pub const HEAD: u32 = 0x800d_da44;
/// `[0x800DDA58]`: the text block (0x4C bytes, `ov1_p2_text.rs`).
pub const TEXT: u32 = 0x800d_da58;
/// `[0x800DD9DC]`: the visual block (0x58 bytes, `ov1_p2_visual.rs`).
pub const VISUAL: u32 = 0x800d_d9dc;

pub(crate) const fn off(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}
/// `lh`, sign-extended to a register value.
pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as u32
}
pub(crate) fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
/// `lb`, sign-extended to a register value.
pub(crate) fn lb(m: &mut Machine, address: u32) -> u32 {
    m.lb(address) as u32
}
pub(crate) fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
/// `sra`.
pub(crate) fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
/// `slt`.
pub(crate) fn slt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// The value of a halfword register (`sll 16; sra 16`).
pub(crate) fn sext16(value: u32) -> u32 {
    value as u16 as i16 as u32
}
/// An unaligned word load (`lwl`/`lwr` pair).
pub(crate) fn load_unaligned(m: &mut Machine, address: u32) -> u32 {
    let bytes = m.read_bytes(address, 4);
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
/// An unaligned word store (`swl`/`swr` pair).
pub(crate) fn store_unaligned(m: &mut Machine, address: u32, value: u32) {
    m.write_bytes(address, &value.to_le_bytes());
}
/// Signed `div` (quotient, remainder) with the compiler's `break 7` (zero
/// divisor, at `zero_break`) and `break 6` (`0x80000000 / -1`, at
/// `overflow_break`) checks: a reached trap records [`Fault::Trap`] and
/// yields the MIPS result.
pub(crate) fn div_trap(
    m: &mut Machine,
    numerator: u32,
    divisor: u32,
    zero_break: u32,
    overflow_break: u32,
) -> (u32, u32) {
    let (n, d) = (numerator as i32, divisor as i32);
    if d == 0 {
        m.raise(Fault::Trap {
            address: zero_break,
            code: 7,
        });
        return (if n < 0 { 1 } else { u32::MAX }, numerator);
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: overflow_break,
            code: 6,
        });
        return (numerator, 0);
    }
    (n.wrapping_div(d) as u32, n.wrapping_rem(d) as u32)
}
/// The low word of a `mult`.
pub(crate) fn mul(a: u32, b: u32) -> u32 {
    a.wrapping_mul(b)
}
/// `[gp+0x2EC]`: the game context.
pub(crate) fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}
/// A call to a routine translated elsewhere (or interpreted by the
/// development fallback), by its JP address.
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}
/// Whether module 1 is loaded (the gate of the part's dispatchers).
pub(crate) fn loaded(m: &mut Machine) -> bool {
    loaded_module(m) == MODULE
}

// ---- resident routines (JP addresses) -----------------------------------------------

/// 800ADEA8 strcpy(dst, src).
pub(crate) const STRCPY: u32 = 0x800a_dea8;
/// 800ADE38 strcmp(a, b).
pub(crate) const STRCMP: u32 = 0x800a_de38;
