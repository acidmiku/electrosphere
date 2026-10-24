//! Inner state 13, the "CONNECTION LOST" screen: the mission screens are
//! entered with it when the context's resume byte (+0x95B) is set (the
//! mission was abandoned). Initialization 800D3730 rewinds the current
//! mission (context +0x390) to the first record of its mission number;
//! the tick 800D3794 runs the hub loader's callbacks (the table at
//! 0x800F39F4, owned by `ov2core`) under the pulsing "CONNECTION LOST"
//! text and then leaves for the hub (state 0, or 4 without mission data).
//! Also here: the record lookup 800D1828 and the pulsing colour 800F17C0,
//! which state 7 shares.
//!
//! State `[0x800F5E8C]` (0x800F9440): +0 loader phase and +2 substate
//! (halfwords).
use super::{CONTEXT, MISSION, MISSION_RECORDS, NEXT_PRIMITIVE, call};
use crate::machine::{Machine, SmallData};

/// `[0x800F5E8C]`: the state's loader phase and substate.
pub const STATE: u32 = 0x800f_5e8c;
/// 800F39F4: per loader phase, the address of its callback list.
pub const LOADER_PHASES: u32 = 0x800f_39f4;
/// 800F3A1C: "CONNECTION LOST" per language (context +0x38C).
pub const TEXT: u32 = 0x800f_3a1c;
/// 800CB130: the pulse's bright colour (0, 0xE8, 0xE0) and pad byte.
pub const BRIGHT: u32 = 0x800c_b130;
/// `[0x800F9E14]`: the visual-script block (+0x98 + 4*slot: slot colours).
pub const VISUAL: u32 = 0x800f_9e14;
/// 800CEB80: loader page (`ov2core`); 800CEB90: bind the common scripts
/// (`ov2core`); 800D17D0: mission number of a mission id (`ov2air`);
/// 800D04C8: draw a string (`ov2core`); 800AC148: memset (SDK).
pub const LOADER_PAGE: u32 = 0x800c_eb80;
pub const BIND_SCRIPTS: u32 = 0x800c_eb90;
pub const MISSION_NUMBER: u32 = 0x800d_17d0;
pub const DRAW_STRING: u32 = 0x800d_04c8;
pub const MEMSET: u32 = 0x800a_c148;

fn sext16(value: u16) -> u32 {
    i32::from(value as i16) as u32
}

/// 800D1828: the mission id (+0) of the first of the 41 mission records
/// whose number (+2) is `number`; record 0's id when none is.
pub fn first_record_d1828(m: &mut Machine, number: u32) -> u32 {
    for index in 0..41u32 {
        let record = MISSION_RECORDS + index * 12;
        if u32::from(m.lbu(record + 2)) == number {
            return u32::from(m.lbu(record));
        }
    }
    u32::from(m.lbu(MISSION_RECORDS))
}

/// Signed high word of `a * 0x51EB851F` (the compiled /25 and /50).
fn mulhi(a: i32) -> i32 {
    super::mulhi(a, 0x51eb_851f)
}

/// 800F17C0: visual slot `slot`'s colour pulses between `from` and `to`
/// (RGB byte triples): with `f` = |frame counter mod 50 - 25| (context
/// +0x20), each channel is `from + (to - from) * f / 25`. Returns the
/// routine's v0: the sign word of the last channel's product.
pub fn pulse_f17c0(m: &mut Machine, slot: u32, from: u32, to: u32) -> u32 {
    let ctx = m.gw(CONTEXT);
    let counter = m.lw(ctx + 0x20) as i32;
    let start = u32::from(m.lbu(from));
    let quotient = (mulhi(counter) >> 4).wrapping_sub(counter >> 31);
    let distance = counter
        .wrapping_sub(quotient.wrapping_mul(50))
        .wrapping_sub(0x19);
    let target = u32::from(m.lbu(to));
    let factor = distance.wrapping_abs();
    let channel = |m: &mut Machine, index: u32, start: u32, target: u32| {
        let product = (target.wrapping_sub(start) as i32).wrapping_mul(factor);
        let step = (mulhi(product) >> 3).wrapping_sub(product >> 31);
        let block = m.lw(VISUAL);
        let at = block.wrapping_add(slot << 2);
        m.sb(at + 0x98 + index, start.wrapping_add(step as u32) as u8);
        (product >> 31) as u32
    };
    channel(m, 0, start, target);
    let target = u32::from(m.lbu(to + 1));
    let start = u32::from(m.lbu(from + 1));
    channel(m, 1, start, target);
    let target = u32::from(m.lbu(to + 2));
    let start = u32::from(m.lbu(from + 2));
    channel(m, 2, start, target)
}

