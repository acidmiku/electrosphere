//! Flight frame control: state callback 80033EF0, frame body 800339E0,
//! regular update 80032F3C and frame tail 80033DC0, and the other resident
//! state callbacks of table 800B0A20 (80034000 sequence, 80034040 replay,
//! 80034160 mission result, 8003423C, 80034274, 800342AC, 800342EC pause,
//! 800361BC/8003623C aircraft loading) with their overlays and helpers.
//!
//! Subsystems owned by other modules are called by original address through
//! `dispatch::call`, which every module answers for the routines it
//! translates; an unclaimed address faults explicitly.
use crate::{
    common::{CONTEXT, GAMEPLAY, RESOURCES, request_transition},
    dispatch,
    machine::{Machine, SmallData},
};

/// `[0x800BD098]`: flight state block read and updated by 80032F3C.
const FLIGHT_STATE_POINTER: u32 = 0x800b_d098;

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}

// JP-REVIEW(R0085): exact function, calls differ: PAL 80033C58 -> JP 80033EF0: instructions equal once addresses are masked, but only 4/5 calls go to the PAL callee's counterpart
/// 80033EF0: regular flight state callback (resident state 2, substate 7).
// JP-PATCH: replaced by `crate::jp::res_p1_frame::flight_callback` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn flight_callback(m: &mut Machine) -> u32 {
    crate::jp::res_p1_frame::flight_callback(m)
}

// JP-REVIEW(R0086): near function: PAL 80033748 -> JP 800339E0, imm:2, 2 instructions differ (whole routine)
/// 800339E0: one flight frame. `update` is the update routine (80032F3C or
/// a mode variant), called with a0 = gameplay, a1 = `[context+8]`,
/// a2 = context as the original leaves them.
// JP-PATCH: replaced by `crate::jp::res_p1_frame::frame` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn frame(m: &mut Machine, update: u32) {
    crate::jp::res_p1_frame::frame(m, update)
}

/// 80032F3C: regular flight update, unless a resident transition is pending.
pub fn update(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    if !m.lw(context) & 0x2000_0000 == 0 {
        return;
    }
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    for routine in [
        0x8007_29dc,
        0x8004_5898,
        0x8003_8a3c,
        0x8005_17f4,
        0x8004_658c,
        0x8006_c5b4,
        0x8007_dac8,
        0x8008_50d8,
    ] {
        call(m, routine, [0; 4]);
    }
    let frame = m.push_frame(0x30);
    let state = m.lw(FLIGHT_STATE_POINTER);
    for (offset, source) in [(0x10, 0x1c), (0x14, 8), (0x18, 0xc), (0x1c, 0x10)] {
        let value = m.lw(state + source);
        m.sw(frame + offset, value);
    }
    call(m, 0x8003_a23c, [frame + 0x10, 0, 0, 0]);
    let state = m.lw(FLIGHT_STATE_POINTER);
    let target = m.lw(frame + 0x24);
    if m.lw(state + 0x18) != target {
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay) & 2 != 0 {
            call(m, 0x8007_e890, [0; 4]);
        }
    }
    let value = m.lw(frame + 0x20);
    let state = m.lw(FLIGHT_STATE_POINTER);
    m.sw(camera + 0xbc, value);
    let target = m.lw(frame + 0x24);
    m.sw(state + 0x18, target);
    m.pop_frame(0x30);
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay) & 2 != 0 {
        call(m, 0x8007_f0f8, [0; 4]);
    }
}

// JP-REVIEW(R0087): near function: PAL 80033B28 -> JP 80033DC0, imm:2, 2 instructions differ (whole routine)
/// 80033DC0: per-frame tail. Steps 800371C4 unless replaying (context
/// +0x3A9), counts frames while no transition is pending, turns gameplay
/// request bits into resident transitions and applies a pending one through
/// 80032080. Returns the original v0 (0 or 80032080's result).
// JP-PATCH: replaced by `crate::jp::res_p1_frame::frame_tail` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn frame_tail(m: &mut Machine) -> u32 {
    crate::jp::res_p1_frame::frame_tail(m)
}

