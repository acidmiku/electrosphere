//! JP overlay 2, part 4 (800EB944..800F5834): helpers shared by the part's
//! modules. The routines themselves are in the `ov2_p4_*` modules, each with
//! its own [`dispatch`](super::ov2_p4_text::dispatch) that answers only while
//! module 2 is loaded:
//!
//! * [`super::ov2_p4_disc`]: 800EB944..800EC33F, the disc-check screen
//!   and a debug TIM dump;
//! * [`super::ov2_p4_stream`]: 800EC340..800ECD1F, the broadcast picture
//!   stream (block `[0x80112DFC]`);
//! * [`super::ov2_p4_load`]: 800ECD20..800ED15B, resource upload helpers;
//! * [`super::ov2_p4_story`]: 800ED15C..800F00A7, the story-event block
//!   `[0x80112DB0]`: its picture script, the broadcast, the selection list,
//!   the hub's wireframe and the picture slots;
//! * [`super::ov2_p4_scene`]: 800F00A8..800F33A7, the story-event phases
//!   and the disc-swap phases;
//! * [`super::ov2_p4_text`]: 800F33A8..800F4C03, the bitmap-font renderer
//!   (block `[0x80112E00]`);
//! * [`super::ov2_p4_graph`]: 800F4C04..800F5833, the debriefing's replay
//!   graph (blocks `[0x80112E04]`, `[0x80112E08]`).
use crate::dispatch;
use crate::machine::{Fault, Machine};

/// `dispatch::call` (calls into other parts and the resident).
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}

/// Signed `div n, d` behind the compiler's checks: `break 7` at `zero` for
/// a zero divisor and `break 6` at `overflow` for `0x80000000 / -1`. A
/// reached trap records [`Fault::Trap`]. Returns (LO, HI) as the CPU leaves
/// them.
pub(crate) fn div(m: &mut Machine, n: u32, d: u32, zero: u32, overflow: u32) -> (u32, u32) {
    let (a, b) = (n as i32, d as i32);
    if b == 0 {
        m.raise(Fault::Trap {
            address: zero,
            code: 7,
        });
        return (if a < 0 { 1 } else { u32::MAX }, n);
    }
    if b == -1 && a == i32::MIN {
        m.raise(Fault::Trap {
            address: overflow,
            code: 6,
        });
        return (n, 0);
    }
    (a.wrapping_div(b) as u32, a.wrapping_rem(b) as u32)
}

/// Sign-extend a halfword load (`lh`).
pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}

/// `(x << 16) >> 16` (arithmetic): the low halfword sign-extended.
pub(crate) fn sext16(x: u32) -> u32 {
    x as u16 as i16 as i32 as u32
}

/// Signed `slt`.
pub(crate) fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}

/// `sra`.
pub(crate) fn sra(x: u32, n: u32) -> u32 {
    ((x as i32) >> n) as u32
}

/// `mult x, k; mfhi` (signed).
pub(crate) fn mulhi(x: u32, k: u32) -> u32 {
    ((i64::from(x as i32) * i64::from(k as i32)) >> 32) as u32
}

/// A compiled signed division by a constant: `hi(x * k)` (plus `x` when
/// `add`), arithmetic shift, minus the sign of `x`.
pub(crate) fn magic_div(x: u32, k: u32, add: bool, shift: u32) -> u32 {
    let hi = mulhi(x, k);
    let hi = if add { hi.wrapping_add(x) } else { hi };
    sra(hi, shift).wrapping_sub(sra(x, 31))
}

/// Wrapping address arithmetic (`addiu`/`addu` on a pointer read from RAM:
/// a -1 sentinel plus an offset is a valid low address in the original).
pub(crate) trait At {
    fn at(self, offset: u32) -> u32;
}
impl At for u32 {
    fn at(self, offset: u32) -> u32 {
        self.wrapping_add(offset)
    }
}

/// `lwl rt, address`: the bytes from the aligned word up to `address`
/// into the high bytes of `rt` (little-endian).
pub(crate) fn lwl(m: &mut Machine, address: u32, rt: u32) -> u32 {
    const MASK: [u32; 4] = [0x00ff_ffff, 0x0000_ffff, 0x0000_00ff, 0];
    const SHIFT: [u32; 4] = [24, 16, 8, 0];
    let shift = (address & 3) as usize;
    let word = m.lw(address & !3);
    (rt & MASK[shift]) | (word << SHIFT[shift])
}

/// `lwr rt, address`: the bytes from `address` to the end of its aligned
/// word into the low bytes of `rt`.
pub(crate) fn lwr(m: &mut Machine, address: u32, rt: u32) -> u32 {
    const MASK: [u32; 4] = [0, 0xff00_0000, 0xffff_0000, 0xffff_ff00];
    const SHIFT: [u32; 4] = [0, 8, 16, 24];
    let shift = (address & 3) as usize;
    let word = m.lw(address & !3);
    (rt & MASK[shift]) | (word >> SHIFT[shift])
}

/// `swl rt, address`: the high bytes of `rt` into the aligned word up to
/// `address`.
pub(crate) fn swl(m: &mut Machine, address: u32, rt: u32) {
    const MASK: [u32; 4] = [0xffff_ff00, 0xffff_0000, 0xff00_0000, 0];
    const SHIFT: [u32; 4] = [24, 16, 8, 0];
    let shift = (address & 3) as usize;
    let aligned = address & !3;
    let word = m.lw(aligned);
    m.sw(aligned, (word & MASK[shift]) | (rt >> SHIFT[shift]));
}

/// `swr rt, address`: the low bytes of `rt` from `address` to the end of
/// its aligned word.
pub(crate) fn swr(m: &mut Machine, address: u32, rt: u32) {
    const MASK: [u32; 4] = [0, 0x0000_00ff, 0x0000_ffff, 0x00ff_ffff];
    const SHIFT: [u32; 4] = [0, 8, 16, 24];
    let shift = (address & 3) as usize;
    let aligned = address & !3;
    let word = m.lw(aligned);
    m.sw(aligned, (word & MASK[shift]) | (rt << SHIFT[shift]));
}

/// The compiler's unaligned 8-byte copy (`lwl/lwr` two words from `from`,
/// then `swl/swr` them to `to`).
pub(crate) fn copy8(m: &mut Machine, from: u32, to: u32) {
    let first = lwl(m, from.at(3), 0);
    let first = lwr(m, from, first);
    let second = lwl(m, from.at(7), 0);
    let second = lwr(m, from.at(4), second);
    swl(m, to.at(3), first);
    swr(m, to, first);
    swl(m, to.at(7), second);
    swr(m, to.at(4), second);
}