/// 800D3730: initialization: loader phase and substate 0, loader page 4
/// (800CEB80), and the current mission (context +0x390) rewound to the
/// first record of its mission number (800D17D0, 800D1828). Returns 1.
pub fn init_d3730(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = m.lw(STATE);
    m.sh(s + 2, 0);
    m.sh(s, 0);
    call(m, LOADER_PAGE, [4, 0, 0, 0]);
    let ctx = m.gw(CONTEXT);
    let current = sext16(m.lhu(ctx + 0x390));
    let number = call(m, MISSION_NUMBER, [current, 0, 0, 0]);
    let first = first_record_d1828(m, number);
    let ctx = m.gw(CONTEXT);
    m.sh(ctx + 0x390, first as u16);
    m.pop_frame(0x18);
    1
}

/// 800D3794: the tick: draw "CONNECTION LOST" (font 0x13 at (200, 150),
/// colour 0x808080), run the loader callback of the current phase and
/// substate (result 1 next substate, 2 next phase, 4 bind the common
/// scripts and leave the state with 0x100), then pulse visual slot 0x78
/// between (0, 0xE8, 0xE0) and black. Returns 0 while loading.
///
/// The loader callbacks read no argument register; the translation passes
/// the state address in `a0` (as the original) and zeros for `a1..a3`.
pub fn tick_d3794(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x40);
    let ctx = m.gw(CONTEXT);
    let block = m.lw(MISSION);
    let language = m.lw(ctx + 0x38c);
    m.sw(frame + 0x10, 0xc8);
    m.sw(frame + 0x14, 0x96);
    m.sw(frame + 0x18, 0x0080_8080);
    let ot = m.lw(block + 0x90);
    let prim = m.lw(block.wrapping_add(NEXT_PRIMITIVE));
    let string = m.lw(TEXT.wrapping_add(language << 2));
    let next = call(m, DRAW_STRING, [0x13, string, ot, prim]);
    let block = m.lw(MISSION);
    m.sw(block.wrapping_add(NEXT_PRIMITIVE), next);
    let s = m.lw(STATE);
    let phase = sext16(m.lhu(s));
    let list = m.lw(LOADER_PHASES.wrapping_add(phase << 2));
    let substate = sext16(m.lhu(s + 2));
    let callback = m.lw(list.wrapping_add(substate << 2));
    let result = call(m, callback, [s, 0, 0, 0]);
    if result & 1 != 0 {
        let s = m.lw(STATE);
        let value = m.lhu(s + 2).wrapping_add(1);
        m.sh(s + 2, value);
    } else if result & 2 != 0 {
        let s = m.lw(STATE);
        let value = m.lhu(s);
        m.sh(s + 2, 0);
        m.sh(s, value.wrapping_add(1));
    } else if result & 4 != 0 {
        call(m, BIND_SCRIPTS, [0; 4]);
        m.pop_frame(0x40);
        return 0x100;
    }
    // The 4-byte colour is copied with lwl/lwr (no alignment needed).
    for i in 0..4 {
        let byte = m.lbu(BRIGHT + i);
        m.sb(frame + 0x20 + i, byte);
    }
    call(m, MEMSET, [frame + 0x28, 0, 4, 0]);
    pulse_f17c0(m, 0x78, frame + 0x20, frame + 0x28);
    m.pop_frame(0x40);
    0
}

/// Claims of this part by original address, asked by `ov2_hub::dispatch`
/// (nested so the crate-wide claim scan sees them).
pub mod claims {
    use super::*;
    use crate::{common::loaded_module, ov2_hub::MODULE};

    /// The claims, only while module 2 is loaded.
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        if loaded_module(m) != MODULE {
            return None;
        }
        let [a0, a1, a2, _] = args;
        Some(match target {
            0x800d_1828 => first_record_d1828(m, a0),
            0x800d_3730 => init_d3730(m),
            0x800d_3794 => tick_d3794(m),
            0x800f_17c0 => pulse_f17c0(m, a0, a1, a2),
            _ => return None,
        })
    }
}