// ==== the other resident state callbacks (table 800B0A20) ==================================

/// `slt`: signed less-than.
fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// Clear gameplay word 0 bit 1 (the states other than 80033EF0 run
/// without it).
fn clear_flight_bit(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay) & !2;
    m.sw(gameplay, flags);
}
/// The OT and primitive cursor of a state's screen overlay: context +0xC,
/// gameplay +0x14.
fn overlay_cursors(m: &mut Machine) -> (u32, u32) {
    let context = m.gw(CONTEXT);
    let gameplay = m.gw(GAMEPLAY);
    (m.lw(context + 0xc), m.lw(gameplay + 0x14))
}

/// 80034000: state 4 (phase 0x10): the frame with update 8003307C.
pub fn sequence_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_307c);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

// JP-REVIEW(R0094): near function: PAL 80033DA8 -> JP 80034040, del:4 imm:6 ref:1, 11 instructions differ (whole routine)
/// 80034040: state 5 (phase 0x80, the replay): the frame with update
/// 800330CC and its overlay: with context +0x3A9 set, the fade bar
/// 80035578 (modes 1/2) and on the replay's end (gameplay bit 28) the
/// return to the menu (8003048C, 8002C6A8; result 0x60000000 | context
/// +0x3A4 when the replay was cut, else 0x50000000, without the frame
/// tail); otherwise the replay caption PAL 80034E84 unless context +0x3A2.
// JP-PATCH: replaced by `crate::jp::res_p1_frame::replay_state` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn replay_state(m: &mut Machine) -> u32 {
    crate::jp::res_p1_frame::replay_state(m)
}

/// 80034160: state 6 (phase 0x400, the mission result): the frame with
/// update 80033104; with gameplay +4 bit 0x1000 the result overlay
/// (8003514C, the blinking caption 800353E8, context bit 25) and on a
/// confirm (gameplay bit 5) the transition to the next resident (8002DDE0,
/// 8002937C, 80032F18(0x400, gameplay +8)) with the player reset 8006D078.
pub fn result_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_3104);
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 4) & 0x1000 != 0 {
        result_flags(m);
        let (ot, prim) = overlay_cursors(m);
        let prim = result_caption(m, ot, prim);
        let gameplay = m.gw(GAMEPLAY);
        let context = m.gw(CONTEXT);
        m.sw(gameplay + 0x14, prim);
        let flags = m.lw(context) | 0x0200_0000;
        m.sw(context, flags);
        let flags = m.lw(gameplay);
        if flags & 0x20 != 0 {
            m.sw(gameplay, flags & !0x20);
            call(m, 0x8002_dde0, [0; 4]);
            call(m, 0x8002_937c, [0; 4]);
            let gameplay = m.gw(GAMEPLAY);
            let next = m.lw(gameplay + 8);
            request_transition(m, 0x400, next);
            reset_player(m);
        }
    }
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 8003423C: state 7 (phase 0x4000): 800332A4, then the frame tail.
pub fn state_33fb4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    call(m, 0x8003_32a4, [0; 4]);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 80034274: state 8 (phase 0x20000): 800332C4, then the frame tail.
pub fn state_33fec(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    call(m, 0x8003_32c4, [0; 4]);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

/// 800342AC: state 9 (phase 0x100000): the frame with update 800331E0.
pub fn state_34024(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_flight_bit(m);
    frame(m, 0x8003_31e0);
    let v0 = frame_tail(m);
    m.pop_frame(0x18);
    v0
}

// JP-REVIEW(R0095): exact function, calls differ: PAL 80034064 -> JP 800342EC: instructions equal once addresses are masked, but only 3/4 calls go to the PAL callee's counterpart
/// 800342EC: state 10 (phase 0x200000, the pause menu): context bit 25,
/// the menu input 80035044, the frame with update 8003329C, and the menu
/// overlay PAL 80034BB0.
// JP-PATCH: replaced by `crate::jp::res_p1_frame::pause_state` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn pause_state(m: &mut Machine) -> u32 {
    crate::jp::res_p1_frame::pause_state(m)
}

// JP-REVIEW(R0096): near function: PAL 80035E78 -> JP 800361BC, imm:1, 1 instructions differ (whole routine)
/// 800361BC -> 0x4000000: state 11: start loading the aircraft file of
/// context +0x3AA (file index table 800B0BD0): resource +0x60 file, its
/// size (8002C63C) allocated (80027410), the read started (8002C82C with
/// completion 800357B4, loading state +8 = 1), then a poll (800357C0).
// JP-PATCH: replaced by `crate::jp::res_p1_frame::load_aircraft_state` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn load_aircraft_state(m: &mut Machine) -> u32 {
    crate::jp::res_p1_frame::load_aircraft_state(m)
}

/// 8003623C: state 13: poll (800357C0), install the loaded effect blocks
/// (8007DBC4 on the block pair the resource +0x60 file resolves to through
/// 80027338 twice), phase 0x80, and 80032080(0, 0).
pub fn install_effects_state(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, 0x8003_57c0, [0; 4]);
    let resources = m.gw(RESOURCES);
    let file = m.lw(resources + 0x60);
    let entry = call(m, 0x8002_7338, [file, 0, 0, 0]);
    let first = m.lw(entry);
    let pair = call(m, 0x8002_7338, [first, 0, 0, 0]);
    install_effect_blocks(m, pair);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 4, 0x80);
    m.sw(gameplay + 8, 0);
    let v0 = call(m, 0x8003_2080, [0, 0, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 80035044: the pause menu input: gameplay bit 6; context +0x130 bits 0/1
/// move the cursor (gameplay +0x29E, wrapped to 0..1); a confirm (context
/// +0x12C bit 4) sets gameplay bit 30 (continue) or 31 (quit).
pub fn pause_input(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay) | 0x40;
    m.sw(gameplay, flags);
    let cursor = gameplay + 0x29c + 2;
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x130) & 1 != 0 {
        let value = m.lhu(cursor).wrapping_sub(1);
        m.sh(cursor, value);
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x130) & 2 != 0 {
        let value = m.lhu(cursor).wrapping_add(1);
        m.sh(cursor, value);
    }
    if m.lh(cursor) < 0 {
        m.sh(cursor, 1);
    }
    if m.lh(cursor) >= 2 {
        m.sh(cursor, 0);
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0x12c) & 0x10 != 0 {
        let bit = match m.lh(cursor) {
            0 => 0x4000_0000,
            1 => 0x8000_0000,
            _ => return,
        };
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(gameplay) | bit;
        m.sw(gameplay, flags);
    }
}

/// 8003514C: the result screen flags: gameplay bit 6, and bit 5 (confirm)
/// with context +0x12C bit 6.
pub fn result_flags(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    let context = m.gw(CONTEXT);
    m.sw(gameplay, flags | 0x40);
    if m.lw(context + 0x12c) & 0x40 != 0 {
        m.sw(gameplay, flags | 0x60);
    }
}

// JP-REVIEW(R0098): changed function: PAL 80034DF4 -> JP 800353E8, call-:1 del:7 imm:4 op:5 reg:1, 17 instructions differ (whole routine)
/// 800353E8 (ot, prim) -> next primitive: the blinking result caption
/// (text 0xB at (0xA0, 0xD4), green by the frame counter's sine).
// JP-PATCH: replaced by `crate::jp::res_p1_frame::result_caption` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn result_caption(m: &mut Machine, ot: u32, prim: u32) -> u32 {
    crate::jp::res_p1_frame::result_caption(m, ot, prim)
}

// JP-REVIEW(R0100): near function: PAL 80035080 -> JP 80035578, imm:1 op:1, 2 instructions differ (whole routine)
/// 80035578 (ot, prim, level) -> next primitive: the replay fade bar: a
/// grey TILE (0, 0xE8, 0x140 x 2) of `level` (clamped 0..0x1000) linked at
/// `ot`, then the bar sprite 8003563C at (0xE0, 0xE0) of width `level / 128`
/// with the image 8002362C.
// JP-PATCH: replaced by `crate::jp::res_p1_frame::fade_bar` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn fade_bar(m: &mut Machine, ot: u32, prim: u32, level: u32) -> u32 {
    crate::jp::res_p1_frame::fade_bar(m, ot, prim, level)
}

// JP-REVIEW(R0101): near function: PAL 8006B8A8 -> JP 8006D078, imm:1, 1 instructions differ (whole routine)
/// 8006D078: reset the player for the next resident: flags 0x1C000, hit
/// points from its type (8006CD6C +0x84), 30 at difficulty 2 and above.
// JP-PATCH: replaced by `crate::jp::res_p1_frame::reset_player` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn reset_player(m: &mut Machine) {
    crate::jp::res_p1_frame::reset_player(m)
}

/// 8007DBC4 (pair): install an effect block: `[gp+0x544]` = block
/// `[pair]` with its data `[pair + 4]` at +4, the end of the data (+0x2050
/// bytes, word aligned) - 0x40 at +8, the offsets +0xC/+0x10 rebased onto
/// the data when not negative, +0x1018 cleared and +0x184C = -1.
pub fn install_effect_blocks(m: &mut Machine, pair: u32) {
    let block = m.lw(pair);
    let data = m.lw(pair + 4);
    let second = m.lw(block + 0x10);
    let size = m.lw(block + 0x2050);
    m.sw(block + 4, data);
    let base = m.lw(block + 4);
    m.set_gw(0x544, block);
    m.sw(block + 0x1018, 0);
    m.sw(block + 0x184c, u32::MAX);
    m.sw(
        block + 8,
        data.wrapping_add((size >> 2) << 2).wrapping_sub(0x40),
    );
    if !lt(second, 0) {
        m.sw(block + 0x10, base.wrapping_add(second));
    }
    let block = m.gw(0x544);
    let first = m.lw(block + 0xc);
    if !lt(first, 0) {
        m.sw(block + 0xc, base.wrapping_add(first));
    }
}

/// Native targets owned by this module for indirect calls (`jalr`).
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    match target {
        0x8003_3ef0 => Some(flight_callback(m)),
        0x8003_2f3c => {
            update(m);
            Some(0)
        }
        0x8003_3dc0 => Some(frame_tail(m)),
        0x8003_39e0 => {
            frame(m, a0);
            Some(0)
        }
        0x8003_4000 => Some(sequence_state(m)),
        0x8003_4040 => Some(replay_state(m)),
        0x8003_4160 => Some(result_state(m)),
        0x8003_423c => Some(state_33fb4(m)),
        0x8003_4274 => Some(state_33fec(m)),
        0x8003_42ac => Some(state_34024(m)),
        0x8003_42ec => Some(pause_state(m)),
        0x8003_61bc => Some(load_aircraft_state(m)),
        0x8003_623c => Some(install_effects_state(m)),
        0x8003_5044 => {
            pause_input(m);
            Some(0)
        }
        0x8003_514c => {
            result_flags(m);
            Some(0)
        }
        0x8003_53e8 => Some(result_caption(m, a0, a1)),
        0x8003_5578 => Some(fade_bar(m, a0, a1, a2)),
        0x8006_d078 => {
            reset_player(m);
            Some(0)
        }
        0x8007_dbc4 => {
            install_effect_blocks(m, a0);
            Some(0)
        }
        // common.rs translates 80032F18 without a claim; claimed here so
        // the states' callers can reach it by address.
        0x8003_2f18 => {
            request_transition(m, a0, a1);
            Some(0)
        }
        _ => None,
    }
}
