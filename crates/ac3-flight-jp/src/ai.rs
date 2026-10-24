//! Entity AI, missile behaviours, trails and effect spawners reached from the
//! world/entity updates (`entities.rs`): the movement states (table
//! 80023E58) and action states (table 80023EB8) the AI pass runs per slot,
//! behaviour selection (80050720 family), the missile behaviour routines
//! (tables 800B2C80/800B2C94/800B2CA8), the trail point writers and the
//! effect spawners they call; also the effect handler tables of
//! `effects.rs` (sparks 8002479C, trails 80024804, destruction effects
//! 80024874 with their timer events, water impacts of 80080EB4) and the
//! unreferenced text formatting helpers 80027C24..80027E80.
//!
//! Conventions as in `entities.rs`: every routine with an original stack
//! frame allocates that frame on the guest stack (`push_frame`), and routines
//! owned elsewhere are reached by original address through
//! [`dispatch::call`].
use crate::{
    common, dispatch,
    machine::{Machine, SmallData},
    sdk, shared,
};
use ac3_gte::{ctrl, reg};

// ---- globals ------------------------------------------------------------------

/// 800C6FD0: per-slot status words (32 slots).
const SLOT_FLAGS: u32 = 0x800c_6fd0;
/// 800C6ED0: per-slot object (type description) pointers.
const SLOT_OBJECTS: u32 = 0x800c_6ed0;
/// 800BD080 = `gp+0x50C`: the shots block pointer.
const SHOTS_POINTER: u32 = 0x800b_d080;
/// 800104AC: 1.14 sine table (16384 entries per turn plus a quarter).
const SINE: u32 = 0x8001_04ac;
/// `[gp+0x34C]`: per-slot AI records (0xB8 bytes).
const GP_SLOT_RECORDS: u32 = 0x34c;
/// `[gp+0x384]`: per-slot info (0x40 bytes: +0x14 type parameters,
/// +0x18 pilot block).
const GP_SLOT_INFO: u32 = 0x384;
/// `[gp+0x53C]`: trail queues.
const GP_TRAILS: u32 = 0x53c;
/// Scratchpad base.
const SCRATCH: u32 = 0x1f80_0000;

// ---- helpers ------------------------------------------------------------------

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> (shift & 31)) as u32
}
/// `slt`: signed less-than.
fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
fn abs(value: u32) -> u32 {
    if (value as i32) < 0 {
        value.wrapping_neg()
    } else {
        value
    }
}
/// `mult` + `mflo`.
fn lo(a: u32, b: u32) -> u32 {
    (a as i32).wrapping_mul(b as i32) as u32
}
/// `mult` + `mfhi`.
fn hi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}
/// `div` quotient (FLIGHT_MACHINE.md rule 2; the original's `break` traps
/// for a zero divisor and `-0x80000000 / -1` are not modelled).
fn div(dividend: u32, divisor: u32) -> u32 {
    let (a, b) = (dividend as i32, divisor as i32);
    if b == 0 {
        return if a < 0 { 1 } else { u32::MAX };
    }
    a.wrapping_div(b) as u32
}
/// Sign-extend a halfword held in a register.
fn sext16(value: u32) -> u32 {
    value as u16 as i16 as i32 as u32
}
fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}
fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
fn bit(index: u32) -> u32 {
    1u32 << (index & 31)
}
/// Entity pointer `slot` of the 64-entry table `[gp+0x2E8]`.
fn entity(m: &mut Machine, slot: u32) -> u32 {
    let table = m.gw(common::ENTITIES);
    m.lw(table.wrapping_add(slot << 2))
}
/// The per-slot AI record of `slot` (`[gp+0x34C] + slot * 0xB8`).
fn slot_record(m: &mut Machine, slot: u32) -> u32 {
    m.gw(GP_SLOT_RECORDS).wrapping_add(slot.wrapping_mul(0xb8))
}
/// The per-slot info block of `slot` (`[gp+0x384] + slot * 0x40`).
fn slot_info(m: &mut Machine, slot: u32) -> u32 {
    (slot << 6).wrapping_add(m.gw(GP_SLOT_INFO))
}
/// A routine owned elsewhere, by original address.
fn original(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}
/// Run `body` inside an original stack frame of `size` bytes.
fn with_frame<T>(m: &mut Machine, size: u32, body: impl FnOnce(&mut Machine, u32) -> T) -> T {
    let frame = m.push_frame(size);
    let result = body(m, frame);
    m.pop_frame(size);
    result
}
fn set_word_bits(m: &mut Machine, address: u32, bits: u32) {
    let value = m.lw(address) | bits;
    m.sw(address, value);
}
fn clear_word_bits(m: &mut Machine, address: u32, bits: u32) {
    let value = m.lw(address) & !bits;
    m.sw(address, value);
}
/// Copy `count` words (load all, then store all).
fn copy_block(m: &mut Machine, target: u32, source: u32, count: u32) {
    let words: Vec<u32> = (0..count).map(|i| m.lw(source + i * 4)).collect();
    for (i, word) in words.into_iter().enumerate() {
        m.sw(target + i as u32 * 4, word);
    }
}
/// The sine-table entry for a 14-bit angle index already scaled to bytes.
fn sine_at(m: &mut Machine, index: u32) -> u32 {
    lh(m, SINE.wrapping_add(index << 1))
}
/// The shots block (`[gp+0x50C]`).
fn shots(m: &mut Machine) -> u32 {
    m.lw(SHOTS_POINTER)
}
// ---- routines owned elsewhere ---------------------------------------------------

/// 8007ECE8 (`effects`): a point effect command of `kind`; returns it.
fn point_command(m: &mut Machine, kind: u32, point: u32) -> u32 {
    original(m, 0x8007_ece8, [kind, point, 0, 0])
}
/// 80081C30 (`effects`): a smoke puff of `kind` on the frames `seed` selects.
fn smoke(m: &mut Machine, position: u32, velocity: u32, seed: u32, kind: u32) {
    original(m, 0x8008_1c30, [position, velocity, seed, kind]);
}
/// 8002F3C0 (`player`): sound `sound` positioned at `position`.
fn sound_at(m: &mut Machine, sound: u32, position: u32, shift: u32) -> u32 {
    original(m, 0x8002_f3c0, [sound, position, shift, 0])
}
/// 80038320 (`player`): mark `slot` destroyed.
fn mark_destroyed(m: &mut Machine, slot: u32) {
    original(m, 0x8003_8320, [slot, 0, 0, 0]);
}
/// 800451AC (`player`): `out` = `origin` + `offset` rotated by the origin's
/// angles.
fn offset_point(m: &mut Machine, origin: u32, offset: u32, out: u32) -> u32 {
    original(m, 0x8004_51ac, [origin, offset, out, 0])
}
// JP-REVIEW(R0002): near function: PAL 800454EC -> JP 80046124, del:3 imm:15 ins:4, 22 instructions differ (whole routine)
/// 80046124 (`player`): pick and lock a target for `slot`.
fn pick_target(m: &mut Machine, slot: u32) -> u32 {
    original(m, 0x8004_6124, [slot, 0, 0, 0])
}

// ==== movement states (table 80023E58) ==========================================

/// Movement states 2 and 3: destination = target (+0x3F) position plus
/// the formation offset (+0x44) rotated by the target's angles.
fn follow_offset(m: &mut Machine, slot: u32, record: u32) {
    let target_slot = lbu(m, record + 0x3f);
    let target = entity(m, target_slot);
    if lbu(m, record + 0x3e) == 0 {
        m.sb(record + 0x3e, 1);
    }
    offset_point(m, target + 8, record + 0x44, record + 0x68);
    crate::entities::clamp_destination_4efbc(m, slot, record + 0x68);
}

/// 8004EE68: movement state 2, formation: follow the offset point; close
/// in (+0x88 < 0x2000) sets slot flag 0x20 and action 0, else action 4;
/// speed mode 7 when flagged 0x40 or following a non-player, else 4.
pub fn formation_4d860(m: &mut Machine, slot: u32) {
    with_frame(m, 0x28, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        follow_offset(m, slot, record);
        if lt(m.lw(record + 0x88), 0x2000) {
            set_word_bits(m, SLOT_FLAGS + (slot << 2), 0x20);
            m.sb(record + 0x63, 0);
        } else {
            m.sb(record + 0x63, 4);
        }
        let mode = if m.lw(mover) & 0x40 != 0 || lbu(m, record + 0x3f) != 0 {
            7
        } else {
            4
        };
        m.sb(record + 0x61, mode);
    });
}

/// 8004EF80: movement state 3, wingman: follow the offset point; action 7
/// close in, else 4; speed mode 5.
pub fn wingman_4d978(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        follow_offset(m, slot, record);
        let action = if lt(m.lw(record + 0x88), 0x2000) {
            7
        } else {
            4
        };
        m.sb(record + 0x63, action);
        m.sb(record + 0x61, 5);
    });
}

// JP-REVIEW(R0004): near function: PAL 8004DAC0 -> JP 8004F0C8, del:1 imm:5 ins:2 op:2, 10 instructions differ (whole routine)
/// 8004F0C8: movement state 5, attack run on the player: phase 0 sets the
/// run timer (+0x40), 1 flies at the player's predicted position (with a
/// circling offset from difficulty 2), 2 picks the break-off manoeuvre,
/// 3 ends on record flag 0x400000.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::attack_run_4dac0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn attack_run_4dac0(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::attack_run_4dac0(m, slot)
}

/// `rand() % 3` as compiled (magic 0x55555556).
fn rand_mod3(m: &mut Machine) -> u32 {
    let value = sdk::rand(m);
    let third = hi(value, 0x5555_5556).wrapping_sub(sra(value, 31));
    value.wrapping_sub(third.wrapping_mul(3))
}

// JP-REVIEW(R0006): near function: PAL 8004DF64 -> JP 8004F570, del:1 imm:9 ins:7 op:6, 23 instructions differ (whole routine)
/// 8004F570: movement state 7, missile evasion: on entry the evasion time
/// (+0x40) from the pilot block; while a missile tracks the slot, repeated
/// break turns (tables 800B2028/800B2038 by difficulty) with contrails;
/// state 0x10 waits out a decoyed missile, 7 dives to the player.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::evade_4df64` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn evade_4df64(m: &mut Machine, slot: u32) -> u32 {
    crate::jp::res_p2_ai::evade_4df64(m, slot)
}

/// 8004FBA4: movement state 11: action 0x1F on entry; record flag
/// 0x400000 ends the state.
pub fn state_11_4e580(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    if lbu(m, record + 0x3e) == 0 {
        m.sb(record + 0x3e, 1);
        m.sb(record + 0x63, 0x1f);
        return;
    }
    if m.lw(record) & 0x40_0000 != 0 {
        m.sb(record + 0x3d, 0xff);
    }
}

// JP-REVIEW(R0007): near function: PAL 8004E5E0 -> JP 8004FC04, del:3 imm:1 op:6, 10 instructions differ (whole routine)
/// 8004FC04: movement state 12, destroyed: pick the crash action (0x2B for
/// kinds 1/3, 0x2A for entity kinds 0x14/0x15, 0x28 for 0x3E, else
/// 0x28 + rand % 3); speed mode 9.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::destroyed_4e5e0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn destroyed_4e5e0(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::destroyed_4e5e0(m, slot)
}

/// 8004FCF0: movement state 14: action 0x19 on entry; speed mode 9.
pub fn state_14_4e6d8(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    if lbu(m, record + 0x3e) == 0 {
        m.sb(record + 0x3e, 1);
        m.sb(record + 0x63, 0x19);
    }
    m.sb(record + 0x61, 9);
}

/// 8004FD38: movement state 15: on entry drop the entity by 0x1000000 and
/// start action 0x1A; speed mode 9.
pub fn state_15_4e720(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    let mover = entity(m, slot);
    if lbu(m, record + 0x3e) == 0 {
        m.sb(record + 0x3e, 1);
        let height = m.lw(mover + 0xc).wrapping_add(0xff00_0000);
        m.sw(mover + 0xc, height);
        m.sb(record + 0x63, 0x1a);
    }
    m.sb(record + 0x61, 9);
}

// JP-REVIEW(R0008): near function: PAL 80047D78 -> JP 80048A80, imm:2, 2 instructions differ (whole routine)
/// 80048A80: the event value (+6) of the most recent of the context's
/// pending entries (+0x74C, 8 bytes, count +0x3C6) whose +1 flag is clear,
/// or -1.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::latest_event_47d78` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn latest_event_47d78(m: &mut Machine) -> u32 {
    crate::jp::res_p2_ai::latest_event_47d78(m)
}

/// 8004FD9C: movement state 16, scripted launch: place the entity at one
/// of six launch points (800B2048/800B2060, by side and rand % 3), climb
/// out, level off at the waypoint, then run the mission's +0x148 script.
pub fn launch_4e784(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        if lbu(m, record + 0x3e) == 0 {
            let mut point = 0u32;
            let event = latest_event_47d78(m);
            if event != u32::MAX {
                let source = entity(m, event);
                if lt(m.lw(source + 8), 0) {
                    point = 3;
                }
            }
            point = point.wrapping_add(rand_mod3(m));
            let x = m.lw(0x800b_2048u32.wrapping_add(point << 2));
            m.sw(mover + 8, x);
            let z = m.lw(0x800b_2060u32.wrapping_add(point << 2));
            m.sh(mover + 0x18, 0x1000);
            m.sh(mover + 0x1a, 0x2000);
            m.sh(mover + 0x1c, 0);
            m.sw(mover + 0x10, z);
            m.sb(record + 0x3e, 1);
        }
        if lbu(m, record + 0x3e) == 1 {
            m.sb(record + 0x61, 9);
            m.sb(record + 0x63, 0x1a);
            if lt(m.lw(mover + 0xc), 0xfec0_0000) {
                m.sb(record + 0x3e, 2);
            }
        }
        if lbu(m, record + 0x3e) == 2 {
            m.sb(record + 0x61, 0);
            m.sb(record + 0x63, 0);
            if lt(m.lw(mover + 0x10), 0xe000_0000) {
                m.sb(record + 0x3e, 3);
                let x = m.lw(mover + 8);
                m.sw(record + 0x6c, 0xe700_0000);
                m.sw(record + 0x70, 0xc700_0000);
                m.sw(record + 0x68, x);
            }
        }
        if lbu(m, record + 0x3e) == 3 {
            m.sb(record + 0x61, 0);
            m.sb(record + 0x63, 5);
            if lt(m.lw(mover + 0xc), 0xe700_0000) {
                m.sb(record + 0x3e, 4);
                let mission = m.gw(0x338);
                let script = m.lw(mission + 0x148);
                crate::script::interpret(m, script);
            }
        }
    });
}

/// 8004FF88: movement state 17, escorted landing: follow the target
/// (+0x3F) position, swaying sideways until close; within 0x200 run the
/// mission's +0x148 script.
pub fn escorted_4e970(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        let target_slot = lbu(m, record + 0x3f);
        let target = entity(m, target_slot);
        if lbu(m, record + 0x3e) == 0 {
            m.sb(record + 0x3e, 1);
            m.sb(record + 0x63, 4);
            m.sb(record + 0x61, 0);
        }
        if lbu(m, record + 0x3e) != 1 {
            return;
        }
        copy_block(m, record + 0x68, target + 8, 4);
        if !lt(m.lw(record + 0x88), 0x2001) {
            let gameplay = m.gw(common::GAMEPLAY);
            let sway = (m.lw(gameplay + 0xc) << 8) & 0x7f00;
            let sway = lh(m, SINE + sway);
            let heading = lhu(m, mover + 0x1a) & 0x3fff;
            let sine = sine_at(m, heading);
            let dx = lo(sine, sway);
            let heading = lhu(m, mover + 0x1a) & 0x3fff;
            let cosine = sine_at(m, heading + 0x1000);
            let dz = lo(cosine, sway);
            let x = m.lw(mover + 8).wrapping_add(sra(dx, 14));
            m.sw(mover + 8, x);
            let z = m.lw(mover + 0x10).wrapping_add(sra(dz, 14));
            m.sw(mover + 0x10, z);
        }
        if lt(m.lw(record + 0x88), 0x200) {
            let mission = m.gw(0x338);
            let script = m.lw(mission + 0x148);
            crate::script::interpret(m, script);
        }
    });
}

/// 800500EC: movement state 18: action 0x1B and speed mode 9 on entry.
pub fn state_18_4ead4(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    if lbu(m, record + 0x3e) == 0 {
        m.sb(record + 0x63, 0x1b);
        m.sb(record + 0x61, 9);
        m.sb(record + 0x3e, 1);
    }
}

/// 80050134: movement state 19, hover: clear the turn requests on entry;
/// speed mode 8, action 0.
pub fn hover_4eb1c(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    if lbu(m, record + 0x3e) == 0 {
        m.sh(record + 0x80, 0);
        m.sh(record + 0x82, 0);
        m.sh(record + 0x84, 0);
        m.sb(record + 0x3e, 1);
    }
    m.sb(record + 0x61, 8);
    m.sb(record + 0x63, 0);
}

// JP-REVIEW(R0009): near function: PAL 8004EB6C -> JP 80050184, del:3 imm:3 ins:3 op:6, 15 instructions differ (whole routine)
/// 80050184: movement state 20, timed scripted flight: the hold time
/// (+0x40) depends on the mission (and slot in mission 0x29); then action
/// 0x34 toward the origin; record flag 0x400000 ends it and invalidates
/// the slot info +0x24.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::timed_flight_4eb6c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn timed_flight_4eb6c(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::timed_flight_4eb6c(m, slot)
}

// JP-REVIEW(R0011): near function: PAL 8004ECBC -> JP 800502D4, imm:6 ins:2 op:4, 12 instructions differ (whole routine)
/// 800502D4: movement state 21, escort duty: count the escort time
/// (+0x40, 150 frames) and turn away after it; while gameplay +4 bit
/// 0x800, track whether the escorted object is safe (player +8/+0xA, radio
/// calls every 16 frames), and nag (gameplay bit 0x2000) when the player
/// strays.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::escort_duty_4ecbc` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn escort_duty_4ecbc(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::escort_duty_4ecbc(m, slot)
}

/// 80050534: movement state 22, peel off: turn away (attitude -0x800 roll,
/// 0x800 pitch) once, wait for record flag 0x400000; speed mode 8.
pub fn peel_off_4ef14(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    match lbu(m, record + 0x3e) {
        0 => {
            m.sb(record + 0x63, 0);
            m.sb(record + 0x3e, 1);
        }
        1 => {
            m.sb(record + 0x63, 0xc);
            m.sh(record + 0x7c, 0xf800);
            m.sh(record + 0x78, 0x800);
            m.sh(record + 0x7a, 0);
            m.sb(record + 0x3e, 2);
        }
        2 => {
            if m.lw(record) & 0x40_0000 != 0 {
                m.sb(record + 0x3e, 3);
            }
        }
        3 => m.sb(record + 0x63, 0),
        _ => {}
    }
    m.sb(record + 0x61, 8);
}

// ==== steering helpers of the action states ======================================

/// 80044034: request a return to level flight: roll (+0x84) = -bank,
/// then pitch (+0x80) = -pitch when already level. Returns 1 while a
/// request remains, else 0.
pub fn level_off_434ec(m: &mut Machine, slot: u32) -> u32 {
    let mover = entity(m, slot);
    let record = slot_record(m, slot);
    let roll = lhu(m, mover + 0x1c).wrapping_neg();
    m.sh(record + 0x84, roll as u16);
    if roll & 0xffff != 0 {
        return 1;
    }
    let pitch = lhu(m, mover + 0x18).wrapping_neg();
    m.sh(record + 0x80, pitch as u16);
    u32::from(pitch & 0xffff != 0)
}

/// 8004409C: clear the turn requests and turn the entity's bank (+0x1C),
/// else its pitch (+0x18), toward zero within the type's rates. Returns 1
/// while turning, else 0.
pub fn relax_attitude_43554(m: &mut Machine, slot: u32) -> u32 {
    let mover = entity(m, slot);
    let info = slot_info(m, slot);
    let record = slot_record(m, slot);
    let params = m.lw(info + 0x14);
    m.sh(record + 0x84, 0);
    m.sh(record + 0x82, 0);
    m.sh(record + 0x80, 0);
    let bank = lh(m, mover + 0x1c);
    let bank_bits = lhu(m, mover + 0x1c);
    if bank != 0 {
        let mut step = bank.wrapping_neg();
        let rate = lh(m, params + 0x18);
        if lt(rate, step) {
            step = rate;
        }
        let rate = rate.wrapping_neg();
        if lt(step, rate) {
            step = rate;
        }
        m.sh(mover + 0x1c, bank_bits.wrapping_add(step) as u16);
        return 1;
    }
    let pitch = lh(m, mover + 0x18);
    let pitch_bits = lhu(m, mover + 0x18);
    if pitch == 0 {
        return 0;
    }
    let mut step = pitch.wrapping_neg();
    let up = lh(m, params + 0x1a);
    if lt(up, step) {
        step = up;
    }
    let down = lh(m, params + 0x1c);
    if lt(step, down) {
        step = down;
    }
    m.sh(mover + 0x18, pitch_bits.wrapping_add(step) as u16);
    1
}

/// 80044188: aim along `direction` (VECTOR) at `distance` (sign-extended
/// halfword): `out` receives the pitch, yaw and roll toward it (14-bit
/// angles), stored as the turn requests (+0x84 roll, then +0x80 pitch
/// and +0x82 yaw when the roll is small). Returns 0 below distance 0x80.
pub fn aim_43640(m: &mut Machine, slot: u32, direction: u32, distance: u32, out: u32) -> u32 {
    with_frame(m, 0x28, |m, _| {
        let distance = sext16(distance);
        let mover = entity(m, slot);
        let record = slot_record(m, slot);
        if lt(distance, 0x80) {
            return 0;
        }
        let dy = m.lw(direction + 4).wrapping_neg();
        let dz = m.lw(direction + 8);
        let pitch = shared::atan2(m, dy, dz);
        m.sh(out, pitch as u16);
        let dx = m.lw(direction);
        let dz = m.lw(direction + 8);
        let yaw = shared::atan2(m, dx, dz);
        m.sh(out + 2, yaw as u16);
        if lt(distance, 0x300) {
            let bank = lhu(m, mover + 0x1c).wrapping_neg();
            m.sh(out + 4, bank as u16);
        } else if !lt(distance, 0x1fff) {
            m.sh(out + 4, 0);
        } else {
            let dy = m.lw(direction + 4).wrapping_neg();
            let dx = m.lw(direction);
            let roll = shared::atan2(m, dx, dy);
            let pitch = lh(m, out);
            m.sh(out + 4, roll as u16);
            if lt(pitch, 0) && !lt(pitch, 0xffff_fc01) {
                let yaw = lh(m, out + 2);
                if lt(yaw, 0x200) && !lt(yaw, 0xffff_fe01) {
                    m.sh(out + 4, roll.wrapping_add(0x2000) as u16);
                }
            }
            let roll = lhu(m, out + 4) & 0x3fff;
            m.sh(out + 4, roll as u16);
            let pitch = lhu(m, out) & 0x3fff;
            let wrap_roll = !lt(lh(m, out + 4), 0x2000);
            m.sh(out, pitch as u16);
            let roll = lhu(m, out + 4);
            if wrap_roll {
                m.sh(out + 4, roll.wrapping_sub(0x4000) as u16);
            }
            if !lt(lh(m, out), 0x2000) {
                let pitch = lhu(m, out).wrapping_sub(0x4000);
                m.sh(out, pitch as u16);
            }
        }
        let roll = m.lhu(out + 4);
        m.sh(record + 0x84, roll);
        if (lhu(m, out + 4).wrapping_add(0x7f) & 0xffff) >= 0xff {
            return 1;
        }
        let pitch = m.lhu(out);
        m.sh(record + 0x80, pitch);
        if (lhu(m, out).wrapping_add(0x7f) & 0xffff) >= 0xff {
            return 1;
        }
        let yaw = m.lhu(out + 2);
        m.sh(record + 0x82, yaw);
        1
    })
}

/// 80044360: level the wings, then pitch toward the height of `target`
/// (VECTOR), limited by the type's climb angle (params +0x16, default
/// 0xE38) and at least 8. Returns 1.
pub fn climb_to_43818(m: &mut Machine, slot: u32, target: u32) -> u32 {
    with_frame(m, 0x20, |m, _| {
        let mover = entity(m, slot);
        let record = slot_record(m, slot);
        let roll = lhu(m, mover + 0x1c).wrapping_neg();
        m.sh(record + 0x84, roll as u16);
        if roll & 0xffff != 0 {
            return 1;
        }
        let rise = sra(m.lw(target + 4).wrapping_sub(m.lw(mover + 0xc)), 12);
        let mut angle = shared::atan2(m, rise.wrapping_neg(), 0x800);
        let info = slot_info(m, slot);
        let params = m.lw(info + 0x14);
        let mut limit = lh(m, params + 0x16);
        if limit == 0 {
            limit = 0xe38;
        }
        if lt(limit, angle) {
            angle = limit;
        }
        let limit = limit.wrapping_neg();
        if lt(angle, limit) {
            angle = limit;
        }
        if angle.wrapping_sub(1) < 7 {
            angle = 8;
        }
        if lt(angle, 0) && !lt(angle, 0xffff_fff9) {
            angle = 0xffff_fff8;
        }
        let pitch = lhu(m, mover + 0x18);
        m.sh(record + 0x80, angle.wrapping_sub(pitch) as u16);
        1
    })
}

/// 80044474: bank toward the bearing of `target` (VECTOR) within the
/// type's bank limit (params +0x14, at most 0x1000) and turn the heading
/// (+0x1A) by the bank-scaled yaw rate. Returns 0 when already on bearing.
pub fn turn_to_4392c(m: &mut Machine, slot: u32, target: u32) -> u32 {
    with_frame(m, 0x28, |m, _| {
        let info = slot_info(m, slot);
        let params = m.lw(info + 0x14);
        let mover = entity(m, slot);
        let record = slot_record(m, slot);
        let mut bank_limit = lhu(m, params + 0x14);
        if !lt(lh(m, params + 0x14), 0x1001) {
            bank_limit = 0x1000;
        }
        if bank_limit == 0 {
            bank_limit = 0x1000;
        }
        let dx = sra(m.lw(target).wrapping_sub(m.lw(mover + 8)), 12);
        let dz = sra(m.lw(target + 8).wrapping_sub(m.lw(mover + 0x10)), 12);
        let bearing = shared::atan2(m, dx, dz);
        let heading = lhu(m, mover + 0x1a);
        let turn = bearing.wrapping_sub(heading) & 0x3fff;
        if turn == 0 {
            return 0;
        }
        let mut bank = if lt(turn, 0x2000) {
            turn
        } else {
            turn | 0xc000
        };
        let signed = sext16(bank);
        let limit = sext16(bank_limit);
        if lt(signed, limit.wrapping_neg()) {
            bank = bank_limit.wrapping_neg();
        } else if lt(limit, signed) {
            bank = bank_limit;
        }
        let roll = lhu(m, mover + 0x1c);
        m.sh(record + 0x84, bank.wrapping_sub(roll) as u16);
        let rate = lh(m, params + 0x1a);
        let current = lh(m, mover + 0x1c);
        let product = lo(rate, current);
        let cap = lh(m, params + 0x1e);
        let mut yaw = sra(product, 12);
        let scaled = sext16(yaw);
        let cap_bits = lhu(m, params + 0x1e);
        if lt(abs(scaled), cap) {
            yaw = if lt(scaled, 0) {
                cap_bits.wrapping_neg()
            } else {
                cap_bits
            };
        }
        if lt(abs(sext16(bank)), abs(sext16(yaw))) {
            yaw = bank;
        }
        let heading = lhu(m, mover + 0x1a).wrapping_add(yaw);
        m.sh(mover + 0x1a, heading as u16);
        1
    })
}

// ==== action states (table 80023EB8) ================================================

/// Action-state preamble: slot flag 0x20 (manoeuvring).
fn manoeuvring(m: &mut Machine, slot: u32) {
    set_word_bits(m, SLOT_FLAGS.wrapping_add(slot << 2), 0x20);
}

/// 8004960C: action 4 (variant), aim at the attack vector (+0x8C, distance
/// +0x9C).
pub fn aim_47ed4(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x20, |m, frame| {
        manoeuvring(m, slot);
        let record = slot_record(m, slot);
        let distance = lh(m, record + 0x9c);
        aim_43640(m, slot, record + 0x8c, distance, frame + 0x10)
    })
}

/// 8004966C: action 4, approach the destination (+0x68): 0 check the
/// height difference, 1 aim at the destination (player helpers
/// 80044F94/80045090), 2 level off, 3 climb to its height, 4 done.
pub fn approach_47f34(m: &mut Machine, slot: u32) {
    with_frame(m, 0x48, |m, frame| {
        let mover = entity(m, slot);
        manoeuvring(m, slot);
        let record = slot_record(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                let x = sra(m.lw(record + 0x68), 12);
                m.sw(frame + 0x10, x);
                let z = sra(m.lw(record + 0x70), 12);
                m.sw(frame + 0x18, z);
                let angle = shared::atan2(m, x, z);
                let height = m.lw(mover + 0xc);
                let phase = if lt(abs(height.wrapping_sub(angle)), 0x401) {
                    2
                } else {
                    1
                };
                m.sb(record + 0x64, phase);
            }
            1 => {
                let x = m.lw(record + 0x68);
                m.sw(frame + 0x10, x);
                let y = m.lw(mover + 0xc);
                m.sw(frame + 0x14, y);
                let z = m.lw(record + 0x70);
                m.sw(frame + 0x18, z);
                original(m, 0x8004_4f94, [mover + 8, frame + 0x10, frame + 0x20, 0]);
                let distance = original(m, 0x8004_5090, [frame + 0x20, 0, 0, 0]);
                aim_43640(m, slot, frame + 0x20, sext16(distance), frame + 0x30);
                if lt(abs(lh(m, frame + 0x30)), 0x10) && lt(abs(lh(m, frame + 0x32)), 0x10) {
                    m.sb(record + 0x64, 2);
                }
            }
            2 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 3);
                }
            }
            3 => {
                climb_to_43818(m, slot, record + 0x68);
                let gap = abs(m.lw(mover + 0xc).wrapping_sub(m.lw(record + 0x6c)));
                if !lt(0x7_ffff, gap) {
                    m.sb(record + 0x64, 4);
                }
            }
            4 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 80049A1C: action 5 (variant), aim at the attack vector, level off, done.
pub fn attack_aim_482e4(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, frame| {
        manoeuvring(m, slot);
        let record = slot_record(m, slot);
        match lbu(m, record + 0x64) {
            0 => m.sb(record + 0x64, 1),
            1 => {
                let distance = lh(m, record + 0x9c);
                aim_43640(m, slot, record + 0x8c, distance, frame + 0x10);
            }
            2 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 3);
                }
            }
            3 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 80049AF8: action 6, fly to the destination: level off, turn to it,
/// level off, climb to its height (repeating), done.
pub fn fly_to_483c0(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let mover = entity(m, slot);
        manoeuvring(m, slot);
        let record = slot_record(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 1);
                }
            }
            1 => {
                if turn_to_4392c(m, slot, record + 0x68) == 0 {
                    m.sb(record + 0x64, 2);
                }
            }
            2 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 3);
                }
            }
            3 => {
                climb_to_43818(m, slot, record + 0x68);
                let gap = abs(m.lw(mover + 0xc).wrapping_sub(m.lw(record + 0x6c)));
                if !lt(0x3_ffff, gap) {
                    m.sb(record + 0x64, 0);
                }
            }
            4 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x63, 0xff);
                }
            }
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 80049C4C: action 5 (kind 3), hover-turn toward the destination: stop
/// rotating, then yaw (+0x1A) toward it within the yaw rate and move the
/// height up to 0x5000 per frame toward its height.
pub fn hover_turn_48514(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let mover = entity(m, slot);
        manoeuvring(m, slot);
        let info = slot_info(m, slot);
        let params = m.lw(info + 0x14);
        let record = slot_record(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                m.sh(mover + 0x34, 0);
                m.sh(mover + 0x32, 0);
                m.sh(mover + 0x30, 0);
                m.sh(record + 0x84, 0);
                m.sh(record + 0x82, 0);
                m.sh(record + 0x80, 0);
                m.sb(record + 0x64, 1);
            }
            1 => {
                let dx = sra(m.lw(record + 0x68).wrapping_sub(m.lw(mover + 8)), 12);
                let dz = sra(m.lw(record + 0x70).wrapping_sub(m.lw(mover + 0x10)), 12);
                let bearing = shared::atan2(m, dx, dz);
                let heading = lhu(m, mover + 0x1a);
                let turn = bearing.wrapping_sub(heading) & 0x3fff;
                let turn = if lt(turn, 0x2000) {
                    turn
                } else {
                    turn | 0xc000
                };
                let signed = sext16(turn);
                let cap = lh(m, params + 0x1e);
                let cap_bits = lhu(m, params + 0x1e);
                if lt(abs(signed), cap) {
                    m.sh(mover + 0x1a, heading.wrapping_add(turn) as u16);
                } else if lt(0, signed) {
                    m.sh(mover + 0x1a, heading.wrapping_add(cap_bits) as u16);
                } else if lt(signed, 0) {
                    m.sh(mover + 0x1a, heading.wrapping_sub(cap_bits) as u16);
                }
                let height = m.lw(mover + 0xc);
                let rise = m.lw(record + 0x6c).wrapping_sub(height);
                let height = if !lt(rise, 0x5001) {
                    height.wrapping_add(0x5000)
                } else if lt(rise, 0xffff_b000) {
                    height.wrapping_sub(0x5000)
                } else {
                    height.wrapping_add(rise)
                };
                m.sw(mover + 0xc, height);
            }
            2 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 0);
                }
            }
            3 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x63, 0xff);
                }
                m.sb(record + 0x64, 0);
            }
            _ => m.sb(record + 0x64, 0),
        }
    });
}

// JP-REVIEW(R0013): near function: PAL 80048134 -> JP 8004986C, imm:3 reg:3, 6 instructions differ (whole routine)
/// 8004986C: action 7, dock: ease the attitude toward the target (+0x3F)
/// by 1/64 and the position toward the destination by 1/256 per frame.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::dock_48134` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn dock_48134(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::dock_48134(m, slot)
}

/// 80049E28: action 0, level off once, then hold (phase 2 ends).
pub fn level_486f0(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 1);
                }
            }
            1 => {}
            2 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 80049EC4: action 8, cruise (phase 1 holds, phase 2 ends).
pub fn cruise_4878c(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    match lbu(m, record + 0x64) {
        0 => m.sb(record + 0x64, 1),
        1 => {}
        2 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

/// 80049F38: action 9, strafe: speed mode 1, aim at the attack vector,
/// level off, done.
pub fn strafe_48800(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, frame| {
        manoeuvring(m, slot);
        let record = slot_record(m, slot);
        m.sb(record + 0x61, 1);
        match lbu(m, record + 0x64) {
            0 => m.sb(record + 0x64, 1),
            1 => {
                let distance = lh(m, record + 0x9c);
                aim_43640(m, slot, record + 0x8c, distance, frame + 0x10);
            }
            2 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 3);
                }
            }
            3 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 8004A2D8: action 10, loop: level off, then a full pitch request of
/// 0x4000 until it is used up.
pub fn loop_48ba0(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 1);
                    m.sh(record + 0x80, 0x4000);
                }
            }
            1 => {
                if lh(m, record + 0x80) == 0 {
                    m.sb(record + 0x64, 2);
                }
            }
            2 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 8004A394: action 11, half loop: level off, pitch 0x2000, level off, done.
pub fn half_loop_48c5c(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 1);
                    m.sh(record + 0x80, 0x2000);
                }
            }
            1 => {
                if lh(m, record + 0x80) == 0 {
                    m.sb(record + 0x64, 2);
                }
            }
            2 => {
                if level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 3);
                }
            }
            3 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// The knife-edge roll target of 8004A474/8004A56C: away from the side of
/// the entity's z (+0x10), by x parity when centred.
fn knife_edge_roll(m: &mut Machine, mover: u32) -> u32 {
    let z = m.lw(mover + 0x10);
    if lt(0, z) {
        0xffff_c000u32.wrapping_sub(lhu(m, mover + 0x1c))
    } else if lt(z, 0) {
        0x4000u32.wrapping_sub(lhu(m, mover + 0x1c))
    } else if m.lw(mover + 8) & 1 != 0 {
        0xffff_c000
    } else {
        0x4000
    }
}

/// 8004A474: action 13, knife edge: roll to 90° and hold until the roll
/// request is used up.
pub fn knife_edge_48d3c(m: &mut Machine, slot: u32) {
    let mover = entity(m, slot);
    let record = slot_record(m, slot);
    let phase = lbu(m, record + 0x64);
    match phase {
        0 | 1 => {
            if phase == 0 {
                let roll = knife_edge_roll(m, mover);
                m.sh(record + 0x84, roll as u16);
                m.sb(record + 0x64, 1);
            }
            if lh(m, record + 0x84) == 0 {
                m.sb(record + 0x64, 2);
            }
        }
        2 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

/// 8004A56C: action 14, knife-edge slide: as 8004A474 with a sideways
/// drift (+0x24 = -0x15000, entity request 0x200000) while rolling.
pub fn knife_slide_48e34(m: &mut Machine, slot: u32) {
    let mover = entity(m, slot);
    let record = slot_record(m, slot);
    let phase = lbu(m, record + 0x64);
    match phase {
        0 | 1 => {
            if phase == 0 {
                let roll = knife_edge_roll(m, mover);
                m.sh(record + 0x84, roll as u16);
                m.sb(record + 0x64, 1);
            }
            m.sw(mover + 0x24, 0xfffe_b000);
            set_word_bits(m, mover + 0x54, 0x20_0000);
            if lh(m, record + 0x84) == 0 {
                m.sw(mover + 0x24, 0);
                m.sb(record + 0x64, 2);
            }
        }
        2 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

// JP-REVIEW(R0014): near function: PAL 80048F40 -> JP 8004A678, imm:1 ins:1 op:2, 4 instructions differ (whole routine)
/// 8004A678: action 12, manoeuvre to the attitude at +0x78..+0x7C: novice
/// pilots (and missions 0x20/0x2F/0x35) roll first then pitch; others
/// request roll and pitch together.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::attitude_48f40` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn attitude_48f40(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::attitude_48f40(m, slot)
}

/// 8004A7E8: actions 15-17, landing: level off and stop (speed mode 9),
/// turn by +0x7C, bleed speed and pitch by +0x78, flare (speed mode 10)
/// until the speed (+0x28) reaches the type's minimum (params +0xC).
pub fn landing_490ac(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        let info = slot_info(m, slot);
        let phase = lbu(m, record + 0x64);
        let mover = entity(m, slot);
        let params = m.lw(info + 0x14);
        match phase {
            0 => {
                m.sb(record + 0x61, 0);
                if level_off_434ec(m, slot) != 0 {
                    return;
                }
                m.sb(record + 0x61, 9);
                m.sw(mover + 0x28, 0);
                m.sw(mover + 0x24, 0);
                m.sw(mover + 0x20, 0);
                let roll = lhu(m, record + 0x7c);
                m.sb(record + 0x64, 1);
                m.sh(record + 0x84, roll as u16);
            }
            1 => {
                if lh(m, record + 0x84) == 0 {
                    m.sb(record + 0x64, 2);
                }
            }
            2 => {
                for offset in [0x38, 0x40] {
                    let velocity = m.lw(mover + offset);
                    if lt(0xa000, abs(velocity)) {
                        m.sw(mover + offset, sra(velocity, 1));
                    }
                }
                let mut step = lh(m, params + 0x1a) << 1;
                let remaining = lh(m, record + 0x78);
                if lt(remaining, step) {
                    step = remaining;
                }
                let pitch = lhu(m, mover + 0x18).wrapping_add(step);
                m.sh(mover + 0x18, pitch as u16);
                let left = lhu(m, record + 0x78).wrapping_sub(step);
                m.sh(record + 0x78, left as u16);
                if left & 0xffff == 0 {
                    m.sb(record + 0x64, 3);
                }
            }
            3 => {
                m.sb(record + 0x61, 0xa);
                let pitch = lh(m, mover + 0x18);
                let pitch_bits = lhu(m, mover + 0x18);
                if lt(0, pitch) && lt(abs(lh(m, mover + 0x1c)), 0x1000) {
                    let rate = lh(m, params + 0x1a);
                    let rate_bits = lhu(m, params + 0x1a);
                    if lt(rate, pitch) {
                        m.sh(mover + 0x18, pitch_bits.wrapping_sub(rate_bits) as u16);
                    }
                } else {
                    let pitch = lh(m, mover + 0x18);
                    let pitch_bits = lhu(m, mover + 0x18);
                    if !lt(pitch, 0xffff_f001) {
                        let rate_bits = lhu(m, params + 0x1a);
                        m.sh(mover + 0x18, pitch_bits.wrapping_sub(rate_bits) as u16);
                    }
                }
                if lt(m.lw(mover + 0x28), m.lw(params + 0xc)) {
                    return;
                }
                if lh(m, record + 0x7a) != 0 || level_off_434ec(m, slot) == 0 {
                    m.sb(record + 0x64, 4);
                }
            }
            4 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 800C71A0: the 64-frame position/attitude history of 8004AD98 (0x18
/// bytes per frame).
const SHADOW_TRACK: u32 = 0x800c_71a0;
/// 800C77A0: per-frame flags of [`SHADOW_TRACK`] (64 bytes).
const SHADOW_FLAGS: u32 = 0x800c_77a0;

/// Clear an entity's velocity (+0x38..+0x40) and its speed components
/// (+0x20..+0x28), in the original store order.
fn stop_moving(m: &mut Machine, mover: u32) {
    for offset in [0x38, 0x3c, 0x40, 0x20, 0x24, 0x28] {
        m.sw(mover + offset, 0);
    }
}

// JP-REVIEW(R0015): near function: PAL 80049300 -> JP 8004AA3C, del:1 ins:1 op:1, 3 instructions differ (whole routine)
/// 8004AA3C: action 25, catapult launch: placed on the deck, swung round a
/// circle of radius 0x800 (1/8 turn per `length` frames; 0xA8C frames in
/// mission 0x1A, else 0x1C20) while rising to the deck height, then run
/// out along -x for 0x130 frames.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::catapult_49300` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn catapult_49300(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::catapult_49300(m, slot)
}

/// 8004AC90: action 26, vertical climb-out: stop, then accelerate upward
/// (+0x3C minus params +0x10 down to -params +4) until above 0xE7000000.
pub fn climb_out_49554(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    let mover = entity(m, slot);
    let info = slot_info(m, slot);
    let params = m.lw(info + 0x14);
    m.sb(record + 0x61, 9);
    match lbu(m, record + 0x64) {
        0 => {
            stop_moving(m, mover);
            m.sb(record + 0x64, 1);
        }
        1 => {
            let limit = m.lw(params + 4).wrapping_neg();
            let rise = m.lw(mover + 0x3c);
            if lt(limit, rise) {
                let step = m.lw(params + 0x10);
                m.sw(mover + 0x3c, rise.wrapping_sub(step));
            }
            if lt(m.lw(mover + 0xc), 0xe700_0000) {
                m.sb(record + 0x64, 2);
            }
        }
        2 => m.sw(mover + 0x3c, 0),
        3 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

/// 8004AD98: action 27, shadow the player 64 frames behind: on entry the
/// history (800C71A0) is filled with the player's position extrapolated
/// back along its velocity; afterwards each frame replays the entry of
/// the frame counter (`[gp+0x2EC]` +0x24, mod 64) and records the player's
/// current state in its place.
pub fn shadow_4965c(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    let mover = entity(m, slot);
    let player = entity(m, 0);
    let phase = lbu(m, record + 0x64);
    m.sb(record + 0x61, 9);
    match phase {
        0 => {
            for index in 0..0x40u32 {
                let lag = 0x40 - index;
                let entry = SHADOW_TRACK + index * 0x18;
                for axis in [0, 4, 8] {
                    let velocity = m.lw(player + 0x38 + axis);
                    let position = m.lw(player + 8 + axis);
                    m.sw(entry + axis, position.wrapping_sub(lo(velocity, lag)));
                }
                // lwl/lwr + swl/swr: the attitude words +0x18/+0x1C.
                let attitude = m.read_bytes(player + 0x18, 8);
                m.write_bytes(entry + 0x10, &attitude);
                m.sb(SHADOW_FLAGS + index, 1);
            }
            m.sb(record + 0x64, 1);
        }
        1 => {
            let context = m.gw(common::CONTEXT);
            let index = m.lw(context + 0x24) & 0x3f;
            let entry = SHADOW_TRACK + index * 0x18;
            let flag = SHADOW_FLAGS + index;
            copy_block(m, mover + 8, entry, 4);
            copy_block(m, mover + 0x18, entry + 0x10, 2);
            if lbu(m, flag) == 0 {
                m.sh(mover + 0x4e, 0);
            }
            copy_block(m, entry, player + 8, 4);
            copy_block(m, entry + 0x10, player + 0x18, 2);
            let firing = if lt(0, lh(m, player + 0x4e)) {
                phase
            } else {
                0
            };
            m.sb(flag, firing as u8);
            copy_block(m, mover + 0x38, player + 0x38, 4);
        }
        2 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

/// 8004AFAC: action 30, return into the pilot's height band (pilot block
/// +4 and +8): relax the attitude, then climb toward the far bound while
/// outside the band.
pub fn height_band_49870(m: &mut Machine, slot: u32) {
    with_frame(m, 0x38, |m, frame| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        let info = slot_info(m, slot);
        let pilot = m.lw(info + 0x18);
        let lower = m.lw(pilot + 8);
        let upper = m.lw(pilot + 4);
        match lbu(m, record + 0x64) {
            0 => {
                if relax_attitude_43554(m, slot) != 0 {
                    return;
                }
                let height = m.lw(mover + 0xc);
                let phase = if lt(lower, height) || lt(height, upper) {
                    1
                } else {
                    2
                };
                m.sb(record + 0x64, phase);
            }
            1 => {
                let height = m.lw(mover + 0xc);
                let goal = if lt(lower, height) {
                    upper
                } else if lt(height, upper) {
                    lower
                } else {
                    m.sb(record + 0x64, 2);
                    return;
                };
                m.sw(frame + 0x18, 0);
                m.sw(frame + 0x10, 0);
                m.sw(frame + 0x14, goal);
                climb_to_43818(m, slot, frame + 0x10);
            }
            2 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

// JP-REVIEW(R0016): near function: PAL 800499CC -> JP 8004B108, del:1 imm:3 ins:3 op:4, 11 instructions differ (whole routine)
/// 8004B108: action 31, turn back toward the origin (offset by (-0x2000,
/// 0x2000) in missions 0x20/0x2F/0x35): roll 0x1000 toward the side of
/// the turn, then pitch through it (+0x78); speed mode 2 meanwhile.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::turn_back_499cc` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn turn_back_499cc(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::turn_back_499cc(m, slot)
}

// JP-REVIEW(R0017): near function: PAL 80049BC4 -> JP 8004B308, imm:1, 1 instructions differ (whole routine)
/// 8004B308: action 40, explode (large for object type 0x3E), then retire.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::blow_up_49bc4` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn blow_up_49bc4(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::blow_up_49bc4(m, slot)
}

/// The end of a falling wreck: explosion, sound 0x1B, phase 2.
fn wreck_explodes(m: &mut Machine, mover: u32, record: u32) {
    explosion_7fc50(m, mover + 8, mover + 0x38, 0);
    sound_at(m, 0x1b, mover + 8, 0);
    m.sb(record + 0x64, 2);
}

/// 8004B3C8: action 41, burn: smoke (kind 1) for 16..47 frames (by the x
/// position), then explode and retire.
pub fn burn_49c84(m: &mut Machine, slot: u32) {
    with_frame(m, 0x28, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        match lbu(m, record + 0x64) {
            0 => m.sb(record + 0x64, 1),
            1 => {
                let frames = lh(m, record + 0x66);
                smoke(m, mover + 8, mover + 0x38, frames, 1);
                let limit = (m.lw(mover + 8) & 0x1f).wrapping_add(0x10);
                if !lt(lh(m, record + 0x66), limit) {
                    wreck_explodes(m, mover, record);
                }
            }
            2 => mark_destroyed(m, slot),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

// JP-REVIEW(R0018): near function: PAL 80049D94 -> JP 8004B4D8, imm:1 ins:1 op:2, 4 instructions differ (whole routine)
/// 8004B4D8: action 42, spinning fall: requests and speed cleared, a
/// random spin about one axis chosen by `[gp+0x2E0]` +0xC; then fall
/// (+0x3C += 0x400) trailing smoke until below the ground height (0, or
/// 0xFF800000 outside missions 0x20/0x2F/0x35), explode and retire.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::spin_fall_49d94` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn spin_fall_49d94(m: &mut Machine, slot: u32) {
    crate::jp::res_p2_ai::spin_fall_49d94(m, slot)
}

/// 8004B6E4: action 43, gliding fall: requests and speed cleared; then
/// bank up to 0x400 and pitch down to -0x800 by 4 per frame, sink
/// (+0x3C += 0x29D) trailing smoke until below 0xFF800000, explode and
/// retire.
pub fn glide_fall_49f9c(m: &mut Machine, slot: u32) {
    with_frame(m, 0x28, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                for offset in [0x84, 0x82, 0x80, 8, 6, 4] {
                    m.sh(record + offset, 0);
                }
                for offset in [0x28, 0x24, 0x20] {
                    m.sw(mover + offset, 0);
                }
                m.sb(record + 0x64, 1);
            }
            1 => {
                let bank = lh(m, mover + 0x1c);
                let bank_bits = lhu(m, mover + 0x1c);
                if bank_bits < 0x400 {
                    m.sh(mover + 0x1c, bank_bits.wrapping_add(4) as u16);
                } else if lt(bank, 0) && !lt(bank, 0xffff_fc01) {
                    m.sh(mover + 0x1c, bank_bits.wrapping_sub(4) as u16);
                } else {
                    let pitch = lh(m, mover + 0x18);
                    let pitch_bits = lhu(m, mover + 0x18);
                    if pitch_bits < 0x800 || (lt(pitch, 0) && !lt(pitch, 0xffff_f801)) {
                        m.sh(mover + 0x18, pitch_bits.wrapping_sub(4) as u16);
                    }
                }
                let flags = m.lw(mover + 0x54) | 0x80_0000;
                let fall = m.lw(mover + 0x3c).wrapping_add(0x29d);
                m.sw(mover + 0x54, flags);
                m.sw(mover + 0x3c, fall);
                let frames = lh(m, record + 0x66);
                smoke(m, mover + 8, mover + 0x38, frames, 0);
                if lt(0xff80_0000, m.lw(mover + 0xc)) {
                    wreck_explodes(m, mover, record);
                }
            }
            2 => mark_destroyed(m, slot),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// Phase 0 of 8004A018/8004A0C0: stop and clear the turn requests.
fn halt(m: &mut Machine, mover: u32, record: u32) {
    m.sb(record + 0x64, 1);
    stop_moving(m, mover);
    for offset in [0x80, 0x82, 0x84] {
        m.sh(record + offset, 0);
    }
}

/// 8004A018: action 50, halt and hold.
pub fn halt_488e0(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    let mover = entity(m, slot);
    match lbu(m, record + 0x64) {
        0 => halt(m, mover, record),
        1 => {}
        2 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

/// 8004A0C0: action 51, taxi: halt, turn the heading toward the
/// destination by up to 0x40 per frame, then roll 1.25 sine units per
/// frame until within 5 units, snapping onto it.
pub fn taxi_48988(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        match lbu(m, record + 0x64) {
            0 => halt(m, mover, record),
            1 => {
                let dx = sra(m.lw(record + 0x68).wrapping_sub(m.lw(mover + 8)), 12);
                let dz = sra(m.lw(record + 0x70).wrapping_sub(m.lw(mover + 0x10)), 12);
                let bearing = shared::atan2(m, dx, dz);
                let heading = lhu(m, mover + 0x1a);
                let turn = bearing.wrapping_sub(heading) & 0x3fff;
                let turn = if lt(turn, 0x2000) {
                    turn
                } else {
                    turn | 0xc000
                };
                let signed = sext16(turn);
                let heading = if !lt(signed, 0x41) {
                    heading.wrapping_add(0x40)
                } else if lt(signed, 0xffff_ffc0) {
                    heading.wrapping_sub(0x40)
                } else {
                    heading.wrapping_add(turn)
                };
                m.sh(mover + 0x1a, heading as u16);
                if turn & 0xffff == 0 {
                    m.sb(record + 0x64, 2);
                }
            }
            2 => {
                let heading = lhu(m, mover + 0x1a) & 0x3fff;
                let sine = sine_at(m, heading);
                let step_x = sra(sine.wrapping_mul(5), 2);
                let heading = lhu(m, mover + 0x1a) & 0x3fff;
                let cosine = sine_at(m, heading + 0x1000);
                let x = m.lw(mover + 8).wrapping_add(step_x);
                let step_z = sra(cosine.wrapping_mul(5), 2);
                let z = m.lw(mover + 0x10).wrapping_add(step_z);
                m.sw(mover + 8, x);
                m.sw(mover + 0x10, z);
                let goal_x = m.lw(record + 0x68);
                let goal_z = m.lw(record + 0x70);
                let dx = sra(goal_x.wrapping_sub(x), 12);
                let dz = sra(goal_z.wrapping_sub(z), 12);
                if lt(abs(dx), 5) && lt(abs(dz), 5) {
                    m.sw(mover + 8, goal_x);
                    let goal_z = m.lw(record + 0x70);
                    m.sw(mover + 0x10, goal_z);
                    m.sb(record + 0x64, 3);
                }
            }
            3 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 8004B88C: action 52, take-off roll: stop and derive the acceleration
/// (+0x14) that reaches the type's minimum speed (params +0xC) over the
/// run length +0x70; accelerate, then pitch up 0x400 until above the
/// pilot's height bound (+8). Always requests entity flag 0x200000.
pub fn take_off_4a144(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    let mover = entity(m, slot);
    let info = slot_info(m, slot);
    let params = m.lw(info + 0x14);
    match lbu(m, record + 0x64) {
        0 => {
            for offset in [0x40, 0x3c, 0x38, 0x28, 0x24, 0x20] {
                m.sw(mover + offset, 0);
            }
            let run = m.lw(record + 0x70) << 1;
            let speed = m.lw(params + 0xc);
            let frames = div(run, speed);
            let acceleration = div(speed, frames);
            m.sb(record + 0x64, 1);
            m.sw(record + 0x14, acceleration);
        }
        1 => {
            let speed = m.lw(mover + 0x28).wrapping_add(m.lw(record + 0x14));
            m.sw(mover + 0x28, speed);
            if !lt(speed, m.lw(params + 0xc)) {
                m.sb(record + 0x64, 2);
                m.sh(record + 0x80, 0x400);
            }
        }
        2 => {
            let pilot = m.lw(info + 0x18);
            let height = m.lw(mover + 0xc);
            if lt(height, m.lw(pilot + 8)) {
                m.sb(record + 0x64, 3);
            }
        }
        3 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
    set_word_bits(m, mover + 0x54, 0x20_0000);
}

// ==== behaviour selection (80046DFC pre-pass, 800469EC) ===============================

/// The engagement range of `slot`: `[[info +0x1C] +8]`.
fn engage_range(m: &mut Machine, slot: u32) -> u32 {
    let info = slot_info(m, slot);
    let limits = m.lw(info + 0x1c);
    m.lw(limits + 8)
}

/// Movement state 1 (pursuit) or 4 (escort) by the target distance +0xA0
/// against the engagement range: 1 -> 4 inside it, 4 -> 1 outside it.
/// Returns false for other states.
fn range_switch(m: &mut Machine, slot: u32, record: u32, state: u32) -> bool {
    match state {
        1 => {
            if m.lw(record + 0xa0) < engage_range(m, slot) {
                m.sb(record + 0x3d, 4);
            }
            true
        }
        4 => {
            if engage_range(m, slot) < m.lw(record + 0xa0) {
                m.sb(record + 0x3d, 1);
            }
            true
        }
        _ => false,
    }
}

/// Engage `+0x60`: movement state 1 toward it with no offset.
fn engage(m: &mut Machine, record: u32) {
    let target = lbu(m, record + 0x60);
    m.sb(record + 0x3d, 1);
    m.sw(record + 0x44, 0);
    m.sw(record + 0x48, 0);
    m.sw(record + 0x4c, 0);
    m.sb(record + 0x3f, target as u8);
}

/// Patrol: movement state 0, no target slot, destination = the waypoint
/// +0x20..+0x2C.
fn patrol(m: &mut Machine, record: u32) {
    m.sb(record + 0x3d, 0);
    m.sb(record + 0x3f, 0xff);
    copy_block(m, record + 0x44, record + 0x20, 4);
}

/// 80050720: default behaviour selection: without a target patrol and
/// look for one (80046124), engaging it when found; with one switch
/// between pursuit and escort by range (states 1/4, unless record flag
/// 0x1000000), otherwise release it (800464B0) and patrol. Returns 1.
pub fn select_default_4f100(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        if lbu(m, record + 0x60) == 0xff {
            patrol(m, record);
            if m.lw(record) & 0x200_0000 != 0 {
                return 1;
            }
            let found = pick_target(m, slot);
            m.sb(record + 0x60, found as u8);
            if found & 0xff != 0xff {
                engage_reversed(m, record);
            }
            return 1;
        }
        let state = lbu(m, record + 0x3c);
        if state == 0 {
            return 1;
        }
        if m.lw(record) & 0x100_0000 == 0 && range_switch(m, slot, record, state) {
            return 1;
        }
        let target = shared::release_lock(m, slot);
        m.sb(record + 0x60, target as u8);
        patrol(m, record);
        1
    })
}

/// [`engage`] with the offset words cleared in the order of 80050720.
fn engage_reversed(m: &mut Machine, record: u32) {
    let target = lbu(m, record + 0x60);
    m.sb(record + 0x3d, 1);
    m.sw(record + 0x4c, 0);
    m.sw(record + 0x48, 0);
    m.sw(record + 0x44, 0);
    m.sb(record + 0x3f, target as u8);
}

/// 800508D0: behaviour selection for escorts (types 2 and 4): retire when
/// the leader (+0x1F) is destroyed (returns 0); keep an escort formation
/// on the leader; without a target lock one (type 2: the leader's, else
/// 80046124) and engage it, or follow the leader (80050AC0); with one
/// switch by range, keep state 2, or release it and follow. Returns 1.
pub fn select_escort_4f2b0(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x28, |m, _| {
        let record = slot_record(m, slot);
        let leader = lbu(m, record + 0x1f);
        let leader_entity = entity(m, leader);
        if !lt(0, lh(m, leader_entity + 0x4e)) {
            m.sb(record + 0x1c, 0xff);
            return 0;
        }
        if lbu(m, record + 0x1c) == 4
            && lbu(m, record + 0x3c) == 2
            && m.lw(record) & 0x200_0000 != 0
            && lbu(m, record + 0x3f) == leader
        {
            return 1;
        }
        if m.lw(record) & 0x100_0000 == 0 {
            let previous = lbu(m, record + 0x60);
            if previous == 0xff {
                if lbu(m, record + 0x1c) == 2 {
                    let leader = lbu(m, record + 0x1f);
                    let target = shared::set_lock(m, slot, leader);
                    m.sb(record + 0x60, target as u8);
                }
                let current = lbu(m, record + 0x60);
                let engaged = current != previous || {
                    let found = pick_target(m, slot);
                    m.sb(record + 0x60, found as u8);
                    found & 0xff != current
                };
                if engaged {
                    engage(m, record);
                } else {
                    follow_leader_4f4a0(m, slot);
                }
                return 1;
            }
            let state = lbu(m, record + 0x3c);
            if range_switch(m, slot, record, state) || state == 2 {
                return 1;
            }
        }
        let target = shared::release_lock(m, slot);
        m.sb(record + 0x60, target as u8);
        follow_leader_4f4a0(m, slot);
        1
    })
}

/// 80050AC0: follow the leader (+0x1F) in movement state 2 with an offset
/// (+0x44..+0x4C): type 2 a random lateral offset 0x1000000 behind, type
/// 4 a point on a circle chosen by the slot, at a random height.
pub fn follow_leader_4f4a0(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        let leader = lbu(m, record + 0x1f);
        m.sb(record + 0x3f, leader as u8);
        if lbu(m, record + 0x3d) == 2 {
            return;
        }
        let kind = lbu(m, record + 0x1c);
        m.sb(record + 0x3d, 2);
        if kind == 2 {
            m.sw(record + 0x44, 0);
            let random = sdk::rand(m);
            m.sw(record + 0x48, (random & 0x1ff).wrapping_sub(0x100) << 7);
            m.sw(record + 0x4c, 0x100_0000);
        } else if kind == 4 {
            let angle = (slot & 0xf) << 10;
            let sine = sine_at(m, angle);
            m.sw(record + 0x44, sine << 7);
            let random = sdk::rand(m);
            let cosine = sine_at(m, angle + 0x1000);
            m.sw(record + 0x48, (random & 0x1ff).wrapping_sub(0x100) << 7);
            m.sw(record + 0x4c, cosine << 7);
        }
    });
}

/// The breakaway of 80050BB4: movement state 1 toward the leader with
/// +0x44/+0x48 cleared and +0x4C alternating (flag +0x1D) between
/// `frame << 13` and `(2 * frame - 20000) << 12`.
fn breakaway(m: &mut Machine, record: u32) {
    let leader = lbu(m, record + 0x1f);
    let toggle = lbu(m, record + 0x1d);
    m.sb(record + 0x3d, 1);
    m.sw(record + 0x44, 0);
    m.sw(record + 0x48, 0);
    m.sb(record + 0x3f, leader as u8);
    let offset = if toggle == 0 {
        m.sb(record + 0x1d, 1);
        let gameplay = m.gw(common::GAMEPLAY);
        m.lw(gameplay + 0xc) << 13
    } else {
        m.sb(record + 0x1d, 0);
        let gameplay = m.gw(common::GAMEPLAY);
        (m.lw(gameplay + 0xc) << 1).wrapping_sub(0x4e20) << 12
    };
    m.sw(record + 0x4c, offset);
}

/// 80050BB4: behaviour selection for formation members (types 9 and 10):
/// retire when the leader is destroyed (returns 0); target the leader
/// (slot flag 0x10000); count frames (+0x38): break away at 0, rejoin
/// the player's position at the pilot's period (pilot +0x14), switch by
/// range, or (other states) break away and restart. Returns 1.
pub fn select_formation_4f594(m: &mut Machine, slot: u32) -> u32 {
    let record = slot_record(m, slot);
    let leader = lbu(m, record + 0x1f);
    let leader_entity = entity(m, leader);
    if !lt(0, lh(m, leader_entity + 0x4e)) {
        m.sb(record + 0x1c, 0xff);
        return 0;
    }
    let target = lbu(m, record + 0x1f);
    m.sb(record + 0x3f, leader as u8);
    m.sb(record + 0x60, target as u8);
    set_word_bits(m, SLOT_FLAGS + (slot << 2), 0x1_0000);
    let count = m.lw(record + 0x38);
    if count == 0 {
        breakaway(m, record);
    } else {
        let info = slot_info(m, slot);
        let pilot = m.lw(info + 0x18);
        if count == lh(m, pilot + 0x14) {
            m.sb(record + 0x3d, 0);
            let player = entity(m, 0);
            copy_block(m, record + 0x44, player + 8, 4);
        } else {
            let state = lbu(m, record + 0x3c);
            if !range_switch(m, slot, record, state) {
                if state != 0 {
                    breakaway(m, record);
                    m.sw(record + 0x38, 0);
                }
                return 1;
            }
        }
    }
    let count = m.lw(record + 0x38).wrapping_add(1);
    m.sw(record + 0x38, count);
    1
}

/// 80050FA8 (record, waypoint) -> 9 or 0: load the waypoint's action
/// (+1 into +0x63), destination (+8..+0x14 into +0x68..+0x74) and
/// attitude (+0x18/+0x1C into +0x78/+0x7C); speed mode 9 for the ground
/// actions 50/51, else 0.
pub fn load_waypoint_4f988(m: &mut Machine, record: u32, point: u32) -> u32 {
    let action = lbu(m, point + 1);
    m.sb(record + 0x63, action as u8);
    copy_block(m, record + 0x68, point + 8, 4);
    copy_block(m, record + 0x78, point + 0x18, 2);
    let action = lbu(m, record + 0x63);
    if action.wrapping_sub(0x32) < 2 {
        m.sb(record + 0x61, 9);
        9
    } else {
        m.sb(record + 0x61, 0);
        0
    }
}

// JP-REVIEW(R0019): near function: PAL 8004F7C8 -> JP 80050DE8, imm:1, 1 instructions differ (whole routine)
/// 80050DE8: scripted route (route of the spawn record +2, 0x20 bytes per
/// waypoint): load the first waypoint; for type 0x3E abort to action 50
/// once the player is down; advance (record flag 0x400000) when the
/// waypoint's condition holds (kind 1: distance +0x88 below its value,
/// kind 2: frames +0x66 reached it); retire at action 0xFF (returns 0).
/// Movement state 13; returns 1.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::follow_route_4f7c8` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn follow_route_4f7c8(m: &mut Machine, slot: u32) -> u32 {
    crate::jp::res_p2_ai::follow_route_4f7c8(m, slot)
}

// JP-REVIEW(R0022): near function: PAL 8004F9F0 -> JP 80051010, del:2 ins:2 op:2, 6 instructions differ (whole routine)
/// 80051010: keyframe flight (track of the spawn record +2, 20-byte keys
/// of position and angles, 8 steps per key): velocity toward the
/// interpolated position, attitude interpolated (a yaw flip near 180°
/// becomes a pitch through the vertical); movement state 13, action 8,
/// speed mode 9. Retires at the end key (-1; returns 0). Mission 3 sets
/// entity byte +5 bit 8.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::keyframes_4f9f0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn keyframes_4f9f0(m: &mut Machine, slot: u32) -> u32 {
    crate::jp::res_p2_ai::keyframes_4f9f0(m, slot)
}

// JP-REVIEW(R0023): near function: PAL 8004FCE4 -> JP 80051304, del:1 imm:1 ins:3 op:4 reg:2, 11 instructions differ (whole routine)
/// 80051304: the movement state of special types: kind 7 -> 14, kind 8 ->
/// 15, kind 6 -> 16 (mission 0x1C) or 17 (mission 0x24) on the leader,
/// others -> 18 on the leader in missions 6 and 0x37. Returns the
/// leftover `v0` (the state, or the last compared constant).
// JP-PATCH: replaced by `crate::jp::res_p2_ai::special_state_4fce4` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn special_state_4fce4(m: &mut Machine, slot: u32) -> u32 {
    crate::jp::res_p2_ai::special_state_4fce4(m, slot)
}

// JP-REVIEW(R0024): near function: PAL 8004FDB0 -> JP 800513D8, del:1 imm:1 op:1, 3 instructions differ (whole routine)
/// 800513D8: retire (+0x1C = 0xFF, returns 0) once the movement request
/// is 0xFF; else returns 1.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::check_finished_4fdb0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn check_finished_4fdb0(m: &mut Machine, slot: u32) -> u32 {
    crate::jp::res_p2_ai::check_finished_4fdb0(m, slot)
}

// ==== spawning, kill log, formations ==================================================

/// `[gp+0x3D4]`: spawn records (0x20 bytes).
const GP_SPAWNS: u32 = 0x3d4;
/// `[gp+0x378]`: group definitions (12 bytes: +2 member count, +4 slots).
const GP_GROUP_DEFS: u32 = 0x378;
/// `[gp+0x380]`: group states (12 bytes: +0 flags, +4 alive count, +6
/// leader slot, +7 break flag, +8 frames since the break).
const GP_GROUP_STATES: u32 = 0x380;
/// 800B1880: formation positions by member count (8 per count from 2).
const FORMATION_SLOTS: u32 = 0x800b_1880;
/// 800B17F0: formation offsets (VECTOR, 16 bytes).
const FORMATION_OFFSETS: u32 = 0x800b_17f0;
/// 80046928: advance `slot` to its next spawn entry (info +0x24 index into
/// the list at +0x29.., at most 7): type, leader, waypoint and timers from
/// the spawn record into the AI record. Returns 1 when the list is done.
pub fn next_spawn_45ce4(m: &mut Machine, slot: u32) -> u32 {
    let record = slot_record(m, slot);
    let info = slot_info(m, slot);
    let index = m.lw(info + 0x24);
    if !lt(index, 7) {
        return 1;
    }
    let index = index.wrapping_add(1);
    let entry = lbu(m, info.wrapping_add(index) + 0x28);
    if entry == 0xff {
        return 1;
    }
    let spawn = m.gw(GP_SPAWNS).wrapping_add(entry << 5);
    m.sw(info + 0x24, index);
    m.sw(info + 0x30, spawn);
    let kind = lbu(m, spawn);
    m.sb(record + 0x1d, 0);
    m.sb(record + 0x1e, 0);
    m.sb(record + 0x1c, kind as u8);
    let leader = lbu(m, spawn + 1);
    m.sb(record + 0x1f, leader as u8);
    copy_block(m, record + 0x20, spawn + 8, 4);
    let word = m.lw(spawn + 0x18);
    m.sw(record + 0x30, word);
    let word = m.lw(spawn + 0x1c);
    m.sw(record + 0x34, word);
    0
}

/// 800382D4: slot flags of `slot` without 1, 0x1000, 0x2000, 0x4000 and
/// 0x100000, plus 2 (destroyed).
pub fn clear_slot_flags_37fc4(m: &mut Machine, slot: u32) {
    let at = SLOT_FLAGS.wrapping_add(slot << 2);
    let flags = m.lw(at) & !1 & !0x2000 & !0x1000 & !0x4000 & 0xffef_ffff;
    m.sw(at, flags | 2);
}

/// 8004BD18 (count, index, out) -> offset address: the formation offset of
/// member `index` of a group of `count` (VECTOR copied to `out`).
pub fn formation_offset_4a5c8(m: &mut Machine, count: u32, index: u32, out: u32) -> u32 {
    let at = index
        .wrapping_add(count.wrapping_sub(2) << 3)
        .wrapping_add(FORMATION_SLOTS);
    let position = lbu(m, at);
    let source = FORMATION_OFFSETS.wrapping_add(position << 4);
    copy_block(m, out, source, 4);
    source
}

/// 8004BDA8 (group): disband: group flag 1 and the members' slot flag
/// 0x100000 cleared.
pub fn disband_group_4a658(m: &mut Machine, group: u32) {
    let offset = group.wrapping_mul(12);
    let state = m.gw(GP_GROUP_STATES).wrapping_add(offset);
    let definition = m.gw(GP_GROUP_DEFS).wrapping_add(offset);
    clear_word_bits(m, state, 1);
    let mut index = 0u32;
    while lt(index, lhu(m, definition + 2)) {
        let member = lbu(m, definition.wrapping_add(index) + 4);
        clear_word_bits(m, SLOT_FLAGS.wrapping_add(member << 2), 0x10_0000);
        index += 1;
    }
}

// JP-REVIEW(R0025): near function: PAL 8004A334 -> JP 8004BA7C, ins:2 op:1, 3 instructions differ (whole routine)
/// 8004BA7C (group): formation flight: the first present member leads
/// (slot flag 0x100000 on the others); disband below two; once the group
/// breaks (a member targeted by a missile, or leader object flag 0x20)
/// members peel off (action 12, movement state 23) and disband when done;
/// otherwise they hold their formation offsets (movement state 3).
pub fn update_group_4a334(m: &mut Machine, group: u32) {
    with_frame(m, 0x30, |m, frame| {
        let members = frame + 0x10;
        let definition = m.gw(GP_GROUP_DEFS).wrapping_add(group.wrapping_mul(12));
        let state = m.gw(GP_GROUP_STATES).wrapping_add(group.wrapping_mul(12));
        let mut alive = 0u32;
        let mut index = 0u32;
        while lt(index, lhu(m, definition + 2)) {
            let slot_at = definition.wrapping_add(index) + 4;
            let member = lbu(m, slot_at);
            let at = SLOT_FLAGS.wrapping_add(member << 2);
            let flags = m.lw(at);
            if flags & 1 != 0 {
                if alive == 0 {
                    m.sb(state + 6, member as u8);
                    let member = lbu(m, slot_at);
                    let at = SLOT_FLAGS.wrapping_add(member << 2);
                    let flags = m.lw(at) & 0xffef_ffff;
                    m.sw(at, flags);
                } else {
                    m.sw(at, flags | 0x10_0000);
                }
                let member = lbu(m, slot_at);
                m.sb(members.wrapping_add(alive), member as u8);
                alive += 1;
                if lbu(m, state + 7) != 1 {
                    let block = shots(m);
                    let member = lbu(m, slot_at);
                    if m.lw(block) & bit(member) != 0 {
                        m.sb(state + 7, 1);
                        m.sw(state + 8, 0);
                    }
                }
            }
            index += 1;
        }
        m.sh(state + 4, alive as u16);
        if alive & 0xffff < 2 {
            disband_group_4a658(m, group);
        }
        let leader = lbu(m, state + 6);
        let object = m.lw(SLOT_OBJECTS.wrapping_add(leader << 2));
        if object == 0 {
            return;
        }
        if lbu(m, state + 7) != 1 && m.lw(object) & 0x20 != 0 {
            m.sb(state + 7, 1);
            m.sw(state + 8, 0);
        }
        let mut index = 1u32;
        while lt(index, lhu(m, state + 4)) {
            let member = lbu(m, members.wrapping_add(index));
            let record = slot_record(m, member);
            if lbu(m, state + 7) == 1 {
                if m.lw(state + 8) == 0 {
                    let x = m.lw(record + 0x44);
                    m.sb(record + 0x63, 0xc);
                    m.sb(record + 0x3d, 0x17);
                    m.sb(record + 0x61, 0);
                    m.sh(record + 0x78, 0x1000);
                    m.sh(record + 0x7a, 0);
                    m.sh(record + 0x7c, sra(x, 7) as u16);
                }
                if m.lw(record) & 0x40_0000 != 0 {
                    disband_group_4a658(m, group);
                    m.sb(record + 0x3d, 0xff);
                    m.sb(record + 0x63, 0xff);
                }
            } else {
                m.sb(record + 0x3d, 3);
                let leader = lbu(m, state + 6);
                m.sb(record + 0x3f, leader as u8);
                let count = lhu(m, state + 4);
                formation_offset_4a5c8(m, count, index, record + 0x44);
            }
            index += 1;
        }
        let frames = m.lw(state + 8).wrapping_add(1);
        m.sw(state + 8, frames);
    });
}

// ==== weapons =========================================================================

// JP-REVIEW(R0026): near function: PAL 80046E0C -> JP 80047A54, del:1 imm:1, 2 instructions differ (whole routine)
/// 80047A54: for each slot requesting fire (slot flag 0x10000, cleared):
/// the gun (80047B00) and, when it does not fire, missiles (80047D68).
/// Stops at the first requesting slot without a target. Returns 0.
pub fn fire_control_46e0c(m: &mut Machine) -> u32 {
    with_frame(m, 0x20, |m, _| {
        for slot in 2..32u32 {
            let at = SLOT_FLAGS + (slot << 2);
            let record = slot_record(m, slot);
            let flags = m.lw(at);
            if flags & 0x1_0000 == 0 {
                continue;
            }
            m.sw(at, flags & 0xfffe_ffff);
            if lbu(m, record + 0x60) == 0xff {
                return 0;
            }
            if gun_46ebc(m, slot) == 1 {
                missiles_47124(m, slot);
            }
        }
        0
    })
}

/// 80047B00: fire the gun (info +0x20) within its range: bursts by
/// pattern on the burst counter +0x5C, aim by pilot skill against the
/// angle off +0xB4; pattern 5 also draws the muzzle flash (800831D0) and
/// sound 0xD. Returns 1 without a gun or out of range, else 0.
pub fn gun_46ebc(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x28, |m, _| {
        let mut votes = 0u32;
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        let info = slot_info(m, slot);
        let gun = m.lw(info + 0x20);
        if gun == 0 {
            return 1;
        }
        if m.lw(record + 0xa0) >= m.lw(gun + 4) {
            m.sh(record + 0x5c, 0);
            return 1;
        }
        let pilot = m.lw(info + 0x18);
        let aimed = match lbu(m, pilot) {
            0 => lhu(m, record + 0xb4).wrapping_sub(0x801) < 0x17ff,
            1 => lhu(m, record + 0xb4).wrapping_sub(0x101) < 0xeff,
            2 | 3 => lt(lh(m, record + 0xb4), 0x1000),
            _ => false,
        };
        if aimed {
            votes = 1;
        }
        let burst = match lhu(m, gun).wrapping_sub(1) {
            0 | 3 => Some((0x41, 0x40)),
            1 => Some((0x43, 0x40)),
            2 => Some((0x27, 0x20)),
            5 => Some((0x13, 0x10)),
            4 => {
                if m.lw(record + 0x5c) & 0x50 == 0x50 {
                    let counter = lhu(m, record + 0x5c);
                    if counter & 1 != 0 {
                        votes += 1;
                    }
                    gun_fire_81b88(m, mover + 8, mover + 0x18, (counter & 0xf) << 6, 0);
                    if lhu(m, record + 0x5c) & 0xf == 0 {
                        sound_at(m, 0xd, mover + 8, 0);
                    }
                }
                None
            }
            _ => None,
        };
        if let Some((mask, value)) = burst
            && m.lw(record + 0x5c) & mask == value
        {
            votes += 1;
        }
        if votes == 2 {
            let kind = lhu(m, gun).wrapping_sub(1);
            let word = m.lw(mover);
            original(m, 0x8006_b034, [word, mover + 8, mover + 0x38, kind]);
        }
        let counter = lhu(m, record + 0x5c).wrapping_add(1);
        m.sh(record + 0x5c, counter as u16);
        0
    })
}

// JP-REVIEW(R0028): changed function: PAL 80047124 -> JP 80047D68, call+:3 del:3 imm:17 ins:91 op:18 reg:46, 175 instructions differ (whole routine)
/// 80047D68: fire missiles (info +0x1C kind, range and angle limits) at the
/// target +0x60 every pilot interval (+0x16; wingmen by player count),
/// unless it is already targeted (shots block +0/+4 masks); in mission 0x17
/// only far out and high. Raises the missile alert (800C6ECC bit 0) while
/// the player is about to be fired on. Returns 0.
// JP-PATCH: replaced by `crate::jp::res_p2_ai::missiles_47124` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn missiles_47124(m: &mut Machine, slot: u32) -> u32 {
    crate::jp::res_p2_ai::missiles_47124(m, slot)
}

// ==== positional sound ================================================================

// ==== projectile behaviours (tables 800B2C80/800B2C94/800B2CA8) =======================

/// `[gp+0x50C]`: the shots block (damage queue mask at +0x33E0, 12-byte
/// entries from +0x33E4).
const GP_SHOTS: u32 = 0x50c;

/// Spin settling (flag 8): the spin rate +0x5C opposes the spin +0x30 at
/// up to 4 × +0x7C, or cancels it and ends (flag 8 cleared); then the spin
/// request (flag 0x2000).
fn settle_spin(m: &mut Machine, shot: u32) {
    if m.lw(shot) & 8 == 0 {
        return;
    }
    let limit = lh(m, shot + 0x7c) << 2;
    let spin = lh(m, shot + 0x30);
    let spin_bits = lhu(m, shot + 0x30);
    if lt(limit, abs(spin)) {
        let rate = if lt(spin, 0) {
            limit
        } else {
            limit.wrapping_neg()
        };
        m.sh(shot + 0x5c, rate as u16);
    } else {
        m.sh(shot + 0x5c, spin_bits.wrapping_neg() as u16);
        clear_word_bits(m, shot, 8);
    }
    set_word_bits(m, shot, 0x2000);
}

/// Phases 0/1 of the boosted behaviours: phase 0 advances to 1 at once;
/// returns whether the shot is boosting (phase 0 or 1).
fn boosting(m: &mut Machine, shot: u32) -> bool {
    match lh(m, shot + 8) {
        0 => {
            let phase = lhu(m, shot + 8).wrapping_add(1);
            m.sh(shot + 8, phase as u16);
            true
        }
        1 => true,
        _ => false,
    }
}

/// The end of the boost (age +0xA past +0x7E): phase 2 when guided (flag
/// 1), else the shot becomes class 1 with age -1. Returns whether it ended.
fn boost_ends(m: &mut Machine, shot: u32) -> bool {
    if !lt(lh(m, shot + 0x7e), lh(m, shot + 0xa)) {
        return false;
    }
    if m.lw(shot) & 1 != 0 {
        let phase = lhu(m, shot + 8).wrapping_add(1);
        m.sh(shot + 8, phase as u16);
    } else {
        m.sb(shot + 4, 1);
        m.sh(shot + 8, 0);
        m.sh(shot + 0xa, 0xffff);
    }
    true
}

/// 8006903C: class 0 kind 0, boosted rocket: speed +0x60 grows by +0x84
/// (+0x50 = +0x60 + +0x64, velocity request 0x4000) while boosting.
pub fn rocket_6786c(m: &mut Machine, shot: u32) {
    if boosting(m, shot) {
        boost_ends(m, shot);
        let speed = m.lw(shot + 0x60).wrapping_add(m.lw(shot + 0x84));
        let drift = m.lw(shot + 0x64);
        let word = m.lw(shot);
        m.sw(shot + 0x60, speed);
        m.sw(shot + 0x50, speed.wrapping_add(drift));
        m.sw(shot, word | 0x4000);
    }
    settle_spin(m, shot);
}

/// 80069158: class 0 kind 1, dropped rocket: falls (+0x4C += 0x29D) for 7
/// frames, then accelerates by +0x84 up to +0x80 while boosting.
pub fn drop_rocket_67988(m: &mut Machine, shot: u32) {
    if boosting(m, shot) {
        if boost_ends(m, shot) {
            m.sw(shot + 0x4c, 0);
        }
        if lt(lh(m, shot + 0xa), 7) {
            let fall = m.lw(shot + 0x4c).wrapping_add(0x29d);
            m.sw(shot + 0x4c, fall);
        } else {
            if lt(m.lw(shot + 0x50), m.lw(shot + 0x80)) {
                let speed = m.lw(shot + 0x60).wrapping_add(m.lw(shot + 0x84));
                m.sw(shot + 0x60, speed);
            }
            let speed = m.lw(shot + 0x60).wrapping_add(m.lw(shot + 0x64));
            m.sw(shot + 0x50, speed);
        }
        set_word_bits(m, shot, 0x4000);
    }
    settle_spin(m, shot);
}

/// 80069D18: class 0 kind 2, bomb: falls (+0x3C += 0x29D) and noses down
/// by 0x20 per frame to -0x800.
pub fn bomb_68548(m: &mut Machine, shot: u32) {
    match lh(m, shot + 8) {
        0 => {
            let phase = lhu(m, shot + 8).wrapping_add(1);
            m.sh(shot + 8, phase as u16);
        }
        1 => {}
        _ => return,
    }
    let fall = m.lw(shot + 0x3c).wrapping_add(0x29d);
    let pitch = lh(m, shot + 0x2c);
    m.sw(shot + 0x3c, fall);
    if lt(pitch, 0xffff_f801) {
        return;
    }
    let pitch = lhu(m, shot + 0x2c).wrapping_sub(0x20);
    m.sh(shot + 0x2c, pitch as u16);
}

/// 80069F48: class 0 kind 3, accelerating shot: speed +0x60 grows by +0x84
/// below +0x80.
pub fn accelerate_68778(m: &mut Machine, shot: u32) {
    match lh(m, shot + 8) {
        0 => {
            let phase = lhu(m, shot + 8).wrapping_add(1);
            m.sh(shot + 8, phase as u16);
        }
        1 => {}
        _ => return,
    }
    if lt(m.lw(shot + 0x50), m.lw(shot + 0x80)) {
        let speed = m.lw(shot + 0x60).wrapping_add(m.lw(shot + 0x84));
        m.sw(shot + 0x60, speed);
    }
    let speed = m.lw(shot + 0x60).wrapping_add(m.lw(shot + 0x64));
    let word = m.lw(shot) | 0x4000;
    m.sw(shot + 0x50, speed);
    m.sw(shot, word);
}

/// 80069D6C: class 0 kind 4: phase 0 -> 1.
pub fn arm_6859c(m: &mut Machine, shot: u32) {
    if lh(m, shot + 8) == 0 {
        let phase = lhu(m, shot + 8).wrapping_add(1);
        m.sh(shot + 8, phase as u16);
    }
}

/// 800319C8 (from, to, angles) -> distance: the turn toward `to` (VECTOR)
/// in the frame of `from` (position with pitch/yaw halfwords at +0x10):
/// `angles` receives pitch, yaw and roll (14-bit); the distance (0x100000
/// beyond reach) is negative when `to` lies behind.
pub fn aim_31784(m: &mut Machine, from: u32, to: u32, angles: u32) -> u32 {
    with_frame(m, 0x68, |m, frame| {
        let pitch = lhu(m, from + 0x10);
        m.sh(frame + 0x40, pitch as u16);
        let yaw = lhu(m, from + 0x12);
        m.sh(frame + 0x44, 0);
        m.sh(frame + 0x42, yaw as u16);
        shared::rotation_yxz_transposed(m, frame + 0x40, frame + 0x10);
        sdk::set_rot_matrix(m, frame + 0x10);
        let dx = sra(m.lw(from).wrapping_sub(m.lw(to)), 12);
        let dy = sra(m.lw(from + 4).wrapping_sub(m.lw(to + 4)), 12);
        let dz = sra(m.lw(from + 8).wrapping_sub(m.lw(to + 8)), 12);
        let spread = abs(dx).wrapping_add(abs(dy)).wrapping_add(abs(dz));
        let mut distance = 0x10_0000;
        if !lt(0xafff, spread) {
            distance = original(m, 0x8008_3f40, [dx, dy, dz, 0]);
        }
        m.sw(frame + 0x30, dx);
        m.sw(frame + 0x34, dy);
        m.sw(frame + 0x38, dz);
        sdk::apply_rot_matrix_lv(m, frame + 0x30, frame + 0x30);
        let x = m.lw(frame + 0x30);
        let y = m.lw(frame + 0x34);
        let z = m.lw(frame + 0x38);
        let flat = abs(x).wrapping_add(abs(z));
        let horizontal = if lt(0x5_7fff, flat) {
            0x10_0000
        } else {
            let shift = if lt(0xafff, flat) { 3 } else { 0 };
            let length = original(m, 0x8008_3f40, [sra(x, shift), 0, sra(z, shift), 0]);
            length << shift
        };
        let pitch = shared::atan2(m, y, horizontal);
        m.sh(angles, pitch as u16);
        let yaw = shared::atan2(m, x, z.wrapping_neg());
        m.sh(angles + 2, yaw.wrapping_neg() as u16);
        let roll = shared::atan2(m, x, y);
        let behind = !lt(abs(lh(m, angles + 2)), 0x1001);
        m.sh(angles + 4, roll.wrapping_neg() as u16);
        if behind {
            distance = distance.wrapping_neg();
        }
        distance
    })
}

/// The guidance of 800692C4/800694CC/80069A9C: aim (800319C8) at the
/// target (+0x74) led by its velocity (+0x78) over the time to impact
/// (none with flag 0x100 when `unled`), turn requests +0x58/+0x5A limited
/// to ±+0x7C, accelerate by +0x88 below +0x80 (requests 0x5000).
fn home(m: &mut Machine, shot: u32, unled: bool) {
    with_frame(m, 0x28, |m, frame| {
        let speed = m.lw(shot + 0x50);
        let mut lead = 1;
        if speed != 0 {
            lead = div(m.lw(shot + 0x68) << 12, speed);
        }
        if unled && m.lw(shot) & 0x100 != 0 {
            lead = 0;
        }
        let velocity = m.lw(shot + 0x78);
        let dx = lo(m.lw(velocity), lead);
        let dy = lo(m.lw(velocity + 4), lead);
        let target = m.lw(shot + 0x74);
        let x = m.lw(target).wrapping_add(dx);
        m.sw(frame + 0x10, x);
        let dz = lo(m.lw(velocity + 8), lead);
        let target = m.lw(shot + 0x74);
        let y = m.lw(target + 4).wrapping_add(dy);
        m.sw(frame + 0x14, y);
        let target = m.lw(shot + 0x74);
        let z = m.lw(target + 8).wrapping_add(dz);
        m.sw(frame + 0x18, z);
        let distance = aim_31784(m, shot + 0x1c, frame + 0x10, shot + 0x58);
        let limit = lh(m, shot + 0x7c);
        let clamp = |value: u32, bits: u32| {
            if lt(value, limit.wrapping_neg()) {
                limit.wrapping_neg()
            } else if lt(limit, value) {
                limit
            } else {
                bits
            }
        };
        let pitch = lh(m, shot + 0x58);
        let pitch_bits = lhu(m, shot + 0x58);
        m.sw(shot + 0x68, distance);
        m.sh(shot + 0x5c, 0);
        m.sh(shot + 0x58, clamp(pitch, pitch_bits) as u16);
        let yaw = lh(m, shot + 0x5a);
        let yaw_bits = lhu(m, shot + 0x5a);
        m.sh(shot + 0x5a, clamp(yaw, yaw_bits) as u16);
        if lt(m.lw(shot + 0x50), m.lw(shot + 0x80)) {
            let speed = m.lw(shot + 0x60).wrapping_add(m.lw(shot + 0x88));
            m.sw(shot + 0x60, speed);
        }
        let speed = m.lw(shot + 0x60).wrapping_add(m.lw(shot + 0x64));
        let word = m.lw(shot) | 0x5000;
        m.sw(shot, word);
        m.sw(shot + 0x50, speed);
        settle_spin(m, shot);
    });
}

/// 800692C4: class 1 kind 1, homing missile.
pub fn homing_67af4(m: &mut Machine, shot: u32) {
    home(m, shot, true);
}

/// 800694CC: class 1 kind 0, homing missile.
pub fn homing_67cfc(m: &mut Machine, shot: u32) {
    home(m, shot, true);
}

/// 80069A9C: class 1 kind 4, homing missile always leading the target.
pub fn homing_682cc(m: &mut Machine, shot: u32) {
    home(m, shot, false);
}

/// The terrain column index of a coordinate (0..0xFF, wrapping into the
/// last eight columns at the edges).
fn terrain_column(value: u32) -> u32 {
    let column = sra(value, 23).wrapping_add(0x80);
    let column = if lt(column, 0) { column & 7 } else { column };
    if lt(column, 0x100) {
        column
    } else {
        (column & 7) + 0xf8
    }
}

/// 800696D4: class 1 kind 2, terrain-following missile: homing, then
/// (beyond 0x1000 and nearly level) a climb request +0x58 from the
/// clearance over the terrain cell 20 frames ahead, within ±16 × +0x7C.
pub fn sea_skimmer_67f04(m: &mut Machine, shot: u32) {
    with_frame(m, 0x18, |m, _| {
        homing_67cfc(m, shot);
        if lt(m.lw(shot + 0x68), 0x1001) || !lt(abs(lh(m, shot + 0x30)), 0xe3) {
            return;
        }
        let ahead = |m: &mut Machine, axis: u32| {
            let velocity = m.lw(shot + 0x38 + axis);
            velocity
                .wrapping_mul(20)
                .wrapping_add(m.lw(shot + 0x1c + axis))
        };
        let mut x = ahead(m, 0);
        let mut y = ahead(m, 4);
        let gameplay = m.gw(common::GAMEPLAY);
        let terrain = gameplay + 0x244;
        let cells = m.lw(terrain + 4);
        m.sh(shot + 0x58, 0);
        let mut z = ahead(m, 8);
        let bounds = gameplay + 0x2a0;
        for (value, low) in [(&mut x, 0u32), (&mut y, 8), (&mut z, 0x10)] {
            let minimum = m.lw(bounds + low);
            if lt(*value, minimum) {
                *value = minimum;
            }
            let maximum = m.lw(bounds + low + 4);
            if lt(maximum, *value) {
                *value = maximum;
            }
        }
        let index = terrain_column(x).wrapping_add(terrain_column(z) << 8);
        let cell = lhu(m, cells.wrapping_add(index << 1)) & 0x1ff;
        if cell == 0 {
            return;
        }
        let objects = m.lw(terrain + 8);
        let limit = lh(m, shot + 0x7c);
        let object = m.lw(objects.wrapping_add(cell << 2));
        let clearance = sra(y, 12)
            .wrapping_add(0x190)
            .wrapping_sub(lh(m, object + 0x16));
        let product = lo(clearance, limit << 2);
        let climb = sra(hi(product, 0x51eb_851f), 6).wrapping_sub(sra(product, 31));
        m.sh(shot + 0x58, climb as u16);
        let floor = (limit << 2).wrapping_neg() << 2;
        if lt(sext16(climb), floor) {
            m.sh(shot + 0x58, floor as u16);
        }
        let ceiling = limit << 4;
        if lt(ceiling, lh(m, shot + 0x58)) {
            m.sh(shot + 0x58, ceiling as u16);
        }
    });
}

/// 80069900: class 1 kind 3, cluster missile: homing until within +0x98,
/// then four submunitions (80068E28) in a cross (+0x70 flag 0x20) or a
/// spread pattern, and expiry (+0x18 = 0).
pub fn cluster_68130(m: &mut Machine, shot: u32) {
    with_frame(m, 0x38, |m, frame| {
        homing_67cfc(m, shot);
        if !lt(m.lw(shot + 0x68), m.lw(shot + 0x98)) {
            return;
        }
        for offset in [0u32, 4, 8] {
            let word = m.lw(shot + 0x1c + offset);
            m.sw(frame + 0x10 + offset, word);
        }
        let pitch = lhu(m, shot + 0x2c);
        m.sh(frame + 0x20, pitch as u16);
        let yaw = lhu(m, shot + 0x2e);
        m.sh(frame + 0x22, yaw as u16);
        let roll = lhu(m, shot + 0x30);
        m.sh(frame + 0x24, roll as u16);
        let spawn = |m: &mut Machine| {
            let owner = m.lw(shot + 0x6c);
            let target = m.lw(shot + 0x70);
            submunition_67658(m, [owner, frame + 0x10, shot + 0x38, target]);
        };
        let turn = |m: &mut Machine, offset: u32, delta: u32| {
            let value = lhu(m, frame + offset).wrapping_add(delta);
            m.sh(frame + offset, value as u16);
        };
        if m.lw(shot + 0x70) & 0x20 != 0 {
            m.sh(frame + 0x20, pitch.wrapping_add(0x2aa) as u16);
            m.sh(frame + 0x22, yaw.wrapping_add(0x2aa) as u16);
            spawn(m);
            turn(m, 0x22, 0xffff_faab);
            spawn(m);
            turn(m, 0x20, 0xffff_fd56);
            spawn(m);
            turn(m, 0x22, 0x555);
        } else {
            m.sh(frame + 0x24, 0);
            spawn(m);
            turn(m, 0x20, 0x471);
            spawn(m);
            turn(m, 0x20, 0xffff_f71d);
            turn(m, 0x22, 0x800);
            spawn(m);
            turn(m, 0x22, 0xffff_f000);
        }
        spawn(m);
        m.sh(shot + 0x18, 0);
    });
}

/// 80068E28 (owner, source, velocity, flags) -> projectile or 0: a
/// submunition (class 2 kind 4 with trails and a random spin), with the
/// end burst for flags 0x20.
pub fn submunition_67658(m: &mut Machine, args: [u32; 4]) -> u32 {
    with_frame(m, 0x20, |m, _| {
        let flags = args[3];
        let shot = original(m, 0x8006_85c0, args);
        if shot == 0 {
            return 0;
        }
        let bits = lbu(m, shot + 0x35);
        m.sh(shot + 0x1a, 0x100);
        m.sb(shot + 0x34, 2);
        m.sb(shot + 6, 4);
        m.sb(shot + 7, 0);
        m.sb(shot + 5, 0);
        m.sb(shot + 0x35, (bits | 0x18) as u8);
        let random = sdk::rand(m);
        m.sh(shot + 0x7e, (random & 3) as u16);
        let random = sdk::rand(m);
        let spin = lhu(m, shot + 0x30)
            .wrapping_sub(0x200)
            .wrapping_add(random & 0x3ff);
        m.sh(shot + 0x30, spin as u16);
        if flags & 0x20 != 0 {
            m.sh(shot + 0x18, 0x96);
            m.sh(shot + 0x7c, 0x18);
            m.sw(shot + 0x84, 0x1000);
            m.sw(shot + 0x88, 0x1000);
            m.sw(shot + 0x9c, 0x280);
            let word = m.lw(shot);
            m.sw(shot + 0x80, 0x3_c000);
            m.sw(shot + 0xa0, 0xc0);
            m.sw(shot, word | 0x10);
        } else {
            m.sh(shot + 0x18, 0xf0);
            m.sh(shot + 0x7c, 0x30);
            m.sw(shot + 0x84, 0x200);
            m.sw(shot + 0x80, 0x6_e000);
            m.sw(shot + 0x88, 0xc00);
        }
        shot
    })
}

/// The impact sound 0x1C of a miss on the player or a wingman (target
/// +0x70 below 2).
fn miss_sound(m: &mut Machine, shot: u32) {
    if lt(m.lw(shot + 0x70) & 0x3f, 2) {
        sound_at(m, 0x1c, shot + 0x1c, 0);
    }
}

/// 80069C8C: class 2 kind 0, impact: a hit (flag 4) bursts (800809F0)
/// with sound 0x1D, a miss sparks (8008074C).
pub fn impact_684bc(m: &mut Machine, shot: u32) {
    with_frame(m, 0x20, |m, _| {
        let point = shot + 0x1c;
        if m.lw(shot) & 4 != 0 {
            impact_burst_7f3a8(m, point, shot + 0x8c, shot + 0x10, 0);
            sound_at(m, 0x1d, point, 0);
        } else {
            impact_sparks_7f104(m, point, shot + 0x8c, 0);
            miss_sound(m, shot);
        }
    });
}

/// 80069D88: class 2 kind 1, splash (80080F10 variant 0); sound 0x25 on a
/// hit.
pub fn splash_685b8(m: &mut Machine, shot: u32) {
    with_frame(m, 0x20, |m, _| {
        let point = shot + 0x1c;
        let hit = m.lw(shot) & 4 != 0;
        splash_7f8c8(m, point, 0);
        if hit {
            sound_at(m, 0x25, point, 0);
        } else {
            miss_sound(m, shot);
        }
    });
}

/// 80069E10: class 2 kind 2, burst at the target (+0x70) unless guided
/// (flag 1): the shot takes its position and attitude.
pub fn burst_on_target_68640(m: &mut Machine, shot: u32) {
    with_frame(m, 0x18, |m, _| {
        if m.lw(shot) & 1 != 0 {
            return;
        }
        let index = m.lw(shot + 0x70) & 0x3f;
        let target = entity(m, index);
        for offset in [0u32, 4, 8] {
            let word = m.lw(target + 8 + offset);
            m.sw(shot + 0x1c + offset, word);
        }
        for offset in [0u32, 2, 4] {
            let angle = lhu(m, target + 0x18 + offset);
            m.sh(shot + 0x2c + offset, angle as u16);
        }
        impact_burst_7f3a8(m, shot + 0x1c, shot + 0x8c, shot + 0x10, 0);
        sound_at(m, 0x1d, shot + 0x1c, 0);
    });
}

/// 80069EC0: class 2 kind 3, large splash (80080F10 variant 1); sound 0x1D
/// on a hit.
pub fn splash_686f0(m: &mut Machine, shot: u32) {
    with_frame(m, 0x20, |m, _| {
        let point = shot + 0x1c;
        let hit = m.lw(shot) & 4 != 0;
        splash_7f8c8(m, point, 1);
        if hit {
            sound_at(m, 0x1d, point, 0);
        } else {
            miss_sound(m, shot);
        }
    });
}

/// 80068454 (owner, target, damage, falloff) -> entry or 0: queue a hit
/// in the first free of the 32 damage entries (shots block +0x33E4).
pub fn queue_damage_66c84(
    m: &mut Machine,
    owner: u32,
    target: u32,
    damage: u32,
    falloff: u32,
) -> u32 {
    let block = m.gw(GP_SHOTS);
    let mask = m.lw(block + 0x33e0);
    if mask == u32::MAX {
        return 0;
    }
    let mut index = 0;
    while mask & bit(index) != 0 {
        index += 1;
    }
    let block = m.gw(GP_SHOTS);
    let entry = block.wrapping_add(index * 12 + 0x33e4);
    m.sw(block + 0x33e0, mask | bit(index));
    m.sh(entry, falloff as u16);
    m.sh(entry + 2, damage as u16);
    m.sw(entry + 4, target);
    m.sw(entry + 8, owner);
    entry
}

/// 8006A978 (shot, radius): blast damage to every present hostile entity
/// (flags 0x4200, another side than the owner +0x6C) within `radius`,
/// falling off by distance / +0xA0.
pub fn blast_691a8(m: &mut Machine, shot: u32, radius: u32) {
    with_frame(m, 0x38, |m, _| {
        let reach = radius.wrapping_mul(3);
        let x = m.lw(shot + 0x1c);
        let y = m.lw(shot + 0x20);
        let side = m.lw(shot + 0x6c) & 0x1c0;
        let z = m.lw(shot + 0x24);
        for index in 0..0x40u32 {
            let target = entity(m, index);
            let word = m.lw(target);
            if word & 0x4200 != 0x4200 || word & side != 0 {
                continue;
            }
            let dx = abs(sra(m.lw(target + 8).wrapping_sub(x), 12));
            let dy = abs(sra(m.lw(target + 0xc).wrapping_sub(y), 12));
            let dz = abs(sra(m.lw(target + 0x10).wrapping_sub(z), 12));
            let size = lh(m, target + 0x4a);
            let spread = dx.wrapping_add(dy).wrapping_add(dz).wrapping_add(size);
            if lt(reach, spread) {
                continue;
            }
            let distance = shared::vector_distance(m, target + 8, shot + 0x1c);
            if lt(radius, distance) {
                continue;
            }
            let falloff = div(distance, m.lw(shot + 0xa0));
            let owner = m.lw(shot + 0x6c);
            let damage = lhu(m, shot + 0x1a);
            queue_damage_66c84(m, owner, word, damage, falloff);
        }
    });
}

/// 8006AAFC: direct-hit damage on the target (+0x70) unless guided (flag
/// 1), present and hostile; falloff +0x9C / +0xA0.
pub fn direct_hit_6932c(m: &mut Machine, shot: u32) {
    with_frame(m, 0x18, |m, _| {
        if m.lw(shot) & 1 != 0 {
            return;
        }
        let index = m.lw(shot + 0x70) & 0x3f;
        let target = entity(m, index);
        let word = m.lw(target);
        if word & 0x4200 != 0x4200 {
            return;
        }
        let owner = m.lw(shot + 0x6c);
        if word & owner & 0x1c0 != 0 {
            return;
        }
        let falloff = div(m.lw(shot + 0x9c), m.lw(shot + 0xa0));
        let damage = lhu(m, shot + 0x1a);
        queue_damage_66c84(m, owner, word, damage, falloff);
    });
}

// ==== effect spawners =================================================================

/// `[gp+0x41C]`: the effect block (particle pools at +0x600 flashes,
/// +0x1098 smoke and debris, +0x33C0 shock waves; tracers at +0x3964).
const GP_EFFECTS: u32 = 0x41c;
/// `[gp+0x33C]`: the water surface (+4 height).
const GP_WATER: u32 = 0x33c;

/// 8007FDB4 (`replay`): a particle from the pool at effect block +`pool`,
/// or 0.
fn particle(m: &mut Machine, pool: u32, count: u32, kind: u32) -> u32 {
    let block = m.gw(GP_EFFECTS);
    original(m, 0x8007_fdb4, [block.wrapping_add(pool), count, kind, 0])
}

/// The variant field (bits 4..11 of word +8) of an effect command.
fn set_variant(m: &mut Machine, command: u32, variant: u32) -> u32 {
    let word = (m.lw(command + 8) & 0xffff_f00f) | ((variant & 0xff) << 4);
    m.sw(command + 8, word);
    word
}

/// A random byte spread: `((value & mask) - centre) << shift`.
fn spread(value: u32, mask: u32, centre: u32, shift: u32) -> u32 {
    (value & mask).wrapping_sub(centre) << shift
}

/// 8007FE28 (point, variant): explosion particles (variants below 6): a
/// flash, a fireball cloud (5 puffs for variant 2, else 2) and for
/// variants 3-5 a rising smoke column.
pub fn explosion_particles_7e7e0(m: &mut Machine, point: u32, variant: u32) {
    with_frame(m, 0x30, |m, _| {
        if variant >= 6 {
            return;
        }
        let x = m.lw(point);
        let y = m.lw(point + 4);
        let z = m.lw(point + 8);
        let flash = particle(m, 0x600, 3, 7);
        if flash != 0 {
            m.sw(flash, 0x8_ffff);
            m.sw(flash + 4, x);
            m.sw(flash + 8, y);
            m.sw(flash + 0xc, z);
            m.sh(flash + 0x16, 0);
            let base: u32 = if variant == 2 { 0x64 } else { 0x32 };
            m.sh(flash + 0x18, 0x100);
            let random = sdk::rand(m);
            m.sh(flash + 0x1a, base.wrapping_add(random & 0x1f) as u16);
        }
        let mut count: u32 = if variant == 2 { 5 } else { 2 };
        while lt(0, count) {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                break;
            }
            m.sh(puff, 0);
            m.sh(puff + 2, count.wrapping_add(0x1d) as u16);
            let random = sdk::rand(m);
            let mixed = sra(random, 8) | (random << 7);
            let px = x.wrapping_add(spread(random, 0x7f, 0x40, 10));
            let py = y.wrapping_add(spread(sra(random, 3), 0x7f, 0x40, 10));
            let pz = z.wrapping_add(spread(sra(random, 6), 0x7f, 0x40, 10));
            m.sh(puff + 0x10, spread(mixed, 0x7f, 0x40, 0) as u16);
            m.sh(puff + 0x12, spread(sra(mixed, 3), 0x7f, 0x50, 0) as u16);
            m.sh(puff + 0x14, spread(sra(mixed, 6), 0x7f, 0x40, 0) as u16);
            m.sw(puff + 4, px);
            m.sw(puff + 8, py);
            m.sw(puff + 0xc, pz);
            let size = if count == 1 {
                m.sh(puff + 0x18, 0x1604);
                m.sh(puff + 0x1a, ((mixed & 0x3f) + 0x6e) as u16);
                m.sh(puff + 0x16, 0);
                0x91
            } else {
                let life = count.wrapping_add(mixed & 3).wrapping_add(0xb);
                m.sh(puff + 0x1a, life as u16);
                m.sh(puff + 0x18, 0);
                0x188
            };
            count = count.wrapping_sub(1);
            m.sh(puff + 0x16, size);
        }
        if variant < 3 {
            return;
        }
        let column = particle(m, 0x1098, 10, 7);
        if column == 0 {
            return;
        }
        let (word, scale, life) = match variant {
            3 => (0xe_0001, 0x801, 0x90),
            4 => (0x12_0001, 0xc00, 0xb8),
            _ => (0x12_0001, 0xc03, 0xb8),
        };
        m.sw(column, word);
        m.sh(column + 0x18, scale);
        let random = sdk::rand(m);
        m.sh(column + 0x1a, ((random & 0x1f) + life) as u16);
        m.sw(column + 4, x);
        m.sw(column + 8, y);
        m.sw(column + 0xc, z);
        let random = sdk::rand(m);
        m.sh(column + 0x10, spread(random, 0x3f, 0x20, 0) as u16);
        m.sh(column + 0x12, spread(sra(random, 3), 0x3f, 0x84, 0) as u16);
        m.sh(column + 0x14, spread(sra(random, 6), 0x3f, 0x20, 0) as u16);
        m.sh(column + 0x16, 0x90);
    });
}

/// 800800E0 (point, variant): explosion: effect command 0 with the
/// variant, and its particles (8007FE28).
pub fn explosion_7ea98(m: &mut Machine, point: u32, variant: u32) {
    with_frame(m, 0x20, |m, _| {
        let command = point_command(m, 0, point);
        set_variant(m, command, variant);
        explosion_particles_7e7e0(m, point, variant);
    });
}

/// A flash particle: kind word, no velocity, size word +0x18.
fn set_flash(m: &mut Machine, flash: u32, word: u32, size: u32) {
    m.sw(flash, word);
    m.sh(flash + 0x16, 0);
    m.sw(flash + 0x18, size);
}

/// 80080140 (point, face, variant): debris particles (variants below 6):
/// variants 3-5 one rising smoke puff; variant 2 a flash (face 1: large)
/// with four debris chunks; variants 0/1 a flash sized by `face`.
pub fn debris_particles_7eaf8(m: &mut Machine, point: u32, face: u32, variant: u32) {
    with_frame(m, 0x30, |m, _| {
        if variant >= 6 {
            return;
        }
        let x = m.lw(point);
        let y = m.lw(point + 4);
        let z = m.lw(point + 8);
        if variant >= 3 {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                return;
            }
            m.sh(puff + 0x16, 0x90);
            m.sw(puff + 4, x);
            m.sw(puff + 8, y);
            m.sw(puff + 0xc, z);
            m.sh(puff + 0x14, 0);
            let (word, rise, scale, life) = match variant {
                3 => (0x10_0000, 0xff80_0000, 0x601, 0xd0),
                4 => (0x14_0000, 0xff60_0000, 0x800, 0x10c),
                _ => (0x14_0000, 0xff60_0000, 0x803, 0x10c),
            };
            m.sw(puff, word);
            m.sw(puff + 0x10, rise);
            m.sh(puff + 0x18, scale);
            let random = sdk::rand(m);
            m.sh(puff + 0x1a, ((random & 0x3f) + life) as u16);
            return;
        }
        // The original stores without checking the allocation.
        let flash = particle(m, 0x600, 3, 7);
        m.sw(flash.wrapping_add(4), x);
        m.sw(flash.wrapping_add(8), y);
        m.sw(flash.wrapping_add(0xc), z);
        if variant == 2 {
            if face == 1 {
                set_flash(m, flash, 0xe_ffff, 0x64_0120);
                return;
            }
            m.sw(flash, 0xb_ffff);
            m.sh(flash + 0x16, 0);
            m.sh(flash + 0x18, 0x100);
            let random = sdk::rand(m);
            m.sh(flash + 0x1a, ((random & 0x7f) + 0x78) as u16);
            for index in 0..4u32 {
                let chunk = particle(m, 0x1098, 10, 7);
                if chunk == 0 {
                    return;
                }
                m.sh(chunk, 0);
                m.sh(chunk + 2, (index + 0x10) as u16);
                let random = sdk::rand(m);
                let mixed = sra(random, 8) | (random << 7);
                let cx = x.wrapping_add(spread(random, 0xff, 0x80, 8));
                let cy =
                    y.wrapping_add(spread(sra(random, 3), 0xff, 0x80, 8).wrapping_add(0xfffd_8000));
                let cz = z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 8));
                m.sh(chunk + 0x10, spread(mixed, 0xff, 0x80, 2) as u16);
                m.sh(chunk + 0x12, spread(sra(mixed, 1), 0x3fc, 0x280, 0) as u16);
                m.sh(chunk + 0x14, spread(sra(mixed, 6), 0xff, 0x80, 2) as u16);
                m.sh(chunk + 0x16, 0x18a);
                m.sw(chunk + 4, cx);
                m.sw(chunk + 8, cy);
                m.sw(chunk + 0xc, cz);
                m.sh(chunk + 0x18, 0);
                m.sh(chunk + 0x1a, ((index & 3) + 0xe) as u16);
            }
            return;
        }
        match face {
            1 => set_flash(m, flash, 0xc_ffff, 0x50_0120),
            2 => set_flash(m, flash, 0xa_ffff, 0x50_0121),
            3 => set_flash(m, flash, 0xa_ffff, 0x50_0122),
            4 => {
                m.sw(flash, 0xc_ffff);
                let random = sdk::rand(m);
                m.sh(flash + 0x10, spread(random, 0x3f, 0x20, 0) as u16);
                let rise = spread(sra(random, 3), 0x3f, 0x20, 0);
                m.sh(flash + 0x14, spread(sra(random, 6), 0x3f, 0x20, 0) as u16);
                m.sh(flash + 0x12, rise as u16);
                m.sh(flash + 0x16, 0x88);
                m.sw(flash + 0x18, 0x64_0123);
            }
            5 => set_flash(m, flash, 0xa_ffff, 0x50_0124),
            _ => set_flash(m, flash, 0xa_ffff, 0x50_0000),
        }
    });
}

/// 800804B8 (point, face, variant): debris: effect command 1 with the
/// variant and face (low four bits), and its particles (80080140).
pub fn debris_7ee70(m: &mut Machine, point: u32, face: u32, variant: u32) {
    with_frame(m, 0x20, |m, _| {
        let face = face & 0xf;
        let command = point_command(m, 1, point);
        let word = (m.lw(command + 8) & 0xffff_f00f) | ((variant & 0xff) << 4);
        m.sw(command + 8, (word & 0xffff_fff0) | face);
        debris_particles_7eaf8(m, point, face, variant);
    });
}

/// 8007EEAC (kind, point, velocity) -> command: a five-word effect command
/// (type 5, flags 0x30) of `kind` with the position (>> 12, 20 bits) and
/// velocity (>> 12, high 12 bits) packed into words +4..+0xC.
pub fn vector_command_7d830(m: &mut Machine, kind: u32, point: u32, velocity: u32) -> u32 {
    with_frame(m, 0x20, |m, _| {
        let command = original(m, 0x8007_df28, [5, 0, 0, 0]);
        let head = m.lw(command);
        let word_x = m.lw(command + 4) & 0xfff0_0000;
        let word_y = m.lw(command + 8) & 0xfff0_0000;
        let word_z = m.lw(command + 0xc) & 0xfff0_0000;
        let head = ((((head & 0xffff_fff0) | 5) & 0xffff_ff8f) | 0x30) & 0xffff_fe7f;
        m.sw(command, head);
        m.sb(command + 3, kind as u8);
        let word_x = word_x | (m.lw(point) >> 12);
        let word_y = word_y | (m.lw(point + 4) >> 12);
        let word_z = word_z | (m.lw(point + 8) >> 12);
        m.sw(command + 4, word_x);
        m.sw(command + 0xc, word_z);
        m.sw(command + 8, word_y);
        let word_x = (word_x & 0xf_ffff) | (sra(m.lw(velocity), 12) << 20);
        let word_y = (word_y & 0xf_ffff) | (sra(m.lw(velocity + 4), 12) << 20);
        let high_z = sra(m.lw(velocity + 8), 12) << 20;
        let word_z = (m.lw(command + 0xc) & 0xf_ffff) | high_z;
        m.sw(command + 4, word_x);
        m.sw(command + 8, word_y);
        m.sw(command + 0xc, word_z);
        command
    })
}

/// 80089E48 (direction: SVECTOR) -> `yaw << 16 | pitch` (12-bit angles
/// through `ratan2`; straight up or down for a vertical direction).
pub fn direction_angles_886d8(m: &mut Machine, direction: u32) -> u32 {
    with_frame(m, 0x20, |m, _| {
        let x = lh(m, direction);
        let z = lh(m, direction + 4);
        let y = lh(m, direction + 2);
        let (yaw, pitch) = if x == 0 && z == 0 {
            (0, if lt(0, y) { 0xffff_fc00 } else { 0x400 })
        } else {
            m.gte.mtc2(reg::IR1, x);
            m.gte.mtc2(reg::IR2, z);
            let yaw = sdk::ratan2(m, x, z);
            m.gte.sqr(false, true);
            let a = m.gte.mfc2(reg::MAC1);
            let b = m.gte.mfc2(reg::MAC2);
            let flat = shared::square_root(m, a.wrapping_add(b));
            (yaw, sdk::ratan2(m, y.wrapping_neg(), flat))
        };
        (yaw << 16) | (pitch & 0xffff)
    })
}

/// 800809F0 (point, velocity, direction, kind): impact burst: effect
/// command 0 (8007EEAC) with `kind` and the direction angles, and its
/// particles (800807A4).
pub fn impact_burst_7f3a8(m: &mut Machine, point: u32, velocity: u32, direction: u32, kind: u32) {
    with_frame(m, 0x28, |m, _| {
        let angles = direction_angles_886d8(m, direction);
        let command = vector_command_7d830(m, 0, point, velocity);
        let word = m.lw(command + 0x10);
        m.sb(command + 2, kind as u8);
        let word = (word & 0xffff_f000) | (angles & 0xfff);
        let word = (word & 0xff00_0fff) | ((sra(angles, 16) & 0xfff) << 12);
        m.sw(command + 0x10, word);
        original(m, 0x8008_07a4, [point, velocity, angles, word]);
    });
}

/// 8008074C (point, velocity, kind): impact sparks: effect command 0
/// (8007ED9C) with `kind`, and its particles (80080534).
pub fn impact_sparks_7f104(m: &mut Machine, point: u32, velocity: u32, kind: u32) {
    with_frame(m, 0x20, |m, _| {
        let command = original(m, 0x8007_ed9c, [0, point, velocity, 0]);
        m.sb(command + 2, kind as u8);
        original(m, 0x8008_0534, [point, velocity, 0, 0]);
    });
}

/// 80080F10 (point, variant): water splash: effect command 5 with the
/// variant, and its particles (80080EB4).
pub fn splash_7f8c8(m: &mut Machine, point: u32, variant: u32) {
    with_frame(m, 0x20, |m, _| {
        let command = point_command(m, 5, point);
        let word = set_variant(m, command, variant);
        original(m, 0x8008_0eb4, [point, variant, word, 0]);
    });
}

/// 80080F70 (point, velocity, large): explosion particles: the large
/// variant (8008 7D94) or a flash, four sparks, a shock wave and 16 debris
/// puffs thrown along the velocity.
pub fn explosion_particles_7f928(m: &mut Machine, point: u32, velocity: u32, large: u32) {
    with_frame(m, 0x38, |m, _| {
        let x = m.lw(point);
        let y = m.lw(point + 4);
        let z = m.lw(point + 8);
        if large == 1 {
            original(m, 0x8008_9504, [x, y, z, 0]);
            return;
        }
        let flash = particle(m, 0x600, 3, 7);
        if flash != 0 {
            let random = sdk::rand(m);
            m.sw(flash, 0x1c_ffff);
            m.sw(flash + 4, x);
            m.sw(flash + 8, y);
            m.sw(flash + 0xc, z);
            m.sh(flash + 0x16, 0);
            m.sh(flash + 0x18, 0x210);
            m.sh(flash + 0x1a, ((random & 0x3f) + 0x24c) as u16);
        }
        for index in 0..4u32 {
            let spark = particle(m, 0x600, 3, 7);
            if spark == 0 {
                break;
            }
            let random = sdk::rand(m);
            m.sh(
                spark,
                (random & 1).wrapping_neg().wrapping_sub(index << 1) as u16,
            );
            m.sh(spark + 2, ((sra(random, 1) & 3) + 0xf) as u16);
            m.sw(spark + 4, x.wrapping_add(spread(random, 0xff, 0x80, 12)));
            m.sw(
                spark + 8,
                y.wrapping_add(spread(sra(random, 3), 0xff, 0x80, 12)),
            );
            m.sw(
                spark + 0xc,
                z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 12)),
            );
            m.sh(spark + 0x18, 0x100);
            let life = (sra(random, 7) & 0x7f) + 0x12c - (index << 5);
            m.sh(spark + 0x1a, life as u16);
            m.sh(spark + 0x16, 0);
        }
        let wave = particle(m, 0x33c0, 1, 6);
        if wave != 0 {
            let random = sdk::rand(m);
            m.sh(wave, 0);
            m.sh(wave + 2, ((random & 3) + 0x10) as u16);
            m.sw(wave + 4, x);
            m.sw(wave + 8, y);
            m.sw(wave + 0xc, z);
            m.sh(wave + 0x14, 0);
            m.sh(wave + 0x16, ((random & 0xff) + 0x640) as u16);
        }
        let throw_x = sra(m.lw(velocity), 9);
        let throw_y = sra(m.lw(velocity + 4), 9);
        let throw_z = sra(m.lw(velocity + 8), 9).wrapping_sub(0x80);
        for index in 0..0x10u32 {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                return;
            }
            m.sh(puff, 0);
            m.sh(puff + 2, (index + 0x1c) as u16);
            let random = sdk::rand(m);
            let mixed = sra(random, 8) | (random << 7);
            let px = x.wrapping_add(spread(random, 0xff, 0x80, 9));
            let py =
                y.wrapping_add(spread(sra(random, 3), 0xff, 0x80, 9).wrapping_add(0xfffd_8000));
            let pz = z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 9));
            m.sw(puff + 4, px);
            let vx = throw_x.wrapping_sub(0x80).wrapping_add(mixed & 0xff);
            m.sh(puff + 0x10, vx as u16);
            let vy = throw_y.wrapping_add((sra(mixed, 3) & 0xff).wrapping_sub(0xc0));
            m.sh(puff + 0x12, vy as u16);
            let vz = throw_z.wrapping_add(sra(mixed, 6) & 0xff);
            m.sh(puff + 0x14, vz as u16);
            m.sw(puff + 8, py);
            m.sw(puff + 0xc, pz);
            if index < 3 {
                m.sh(puff + 0x16, 0x8c);
                m.sh(puff + 0x18, 0x1504);
                m.sh(puff + 0x1a, ((index << 5) + 0x118) as u16);
                let vy = lhu(m, puff + 0x12).wrapping_sub(0x80);
                m.sh(puff + 0x12, vy as u16);
            } else {
                m.sh(puff + 0x16, 0x188);
                m.sh(puff + 0x18, 0);
                m.sh(puff + 0x1a, (index + 0x18) as u16);
            }
        }
    });
}

/// 80081298 (point, velocity, large): explosion: effect command 1
/// (8007ED9C; large also with gameplay flag 0x100) and its particles
/// (80080F70).
pub fn explosion_7fc50(m: &mut Machine, point: u32, velocity: u32, large: u32) {
    with_frame(m, 0x20, |m, _| {
        let gameplay = m.gw(common::GAMEPLAY);
        let large = if m.lw(gameplay) & 0x100 != 0 {
            1
        } else {
            large
        };
        let command = original(m, 0x8007_ed9c, [1, point, velocity, 0]);
        m.sb(command + 2, large as u8);
        explosion_particles_7f928(m, point, velocity, large);
    });
}

/// 80082D80 (point, strength): ground dust: a flash particle sized by the
/// strength (plus random ±0x20) scattered around `point` by the strength's
/// magnitude.
pub fn dust_81738(m: &mut Machine, point: u32, strength: u32) {
    with_frame(m, 0x28, |m, _| {
        let random = sdk::rand(m);
        let strength = strength.wrapping_add(random & 0x3f).wrapping_sub(0x20);
        if !lt(0, strength) {
            return;
        }
        let dust = particle(m, 0x600, 3, 7);
        if dust == 0 {
            return;
        }
        m.gte.mtc2(reg::LZCS, strength);
        m.sw(dust, (sra(strength, 4).wrapping_add(4) << 16) | 1);
        m.sw(dust + 0x18, (strength << 16) | 0x130);
        m.sh(dust + 0x16, 0);
        let zeros = m.gte.mfc2(reg::LZCR);
        let mask = (1u32 << (0x1fu32.wrapping_sub(zeros) & 31)).wrapping_sub(1);
        let half = 1u32 << (0x1eu32.wrapping_sub(zeros) & 31);
        let random = sdk::rand(m);
        let x = m
            .lw(point)
            .wrapping_add((random & mask).wrapping_sub(half) << 10);
        m.sw(dust + 4, x);
        let random = sdk::rand(m);
        let z = m
            .lw(point + 8)
            .wrapping_add((random & mask).wrapping_sub(half) << 10);
        m.sw(dust + 0xc, z);
        let y = m.lw(point + 4);
        m.sw(dust + 8, y);
    });
}

/// 80082FF0 () -> tracer or 0: claim the first free of the 16 tracers
/// (effect block +0x3968, 0x1C bytes; mask word +0x3964, low half in use
/// and high half fresh), or 0 when the mask is negative.
pub fn claim_tracer_819a8(m: &mut Machine) -> u32 {
    let block = m.gw(GP_EFFECTS);
    let mut tracer = block + 0x3968;
    let mask = m.lw(block + 0x3964);
    if lt(mask, 0) {
        return 0;
    }
    let used = mask & (mask >> 16);
    let mut bit = 1u32;
    if used & 1 != 0 {
        loop {
            bit <<= 1;
            tracer = tracer.wrapping_add(0x1c);
            if used & bit == 0 {
                break;
            }
        }
    }
    m.sw(block + 0x3964, mask | bit | (bit << 16));
    tracer
}

/// 80083054 (from, to, length, kind) -> tracer or 0: a tracer from `from`
/// to `to` (VECTORs).
pub fn tracer_81a0c(m: &mut Machine, from: u32, to: u32, length: u32, kind: u32) -> u32 {
    with_frame(m, 0x28, |m, _| {
        let tracer = claim_tracer_819a8(m);
        if tracer == 0 {
            return 0;
        }
        m.sh(tracer, length as u16);
        m.sh(tracer + 2, kind as u16);
        copy_block(m, tracer + 4, from, 3);
        copy_block(m, tracer + 0x10, to, 3);
        tracer
    })
}

/// 800830DC (from, to, length, kind): the tracer as an effect command 1
/// (8007EEAC) with the span (scaled down by 16 beyond ±0x800000, flag 4),
/// length and kind.
pub fn tracer_command_81a94(m: &mut Machine, from: u32, to: u32, length: u32, kind: u32) {
    with_frame(m, 0x30, |m, frame| {
        let mut dx = m.lw(to).wrapping_sub(m.lw(from));
        let mut dy = m.lw(to + 4).wrapping_sub(m.lw(from + 4));
        let mut dz = m.lw(to + 8).wrapping_sub(m.lw(from + 8));
        let far = 0x100_0000 < dx.wrapping_add(0x80_0000)
            || lt(0x80_0000, dy)
            || lt(dy, 0xff80_0000)
            || lt(0x80_0000, dz)
            || lt(dz, 0xff80_0000);
        let scale = if far {
            dx = sra(dx, 4);
            dy = sra(dy, 4);
            dz = sra(dz, 4);
            4
        } else {
            0
        };
        m.sw(frame + 0x18, dz);
        m.sw(frame + 0x14, dy);
        m.sw(frame + 0x10, dx);
        let command = vector_command_7d830(m, 1, from, frame + 0x10);
        let word = m.lw(command + 0x10);
        m.sb(command + 2, kind as u8);
        let word = ((word & 0xffff_f000) | scale) & 0xff00_0fff;
        m.sw(command + 0x10, word | ((length & 0xfff) << 12));
    });
}

/// 800831D0 (position, angles, length, kind): gun fire: the muzzle and the
/// end of the line of fire (0x1AE0 ahead) in the gun's frame, cut at the
/// first terrain hit (8009A800); a tracer command when gameplay flag 2
/// and the tracer itself (kind 1 from the offset muzzle).
pub fn gun_fire_81b88(m: &mut Machine, position: u32, angles: u32, length: u32, kind: u32) {
    with_frame(m, 0x60, |m, frame| {
        shared::rotation_yxz(m, angles, SCRATCH);
        sdk::set_rot_matrix(m, SCRATCH);
        let rotate = |m: &mut Machine, xy: u32, z: u32| -> [u32; 3] {
            m.gte.mtc2(reg::VXY0, xy);
            m.gte.mtc2(reg::VZ0, z);
            m.gte.mvmva(true, false, 0, 0, 3);
            [
                m.gte.mfc2(reg::MAC1),
                m.gte.mfc2(reg::MAC2),
                m.gte.mfc2(reg::MAC3),
            ]
        };
        let muzzle = rotate(m, 0xfff8_0000, 0x37);
        let origin = [m.lw(position), m.lw(position + 4), m.lw(position + 8)];
        let end = rotate(m, 0, 0x1ae0);
        for axis in 0..3 {
            let offset = axis as u32 * 4;
            m.sw(
                frame + 0x10 + offset,
                (muzzle[axis] << 12).wrapping_add(origin[axis]),
            );
        }
        for axis in 0..3 {
            let offset = axis as u32 * 4;
            m.sw(
                frame + 0x20 + offset,
                (end[axis] << 12).wrapping_add(origin[axis]),
            );
        }
        let mut start = origin;
        if kind == 1 {
            let offset = rotate(m, 0x30_0000, 0x82);
            for axis in 0..3 {
                start[axis] = start[axis].wrapping_add(offset[axis] << 12);
            }
        }
        let hit = original(
            m,
            0x8009_a800,
            [frame + 0x10, frame + 0x20, frame + 0x30, SCRATCH],
        );
        let end = if lt(0x7fff_fffe, hit) {
            frame + 0x20
        } else {
            frame + 0x30
        };
        let gameplay = m.gw(common::GAMEPLAY);
        if m.lw(gameplay) & 2 != 0 {
            tracer_command_81a94(m, frame + 0x10, end, length, kind);
        }
        if kind == 1 {
            m.sw(frame + 0x10, start[0]);
            m.sw(frame + 0x14, start[1]);
            m.sw(frame + 0x18, start[2]);
        }
        tracer_81a0c(m, frame + 0x10, end, length, kind);
    });
}

/// 80082C1C (point, velocity): a spark flash thrown back from `point`.
pub fn spark_815d4(m: &mut Machine, point: u32, velocity: u32) {
    with_frame(m, 0x28, |m, _| {
        let spark = particle(m, 0x600, 3, 7);
        if spark == 0 {
            return;
        }
        let vx = m.lw(velocity);
        let vy = m.lw(velocity + 4);
        let x = m.lw(point).wrapping_sub(vx);
        let vz = m.lw(velocity + 8);
        m.sw(spark + 4, x);
        let y = m.lw(point + 4).wrapping_sub(vy);
        m.sw(spark + 8, y);
        let z = m.lw(point + 8);
        m.sh(spark + 0x16, 0x87);
        m.sw(spark + 0xc, z.wrapping_sub(vz));
        let random = sdk::rand(m);
        m.sh(spark, ((random & 1) + 4) as u16);
        m.sh(spark + 2, ((random & 3) + 0x28) as u16);
        let sx = sra(vx.wrapping_neg(), 6)
            .wrapping_sub(0x40)
            .wrapping_add(random & 0x7f);
        let sy = sra(vy.wrapping_neg(), 6)
            .wrapping_sub(0x40)
            .wrapping_add(sra(random, 3) & 0x7f);
        let sz = sra(vz.wrapping_neg(), 6)
            .wrapping_sub(0x40)
            .wrapping_add(sra(random, 6) & 0x7f);
        m.sh(spark + 0x10, sx as u16);
        m.sh(spark + 0x12, sy as u16);
        m.sh(spark + 0x14, sz as u16);
        m.sh(spark + 0x18, 0x111);
        m.sh(spark + 0x1a, ((random & 0x3f) + 0xf0) as u16);
    });
}

/// 80082D38 (point, velocity): sparks: effect command 8 (8007ED9C) and a
/// spark flash (80082C1C).
pub fn sparks_816f0(m: &mut Machine, point: u32, velocity: u32) {
    with_frame(m, 0x20, |m, _| {
        original(m, 0x8007_ed9c, [8, point, velocity, 0]);
        spark_815d4(m, point, velocity);
    });
}

/// 80084E6C (position, velocity): a wake spray (800829BC with gameplay
/// flag 2, else 800827E0) when low over the water, more likely the lower.
pub fn wake_83814(m: &mut Machine, position: u32, velocity: u32) {
    with_frame(m, 0x28, |m, _| {
        let water = m.gw(GP_WATER);
        let y = m.lw(position + 4);
        let vy = m.lw(velocity + 4);
        let level = m.lw(water + 4);
        let depth = sra(y.wrapping_sub(level), 12).wrapping_add(sra(vy, 10));
        if lt(depth, 0) {
            return;
        }
        let random = sdk::rand(m) & 0xfff;
        if lt(depth, random) {
            return;
        }
        let x = m.lw(position);
        let z = m.lw(position + 8);
        let vx = m.lw(velocity);
        let vz = m.lw(velocity + 8);
        let gameplay = m.gw(common::GAMEPLAY);
        m.sw(SCRATCH + 4, y.wrapping_add(vy));
        m.sw(SCRATCH + 0x14, vy);
        m.sw(SCRATCH, x.wrapping_add(vx << 1));
        m.sw(SCRATCH + 8, z.wrapping_add(vz << 1));
        m.sw(SCRATCH + 0x10, vx);
        m.sw(SCRATCH + 0x18, vz);
        let routine = if m.lw(gameplay) & 2 != 0 {
            0x8008_29bc
        } else {
            0x8008_27e0
        };
        original(m, routine, [SCRATCH, SCRATCH + 0x10, 8, vx]);
    });
}

/// 80093B2C (ot, prim, colors, level) -> next primitive: a screen fade
/// (DPCS of 0x62FFFFFF toward black by `level`, semi-transparency mode 1,
/// or 2 for a negative level) linked into `ot`; nothing for level 0.
pub fn fade_92350(m: &mut Machine, ot: u32, prim: u32, colors: u32, level: u32) -> u32 {
    if level == 0 {
        return prim;
    }
    let (level, mode) = if lt(level, 0) {
        (level.wrapping_neg(), 2u32)
    } else {
        (level, 1)
    };
    m.gte.ctc2(ctrl::RFC, 0);
    m.gte.ctc2(ctrl::GFC, 0);
    m.gte.ctc2(ctrl::BFC, 0);
    m.gte.mtc2(reg::RGBC, 0x62ff_ffff);
    m.gte.mtc2(reg::IR0, 0x1000u32.wrapping_sub(level));
    let link = m.lw(ot) | 0x400_0000;
    m.sw(prim, link);
    m.sw(ot, prim & 0x00ff_ffff);
    m.gte.dpcs(true, false);
    m.sw(prim + 4, ((mode & 3) << 5) | 0xe100_0200);
    let first = m.lw(colors);
    let second = m.lw(colors + 4);
    m.sw(prim + 0xc, first);
    m.sw(prim + 0x10, second);
    let color = m.gte.mfc2(reg::RGB2);
    m.sw(prim + 8, color);
    prim.wrapping_add(0x14)
}

// ==== trail queues ([gp+0x53C]) =======================================================

/// The geometry of one family of trail queues.
struct TrailQueues {
    /// Offset of the in-use mask word from the trails base.
    mask: u32,
    /// Offset of the first queue.
    first: u32,
    /// Bytes per queue (+0 count, +4 oldest, +8 ages, +0x88 lives, +0x108
    /// points).
    stride: u32,
    /// Bytes per point.
    point: u32,
    /// The point words copied, by offset.
    words: &'static [u32],
    /// The life of a new point.
    life: u32,
}

/// Missile trails: 32 queues of 0x908 bytes after the mask word.
const MISSILE_TRAILS: TrailQueues = TrailQueues {
    mask: 0,
    first: 4,
    stride: 0x908,
    point: 0x40,
    words: &[
        0, 4, 8, 0x10, 0x14, 0x18, 0x20, 0x24, 0x28, 0x30, 0x34, 0x38,
    ],
    life: 0x5a,
};

/// Entity trails: queues of 0x508 bytes from +0x12128 (mask +0x12124).
const ENTITY_TRAILS: TrailQueues = TrailQueues {
    mask: 0x1_2124,
    first: 0x1_2128,
    stride: 0x508,
    point: 0x20,
    words: &[0, 4, 8, 0x10, 0x14, 0x18],
    life: 0x3f,
};

fn trail_queue(base: u32, queues: &TrailQueues, handle: u32) -> u32 {
    base.wrapping_add(queues.first)
        .wrapping_add(handle.wrapping_mul(queues.stride))
}

fn copy_point(m: &mut Machine, queues: &TrailQueues, target: u32, source: u32) {
    for &offset in queues.words {
        let word = m.lw(source + offset);
        m.sw(target + offset, word);
    }
}

/// Append `point` to queue `handle` (-1: claim the first free queue,
/// emptied) unless 32 points are pending. Returns the handle, or -1.
fn push_trail(m: &mut Machine, queues: &TrailQueues, handle: u32, point: u32) -> u32 {
    let base = m.gw(GP_TRAILS);
    let mask = m.lw(base.wrapping_add(queues.mask));
    let (handle, queue) = if handle == u32::MAX {
        if mask == u32::MAX {
            return u32::MAX;
        }
        let mut index = 0;
        while mask & bit(index) != 0 {
            index += 1;
        }
        let base = m.gw(GP_TRAILS);
        let queue = trail_queue(base, queues, index);
        m.sw(queue + 4, 0);
        m.sw(queue, 0);
        m.sw(base.wrapping_add(queues.mask), mask | bit(index));
        (index, queue)
    } else {
        if !mask & bit(handle) != 0 {
            return u32::MAX;
        }
        (handle, trail_queue(base, queues, handle))
    };
    let count = m.lw(queue);
    if !lt(count.wrapping_sub(m.lw(queue + 4)), 0x20) {
        return handle;
    }
    m.sw(queue + 8 + ((count & 0x1f) << 2), 0);
    let count = m.lw(queue);
    m.sw(queue + 0x88 + ((count & 0x1f) << 2), queues.life);
    let count = m.lw(queue);
    m.sw(queue, count.wrapping_add(1));
    let target = queue + (count & 0x1f) * queues.point + 0x108;
    copy_point(m, queues, target, point);
    handle
}

/// Rewrite the newest point of queue `handle` with `point`. Returns the
/// handle, or -1 for a negative or unused handle.
fn rewrite_trail(m: &mut Machine, queues: &TrailQueues, handle: u32, point: u32) -> u32 {
    let base = m.gw(GP_TRAILS);
    let mask = m.lw(base.wrapping_add(queues.mask));
    if lt(handle, 0) || !mask & bit(handle) != 0 {
        return u32::MAX;
    }
    let queue = trail_queue(base, queues, handle);
    let count = m.lw(queue);
    let target = queue + (count.wrapping_sub(1) & 0x1f) * queues.point + 0x108;
    copy_point(m, queues, target, point);
    handle
}

/// 8007BC88 (handle, point) -> handle or -1: append a missile trail point
/// (position, two side points and a colour word set).
pub fn push_missile_trail_7a60c(m: &mut Machine, handle: u32, point: u32) -> u32 {
    push_trail(m, &MISSILE_TRAILS, handle, point)
}

/// 8007BE3C (handle, point) -> handle or -1: rewrite the newest missile
/// trail point.
pub fn rewrite_missile_trail_7a7c0(m: &mut Machine, handle: u32, point: u32) -> u32 {
    rewrite_trail(m, &MISSILE_TRAILS, handle, point)
}

/// 8007CC6C (handle, point) -> handle or -1: append an entity trail point.
pub fn push_entity_trail_7b5f0(m: &mut Machine, handle: u32, point: u32) -> u32 {
    push_trail(m, &ENTITY_TRAILS, handle, point)
}

/// 8007CDF8 (handle, point) -> handle or -1: rewrite the newest entity
/// trail point.
pub fn rewrite_entity_trail_7b77c(m: &mut Machine, handle: u32, point: u32) -> u32 {
    rewrite_trail(m, &ENTITY_TRAILS, handle, point)
}

// ---- phase-3: script sound, phase entry and the blast effect spawners -----------------------

/// 8002F460 (sound, position, shift): a script-requested positioned sound:
/// the channel from 8002DBE0 (0xFF: none, returned), then the position
/// relative to the camera (8002E6DC), volume (8002E84C with `shift`), pan
/// (8002E8DC), pitch from the distance (8002EA24) and a random detune
/// (8002F0D4), as the player's 8002F3C0.
pub fn script_sound_2f044(m: &mut Machine, sound: u32, position: u32, shift: u32) -> u32 {
    with_frame(m, 0x30, |m, frame| {
        let channel = original(m, 0x8002_dbe0, [sound, position, shift, 0]);
        if channel == 0xff {
            return 0xff;
        }
        let gameplay = m.gw(common::GAMEPLAY);
        let camera = m.lw(gameplay + 0x28);
        original(m, 0x8002_e6dc, [frame + 0x10, camera + 4, position, 0]);
        original(m, 0x8002_e84c, [channel, frame + 0x10, shift & 0xff, 0]);
        original(m, 0x8002_e8dc, [channel, frame + 0x10, 0, 0]);
        let distance = lh(m, frame + 0x1a);
        original(m, 0x8002_ea24, [channel, distance, 0, 0]);
        let detune = (sdk::rand(m) & 0x3ff).wrapping_sub(0x200);
        original(m, 0x8002_f0d4, [channel, detune, 0, 0])
    })
}

/// 800B1FC4: the mission phase table (4 bytes per phase: kind, unused,
/// two entity slots), indexed by `[gp+0x334]`.
const PHASES: u32 = 0x800b_1fc4;
/// `[gp+0x334]`: the current mission phase.
const GP_PHASE: u32 = 0x334;
/// `[gp+0x388]`: per-slot group records (0x28 bytes, +8 the definition).
const GP_SLOT_GROUPS: u32 = 0x388;

// JP-REVIEW(R0029): changed function: PAL 8004C644 -> JP 8004DC38, call+:1 imm:4 ins:129 op:2, 135 instructions differ (whole routine)
/// 8004DC38: enter the current mission phase: for phase kinds 0-3 (and 7
/// outside mission 7) strip the live flags (0x400 from slots under a
/// shared lock 0x400000, else 0x4000) of slots 2..31, then bring in the
/// phase's two entity slots: live (0x4000), reset (8004E0FC, 8004E124),
/// AI record cleared, and placed by the kind: kinds 2/3 on the player
/// (raised 0x1000000, back 0x100000, +-0x400000 sideways at difficulty
/// 2+; attitude and velocity copied; state 0x13), kinds 5/6 at the spawn
/// point (state 0x14), kind 7 150 steps ahead of the player (state 0x15
/// for the first, 0x16 for the second).
// JP-PATCH: replaced by `crate::jp::res_p2_ai::phase_entities_4c644` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn phase_entities_4c644(m: &mut Machine) {
    crate::jp::res_p2_ai::phase_entities_4c644(m)
}

/// 8004E80C -> 0: in phase kinds 5 and 6, run 8004E900 for every live
/// ground slot (32 + n) with hit points whose group definition kind
/// (+0x1D) is 0x10 or 0x11.
pub fn phase_ground_4d204(m: &mut Machine) -> u32 {
    with_frame(m, 0x28, |m, _| {
        let phase = (m.gw(GP_PHASE) << 2).wrapping_add(PHASES);
        if lbu(m, phase).wrapping_sub(5) >= 2 {
            return 0;
        }
        for slot in 0..32u32 {
            let target = entity(m, slot + 32);
            if !m.lw(target) & 0x4000 != 0 || !lt(0, lh(m, target + 0x4e)) {
                continue;
            }
            let groups = m.gw(GP_SLOT_GROUPS);
            let definition = m.lw((slot * 0x28).wrapping_add(groups) + 8);
            let group = lbu(m, definition + 0x1d);
            if group == 0x11 || group == 0x10 {
                original(m, 0x8004_e900, [slot, 0, 0, 0]);
            }
        }
        0
    })
}

/// 80087168 (x, y, z, packed): a shock wave particle: life `packed & 0xFF`
/// plus 0..3, growth `packed >> 16` plus a random byte.
pub fn shock_wave_859f8(m: &mut Machine, x: u32, y: u32, z: u32, packed: u32) {
    with_frame(m, 0x28, |m, _| {
        let wave = particle(m, 0x33c0, 1, 6);
        if wave == 0 {
            return;
        }
        let random = sdk::rand(m);
        m.sh(wave + 2, ((packed & 0xff) + (random & 3)) as u16);
        let growth = sra(packed, 16).wrapping_add(random & 0xff);
        m.sh(wave, 1);
        m.sw(wave + 4, x);
        m.sw(wave + 8, y);
        m.sw(wave + 0xc, z);
        m.sh(wave + 0x14, 1);
        m.sh(wave + 0x16, growth as u16);
        m.sw(wave + 0x10, 0x400);
    });
}

/// 8008721C (x, y, z): a large flash and five falling sparks.
pub fn flash_sparks_85aac(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        let flash = particle(m, 0x600, 3, 7);
        let mut fall: u32 = (-0x64_i32) as u32;
        if flash != 0 {
            let random = sdk::rand(m);
            m.sw(flash, 0x28_ffff);
            m.sh(flash + 0x16, 0x86);
            m.sh(flash + 0x10, spread(sra(random, 2), 0x7f, 0x40, 0) as u16);
            m.sh(flash + 0x12, (-0xf0_i32) as u16);
            m.sh(flash + 0x14, spread(sra(random, 5), 0x7f, 0x40, 0) as u16);
            m.sh(flash + 0x18, 0x210);
            m.sw(flash + 4, x);
            m.sw(flash + 8, y);
            m.sw(flash + 0xc, z);
            m.sh(flash + 0x1a, ((random & 0x7f) + 0x1b4) as u16);
        }
        for index in 0..5u32 {
            let spark = particle(m, 0x600, 3, 7);
            if spark == 0 {
                return;
            }
            let random = sdk::rand(m);
            m.sh(spark, 0xffff);
            m.sh(spark + 2, (index + (random & 1) + 0x12) as u16);
            m.sw(spark + 4, x.wrapping_add(spread(random, 0xff, 0x80, 11)));
            m.sw(
                spark + 8,
                y.wrapping_add(spread(sra(random, 3), 0xff, 0x80, 11)),
            );
            m.sw(
                spark + 0xc,
                z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 11)),
            );
            let random = sdk::rand(m);
            m.sh(spark + 0x12, fall as u16);
            fall = fall.wrapping_sub(0x50);
            m.sh(spark + 0x10, spread(random, 0xff, 0x80, 0) as u16);
            m.sh(spark + 0x14, spread(sra(random, 7), 0xff, 0x80, 0) as u16);
            m.sh(spark + 0x16, 0x85);
            m.sh(spark + 0x18, 0x100);
            let life = (index << 6)
                .wrapping_add(sra(random, 4) & 0x7f)
                .wrapping_add(0x74);
            m.sh(spark + 0x1a, life as u16);
        }
    });
}

/// One debris chunk of the blast spawners: position spread `<< 8` and
/// velocity `<< 2` from three random bytes (x from bits 6.., y from bits
/// 0.., z from bits 3..), the upward velocity offset `lift`; then the
/// size fields by `body`.
fn blast_chunk(m: &mut Machine, chunk: u32, origin: [u32; 3], random: u32, lift: u32) -> u32 {
    let ry = (random & 0xff).wrapping_sub(0x80);
    let rx = (sra(random, 6) & 0xff).wrapping_sub(0x80);
    let rz = sra(random, 3) & 0xff;
    let vy = rz.wrapping_sub(lift) << 2;
    m.sw(chunk + 8, origin[1].wrapping_add(ry << 8));
    m.sh(chunk + 0x10, (ry << 2) as u16);
    m.sw(chunk + 4, origin[0].wrapping_add(rx << 8));
    m.sh(chunk + 0x14, (rx << 2) as u16);
    m.sw(
        chunk + 0xc,
        origin[2].wrapping_add(rz.wrapping_sub(0x80) << 8),
    );
    vy
}

/// 800873E0 (x, y, z): sixteen debris chunks, the first eight large
/// tumbling ones.
pub fn debris_85c70(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        for index in 0..16u32 {
            let chunk = particle(m, 0x1098, 10, 7);
            if chunk == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mut lift = blast_chunk(m, chunk, [x, y, z], random, 0xe0);
            let size = if index < 8 {
                lift = lift.wrapping_sub(0x180);
                let low = random & 0xf;
                m.sh(chunk + 2, (low + 0x18) as u16);
                m.sh(chunk + 0x18, 0x1604);
                m.sh(chunk, 0);
                m.sh(chunk + 0x1a, ((low << 2) + 0x258) as u16);
                0x8b
            } else {
                let low = random & 0xf;
                m.sh(chunk + 0x1a, (low + 0x19) as u16);
                m.sh(chunk, 0);
                m.sh(chunk + 2, (low + 0x1e) as u16);
                m.sh(chunk + 0x18, 0);
                0x195
            };
            m.sh(chunk + 0x16, size);
            m.sh(chunk + 0x12, lift as u16);
        }
    });
}

/// 80087520 (x, y, z): the medium blast: flash and sparks, sixteen debris
/// chunks and a shock wave (0x05140010).
pub fn blast_85db0(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        flash_sparks_85aac(m, x, y, z);
        debris_85c70(m, x, y, z);
        shock_wave_859f8(m, x, y, z, 0x0514_0010);
    });
}

/// 8008762C (x, y, z): eight debris chunks, the first four large.
pub fn debris_85ebc(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        for index in 0..8u32 {
            let chunk = particle(m, 0x1098, 10, 7);
            if chunk == 0 {
                return;
            }
            let random = sdk::rand(m);
            let ry = (random & 0xff).wrapping_sub(0x80);
            m.sh(chunk + 0x10, ry as u16);
            let rx = (sra(random, 6) & 0xff).wrapping_sub(0x80);
            m.sh(chunk + 0x14, rx as u16);
            let rz = sra(random, 3) & 0xff;
            let lift = rz.wrapping_sub(0x110);
            m.sw(chunk + 8, y.wrapping_add(ry << 8));
            m.sw(chunk + 4, x.wrapping_add(rx << 8));
            m.sw(chunk + 0xc, z.wrapping_add(rz.wrapping_sub(0x80) << 8));
            let size = if index < 4 {
                m.sh(chunk + 2, ((random & 0xf) + 0x1a) as u16);
                m.sh(chunk + 0x18, 0x1804);
                m.sh(chunk + 0x1a, ((random & 0x7f) + 0x118) as u16);
                m.sh(chunk, 0);
                0x8d
            } else {
                let low = random & 0xf;
                m.sh(chunk + 0x1a, (low + 0x19) as u16);
                m.sh(chunk, 0);
                m.sh(chunk + 2, (low + 0x1e) as u16);
                m.sh(chunk + 0x18, 0);
                0x190
            };
            m.sh(chunk + 0x16, size);
            m.sh(chunk + 0x12, lift as u16);
        }
    });
}

/// 8008775C (x, y, z): a small flash.
pub fn flash_85fec(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        let flash = particle(m, 0x600, 3, 7);
        if flash == 0 {
            return;
        }
        let random = sdk::rand(m);
        m.sh(flash, 0xffff);
        m.sh(flash + 2, ((random & 7) + 0x14) as u16);
        m.sw(flash + 4, x);
        m.sw(flash + 8, y);
        m.sw(flash + 0xc, z);
        m.sh(flash + 0x16, 0);
        m.sh(flash + 0x18, 0x100);
        m.sh(flash + 0x1a, ((random & 0x7f) + 0xdc) as u16);
    });
}

/// 800877FC (x, y, z): the small blast: a flash, eight debris chunks and
/// a random shock wave.
pub fn blast_8608c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        flash_85fec(m, x, y, z);
        debris_85ebc(m, x, y, z);
        let wave = particle(m, 0x33c0, 1, 6);
        if wave == 0 {
            return;
        }
        let random = sdk::rand(m);
        m.sh(wave + 2, ((random & 3) + 0xa) as u16);
        m.sh(wave + 0x14, 1);
        m.sh(wave, 0);
        m.sw(wave + 4, x);
        m.sw(wave + 8, y);
        m.sw(wave + 0xc, z);
        m.sh(wave + 0x16, ((random & 0x7f) + 0x258) as u16);
        m.sh(wave + 0x10, random as u16);
        let random = sdk::rand(m);
        m.sh(wave + 0x12, random as u16);
    });
}

/// 800878B4 (x, y, z): three stacked large flashes and eight wide sparks.
pub fn flashes_86144(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        let mut rise = 0u32;
        let mut height = y;
        for index in 0..3u32 {
            let flash = particle(m, 0x600, 3, 7);
            if flash == 0 {
                return;
            }
            let random = sdk::rand(m);
            m.sh(flash + 0x12, rise as u16);
            rise = rise.wrapping_sub(0xdc);
            m.sw(flash + 8, height);
            height = height.wrapping_add(0xfff0_0000);
            m.sh(flash, 0xffff);
            m.sh(flash + 2, ((index << 1) + (random & 7) + 0x28) as u16);
            let low = random & 0xff;
            m.sh(flash + 0x10, low.wrapping_sub(0x80) as u16);
            m.sh(flash + 0x14, spread(sra(random, 7), 0xff, 0x80, 0) as u16);
            m.sh(flash + 0x16, 0x87);
            m.sh(flash + 0x18, 0x210);
            m.sw(flash + 4, x);
            m.sw(flash + 0xc, z);
            m.sh(flash + 0x1a, ((index << 8) + low + 0x1f4) as u16);
        }
        let mut fall: u32 = (-0x8c_i32) as u32;
        let mut height = y;
        for index in 0..8u32 {
            let spark = particle(m, 0x600, 3, 7);
            if spark == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mixed = sra(random, 8) | (random << 7);
            let lift = fall.wrapping_sub(0x80);
            fall = fall.wrapping_sub(0x6e);
            let py = height.wrapping_add(spread(sra(random, 3), 0xff, 0x80, 13));
            height = height.wrapping_add(0xfffb_0000);
            let life = index + (random & 3) + 0x16;
            let px = x.wrapping_add(spread(random, 0xff, 0x80, 13));
            let pz = z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 13));
            m.sh(spark, 0xffff);
            m.sh(spark + 0x10, spread(mixed, 0xff, 0x80, 0) as u16);
            let lift = lift.wrapping_add(sra(mixed, 3) & 0xff);
            m.sh(spark + 0x14, spread(sra(mixed, 6), 0xff, 0x80, 0) as u16);
            m.sh(spark + 0x16, 0x85);
            m.sh(spark + 0x18, 0x100);
            let size = (index << 7) + (sra(mixed, 4) & 0x7f) + 0x104;
            m.sh(spark + 2, life as u16);
            m.sw(spark + 4, px);
            m.sw(spark + 8, py);
            m.sw(spark + 0xc, pz);
            m.sh(spark + 0x12, lift as u16);
            m.sh(spark + 0x1a, size as u16);
        }
    });
}

/// 80087AD0 (x, y, z): twenty debris chunks, the first fifteen large and
/// thrown up harder in turn.
pub fn debris_86360(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        for index in 0..20u32 {
            let chunk = particle(m, 0x1098, 10, 7);
            if chunk == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mut lift = blast_chunk(m, chunk, [x, y, z], random, 0x80).wrapping_sub(0x180);
            let size = if index < 15 {
                lift = lift.wrapping_sub((index << 6) + 0x190);
                m.sh(chunk + 2, ((random & 0xf) + index + 0x18) as u16);
                m.sh(chunk + 0x18, 0x1804);
                m.sh(chunk + 0x1a, ((random & 0xff) + 0x2bc) as u16);
                m.sh(chunk, 0);
                0x8d
            } else {
                let low = random & 0xf;
                m.sh(chunk + 0x1a, (low + 0x19) as u16);
                m.sh(chunk, 0);
                m.sh(chunk + 2, (low + 0x1e) as u16);
                m.sh(chunk + 0x18, 0);
                0x195
            };
            m.sh(chunk + 0x16, size);
            m.sh(chunk + 0x12, lift as u16);
        }
    });
}

/// 80087C18 (x, y, z): the large blast: three flashes with sparks, twenty
/// debris chunks and a shock wave (0x07080018).
pub fn blast_864a8(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        flashes_86144(m, x, y, z);
        debris_86360(m, x, y, z);
        shock_wave_859f8(m, x, y, z, 0x0708_0018);
    });
}

// ---- phase 4: the spark (8002479C) and trail (80024804) handler tables -----------------------
//
// effects.rs's 80081BC8 and 80083C3C call these through their code-pointer
// tables with (position, velocity or variant, kind, table entry 3).

/// The scrambled random word the spawners derive a second set of fields
/// from: `(r >> 8) | (r << 7)`.
fn scramble(random: u32) -> u32 {
    sra(random, 8) | (random << 7)
}

/// A point `origin` + three random byte spreads `<< shift` (x from bits
/// 0.., y from 3.., z from 6..).
fn scatter(origin: [u32; 3], random: u32, shift: u32) -> [u32; 3] {
    [
        origin[0].wrapping_add(spread(random, 0xff, 0x80, shift)),
        origin[1].wrapping_add(spread(sra(random, 3), 0xff, 0x80, shift)),
        origin[2].wrapping_add(spread(sra(random, 6), 0xff, 0x80, shift)),
    ]
}

fn store_point(m: &mut Machine, particle: u32, point: [u32; 3]) {
    m.sw(particle + 4, point[0]);
    m.sw(particle + 8, point[1]);
    m.sw(particle + 0xc, point[2]);
}

fn load_point(m: &mut Machine, address: u32) -> [u32; 3] {
    [m.lw(address), m.lw(address + 4), m.lw(address + 8)]
}

/// 8008136C (position, velocity): spark kind 0: a flash at the position
/// jittered by 7-bit offsets (<< 11), on odd seeds a second flash one
/// velocity step back, and three times in four a smoke particle drifting
/// with the velocity.
pub fn spark_7fd24(m: &mut Machine, position: u32, velocity: u32) {
    with_frame(m, 0x38, |m, _| {
        let [vx, vy, vz] = load_point(m, velocity);
        let mut point = [0u32; 3];
        for (axis, value) in point.iter_mut().enumerate() {
            let random = sdk::rand(m) & 0x7f;
            *value = m.lw(position + axis as u32 * 4).wrapping_add(random << 11);
        }
        let seed = sdk::rand(m);
        let flash = particle(m, 0x600, 3, 7);
        if flash != 0 {
            m.sw(flash, 0x28_0000);
            m.sh(flash + 0x18, 0x210);
            store_point(m, flash, point);
            m.sh(flash + 0x16, 0);
            m.sh(flash + 0x1a, ((seed & 0x3f) + 0x80) as u16);
        }
        if seed & 1 != 0 {
            let flash = particle(m, 0x600, 3, 7);
            if flash != 0 {
                m.sw(flash, 0xc_ffff);
                let back = [
                    point[0].wrapping_sub(vx),
                    point[1].wrapping_sub(vy),
                    point[2].wrapping_sub(vz),
                ];
                store_point(m, flash, back);
                m.sh(flash + 0x18, 0x100);
                m.sh(flash + 0x16, 0);
                m.sh(flash + 0x1a, ((sra(seed, 3) & 0x3f) + 0x58) as u16);
            }
        }
        if seed & 3 == 0 {
            return;
        }
        let smoke = particle(m, 0x1098, 10, 7);
        if smoke == 0 {
            return;
        }
        store_point(m, smoke, point);
        let random = sdk::rand(m);
        let drift = |v: u32, r: u32| sra(v, 8).wrapping_sub(0x80).wrapping_add(r & 0xff);
        m.sh(smoke + 0x10, drift(vx, random) as u16);
        m.sh(smoke + 0x12, drift(vy, sra(random, 3)) as u16);
        m.sh(smoke + 0x14, drift(vz, sra(random, 6)) as u16);
        let life = if random & 1 != 0 {
            m.sw(smoke, 0x12_0000);
            m.sh(smoke + 0x16, 0x8b);
            m.sh(smoke + 0x18, 0x1504);
            (random & 0x1f) + 0x64
        } else {
            m.sw(smoke, 0x1e_0000);
            m.sh(smoke + 0x16, 0x18c);
            m.sh(smoke + 0x18, 0);
            (random & 7) + 0x14
        };
        m.sh(smoke + 0x1a, life as u16);
    });
}

/// 8008158C (position, velocity): spark kind 1: a flash one velocity step
/// ahead, scattered (<< 9), and on odd seeds a smoke particle there.
pub fn spark_7ff44(m: &mut Machine, position: u32, velocity: u32) {
    with_frame(m, 0x38, |m, _| {
        let [vx, vy, vz] = load_point(m, velocity);
        let ahead = [
            m.lw(position).wrapping_add(vx),
            m.lw(position + 4).wrapping_add(vy),
            m.lw(position + 8).wrapping_add(vz),
        ];
        let seed = sdk::rand(m);
        let flash = particle(m, 0x600, 3, 7);
        if flash != 0 {
            m.sh(flash, 0xffff);
            m.sh(flash + 2, ((seed & 7) + 9) as u16);
            store_point(m, flash, scatter(ahead, seed, 9));
            m.sh(flash + 0x18, 0x100);
            m.sh(flash + 0x16, 0);
            m.sh(flash + 0x1a, ((seed & 0x3f) + 0x5a) as u16);
        }
        if seed & 1 == 0 {
            return;
        }
        let smoke = particle(m, 0x1098, 10, 7);
        if smoke == 0 {
            return;
        }
        m.sh(smoke, 0);
        m.sh(smoke + 2, ((seed & 7) + 0x10) as u16);
        store_point(m, smoke, ahead);
        let random = sdk::rand(m);
        let drift = |v: u32, r: u32| sra(v, 8).wrapping_sub(0x40).wrapping_add(r & 0x7f);
        m.sh(smoke + 0x10, drift(vx, random) as u16);
        m.sh(smoke + 0x12, drift(vy, sra(random, 3)) as u16);
        m.sh(smoke + 0x14, drift(vz, sra(random, 6)) as u16);
        let life = if random & 1 != 0 {
            m.sh(smoke + 0x16, 0x8d);
            m.sh(smoke + 0x18, 0x1504);
            (random & 0x3f) + 0x96
        } else {
            m.sh(smoke + 0x16, 0x18c);
            m.sh(smoke + 0x18, 0);
            (random & 7) + 0x14
        };
        m.sh(smoke + 0x1a, life as u16);
    });
}

/// 80081760 (position, velocity): spark kind 2: a flash and a smoke
/// particle one velocity step ahead (<< 9 scatter), thrown back against the
/// velocity (>> 11) and up.
pub fn spark_80118(m: &mut Machine, position: u32, velocity: u32) {
    with_frame(m, 0x30, |m, _| {
        let velocity = load_point(m, velocity);
        let mut ahead = [0u32; 3];
        let mut back = [0u32; 3];
        for axis in 0..3 {
            ahead[axis] = m
                .lw(position + axis as u32 * 4)
                .wrapping_add(velocity[axis]);
            back[axis] = sra(velocity[axis].wrapping_neg(), 11);
        }
        type Style = (u32, u32, u32, u32, u32, u32, u32, u32);
        // (pool, count, life mask, life base, spread mask, centre, size, kind)
        let styles: [Style; 2] = [
            (0x600, 3, 0x1f, 0x1e, 0xff, 0x80, 0x210, 0x84),
            (0x1098, 10, 0xf, 0x12, 0x7f, 0x40, 0x1604, 0x83),
        ];
        for (index, (pool, count, life_mask, life_base, mask, centre, size, kind)) in
            styles.into_iter().enumerate()
        {
            let part = particle(m, pool, count, 7);
            if part == 0 {
                continue;
            }
            let random = sdk::rand(m);
            let mixed = scramble(random);
            let point = scatter(ahead, random, 9);
            m.sw(part + 4, point[0]);
            m.sh(
                part + 0x10,
                back[0].wrapping_sub(centre).wrapping_add(mixed & mask) as u16,
            );
            m.sh(part + 0x12, back[1].wrapping_sub(0x100) as u16);
            let vz = back[2]
                .wrapping_sub(centre)
                .wrapping_add(sra(mixed, 7) & mask);
            m.sh(part + 0x16, kind as u16);
            m.sh(part + 0x18, size as u16);
            let life = if index == 0 {
                (mixed & 0x7f) + 0x78
            } else {
                (mixed & 0x3f) + 0x64
            };
            m.sh(part, 0);
            m.sh(part + 2, ((random & life_mask) + life_base) as u16);
            m.sw(part + 8, point[1]);
            m.sw(part + 0xc, point[2]);
            m.sh(part + 0x14, vz as u16);
            m.sh(part + 0x1a, life as u16);
        }
    });
}

/// 80081994 (position, velocity): spark kind 3: a smoke particle one
/// velocity step back (<< 8 scatter) thrown back (>> 7) and up, and on odd
/// seeds a flash there.
pub fn spark_8034c(m: &mut Machine, position: u32, velocity: u32) {
    with_frame(m, 0x38, |m, _| {
        let velocity = load_point(m, velocity);
        let mut behind = [0u32; 3];
        let mut back = [0u32; 3];
        for axis in 0..3 {
            behind[axis] = m
                .lw(position + axis as u32 * 4)
                .wrapping_sub(velocity[axis]);
            back[axis] = sra(velocity[axis].wrapping_neg(), 7);
        }
        let mut seed = sdk::rand(m);
        let smoke = particle(m, 0x1098, 10, 7);
        if smoke != 0 {
            let random = seed;
            seed = scramble(random);
            let point = scatter(behind, random, 8);
            m.sw(smoke + 4, point[0]);
            m.sw(smoke + 8, point[1]);
            let low = seed & 0xff;
            m.sh(
                smoke + 0x10,
                back[0].wrapping_sub(0x80).wrapping_add(low) as u16,
            );
            m.sh(smoke + 0x12, back[1].wrapping_sub(0xc0) as u16);
            let vz = back[2].wrapping_sub(0x80).wrapping_add(sra(seed, 7) & 0xff);
            m.sh(smoke + 0x16, 0x86);
            m.sh(smoke, 0);
            m.sh(smoke + 2, ((random & 0xf) + 0x1e) as u16);
            m.sw(smoke + 0xc, point[2]);
            m.sh(smoke + 0x14, vz as u16);
            m.sh(smoke + 0x18, 0x1804);
            m.sh(smoke + 0x1a, (low + 0xc8) as u16);
        }
        if seed & 1 == 0 {
            return;
        }
        let flash = particle(m, 0x600, 3, 7);
        if flash == 0 {
            return;
        }
        let random = sdk::rand(m);
        let mixed = scramble(random);
        let point = scatter(behind, random, 8);
        m.sh(flash, 0xffff);
        m.sw(flash + 8, point[1]);
        let low = mixed & 0x7f;
        m.sh(
            flash + 0x10,
            back[0].wrapping_sub(0x40).wrapping_add(low) as u16,
        );
        m.sh(flash + 0x12, back[1].wrapping_sub(0xc0) as u16);
        m.sw(flash + 4, point[0]);
        let vz = back[2]
            .wrapping_sub(0x40)
            .wrapping_add(sra(mixed, 7) & 0x7f);
        m.sh(flash + 0x16, 0x87);
        m.sh(flash + 2, ((random & 0x1f) + 0x32) as u16);
        m.sw(flash + 0xc, point[2]);
        m.sh(flash + 0x14, vz as u16);
        m.sh(flash + 0x18, 0x210);
        m.sh(flash + 0x1a, (low + 0xc8) as u16);
    });
}

/// 80083554 (position, variant): trail mode 0: a rising smoke puff below
/// the position and, with a variant, a column of five flashes climbing
/// from it.
pub fn trail_smoke_81f0c(m: &mut Machine, position: u32, variant: u32) {
    with_frame(m, 0x30, |m, _| {
        let y = m.lw(position + 4).wrapping_add(0xffbe_6000);
        let x = m.lw(position);
        let z = m.lw(position + 8);
        let puff = particle(m, 0x1098, 10, 7);
        if puff != 0 {
            let random = sdk::rand(m);
            m.sh(puff, 0xa);
            m.sh(puff + 2, ((random & 0xf) + 0x28) as u16);
            m.sh(puff + 0x10, (random & 0x3f) as u16);
            m.sh(puff + 0x12, (-0x160_i32) as u16);
            m.sh(puff + 0x14, (sra(random, 6) & 0x3f) as u16);
            m.sh(puff + 0x16, 0x85);
            store_point(m, puff, [x, y, z]);
            m.sh(puff + 0x18, 0x1804);
            m.sh(puff + 0x1a, ((random & 0x7f) + 0xc8) as u16);
        }
        if variant == 0 {
            return;
        }
        let mut height = y;
        let mut rise: u32 = (-0x140_i32) as u32;
        let mut size = 0xc8u32;
        for index in 0..5u32 {
            let flash = particle(m, 0x600, 3, 7);
            if flash == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mixed = scramble(random);
            let py = height.wrapping_add(spread(sra(random, 3), 0xff, 0x80, 8));
            height = height.wrapping_add(0xfffb_0000);
            let lift = rise.wrapping_sub(0x80).wrapping_add(sra(mixed, 3) & 0xff);
            rise = rise.wrapping_sub(0x60).wrapping_sub(mixed & 0xf);
            let life = size.wrapping_add(mixed & 0x3f);
            size = size.wrapping_add(0x46).wrapping_add(mixed & 7);
            let age = (index << 2) + (random & 3) + 0x10;
            let px = x.wrapping_add(spread(random, 0xff, 0x80, 8));
            let pz = z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 8));
            m.sh(flash, 0xffff);
            m.sh(flash + 0x10, spread(mixed, 0xff, 0x80, 0) as u16);
            m.sh(flash + 0x14, spread(sra(mixed, 6), 0xff, 0x80, 0) as u16);
            m.sh(flash + 0x16, 0x85);
            m.sh(flash + 0x18, 0x100);
            m.sh(flash + 2, age as u16);
            store_point(m, flash, [px, py, pz]);
            m.sh(flash + 0x12, lift as u16);
            m.sh(flash + 0x1a, life as u16);
        }
    });
}

/// 80083764 (position): trail mode 1: two still smoke puffs above the
/// position.
pub fn trail_puffs_8211c(m: &mut Machine, position: u32) {
    with_frame(m, 0x30, |m, _| {
        let x = m.lw(position);
        let y = m.lw(position + 4).wrapping_add(0xfff1_0000);
        let z = m.lw(position + 8);
        let mut size = 0xdcu32;
        for index in 0..2u32 {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                return;
            }
            let random = sdk::rand(m);
            let life = size.wrapping_add(random & 0x7f);
            size = size.wrapping_add(0x50);
            m.sh(puff + 2, (index + (random & 3) + 0x1e) as u16);
            m.sh(puff + 0x16, 0x18d);
            m.sh(puff + 0x18, 0x1e00);
            m.sh(puff, 0);
            store_point(m, puff, [x, y, z]);
            m.sh(puff + 0x10, 0);
            m.sh(puff + 0x12, 0);
            m.sh(puff + 0x14, 0);
            m.sh(puff + 0x1a, life as u16);
        }
    });
}

/// The large rising smoke of trail modes 2 and 3: scattered (<< 10),
/// thrown up (-0x190) with a 7-bit drift.
fn trail_column(m: &mut Machine, origin: [u32; 3], life: (u32, u32), size: u32, base: u32) {
    let smoke = particle(m, 0x1098, 10, 7);
    if smoke == 0 {
        return;
    }
    let random = sdk::rand(m);
    let mixed = scramble(random);
    let point = scatter(origin, random, 10);
    m.sh(smoke, 0xa);
    m.sh(smoke + 0x10, (mixed.wrapping_add(0x14) & 0x7f) as u16);
    m.sh(
        smoke + 0x14,
        (sra(mixed, 6).wrapping_add(0x32) & 0x7f) as u16,
    );
    m.sh(smoke + 0x12, (-0x190_i32) as u16);
    m.sh(smoke + 0x16, 0x87);
    m.sh(smoke + 0x18, size as u16);
    m.sh(smoke + 2, ((random & life.0) + life.1) as u16);
    store_point(m, smoke, point);
    m.sh(smoke + 0x1a, ((mixed & 0x7f) + base) as u16);
}

/// The flash of trail modes 2 and 3 (with a variant): scattered by
/// `shift`, rising (-0x190 less `lift` bits).
fn trail_flash(
    m: &mut Machine,
    origin: [u32; 3],
    head: u16,
    life: (u32, u32),
    shift: u32,
    lift: u32,
    base: u32,
) {
    let flash = particle(m, 0x600, 3, 7);
    if flash == 0 {
        return;
    }
    let random = sdk::rand(m);
    m.sh(flash, head);
    m.sh(flash + 2, ((random & life.0) + life.1) as u16);
    store_point(m, flash, scatter(origin, random, shift));
    let random = sdk::rand(m);
    m.sh(flash + 0x10, ((random & 0x7f) + 0x14) as u16);
    m.sh(flash + 0x14, ((sra(random, 7) & 0x7f) + 0x32) as u16);
    m.sh(
        flash + 0x12,
        (-0x190_i32 as u32).wrapping_sub(random & lift) as u16,
    );
    m.sh(flash + 0x16, 0x80);
    m.sh(flash + 0x18, 0x210);
    m.sh(flash + 0x1a, ((sra(random, 4) & 0x7f) + base) as u16);
}

/// 80083844 (position, variant): trail mode 2: a large rising smoke and,
/// with a variant, a flash.
pub fn trail_fire_821fc(m: &mut Machine, position: u32, variant: u32) {
    with_frame(m, 0x28, |m, _| {
        let origin = load_point(m, position);
        trail_column(m, origin, (0xf, 0x28), 0x1a04, 0x1f4);
        if variant != 0 {
            trail_flash(m, origin, 0xa, (0xf, 0x64), 11, 0xf, 0x1f4);
        }
    });
}

/// 80083A40 (position, variant): trail mode 3: a smaller rising smoke and,
/// with a variant, a flash.
pub fn trail_fire_823f8(m: &mut Machine, position: u32, variant: u32) {
    with_frame(m, 0x28, |m, _| {
        let origin = load_point(m, position);
        trail_column(m, origin, (7, 0x1e), 0x1904, 0x190);
        if variant != 0 {
            trail_flash(m, origin, 8, (7, 0x44), 10, 0x1f, 0x190);
        }
    });
}

// ---- phase 4: the water impact handlers of 80080EB4 (replay.rs) ------------------------------

/// 80080A94 (x, y, z): a water ring: a flat shock wave (0x0029000A,
/// 0x03200002) and twenty spray puffs (<< 8 scatter).
pub fn water_ring_7f44c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        let wave = particle(m, 0x33c0, 1, 6);
        if wave != 0 {
            m.sw(wave, 0x0029_000a);
            store_point(m, wave, [x, y, z]);
            m.sw(wave + 0x14, 0x0320_0002);
        }
        let mut life = 0x320u32;
        for index in 0..20u32 {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                return;
            }
            m.sh(puff, 0);
            m.sh(puff + 2, ((index & 3) + 0x3c) as u16);
            let random = sdk::rand(m);
            let mixed = scramble(random);
            m.sh(puff + 0x1a, life as u16);
            life += 2;
            let point = scatter([x, y, z], random, 8);
            m.sh(puff + 0x10, (spread(mixed, 0xff, 0x80, 0) << 1) as u16);
            m.sh(
                puff + 0x12,
                (sra(mixed, 2) & 0x1fe).wrapping_sub(0x180) as u16,
            );
            m.sh(
                puff + 0x14,
                (spread(sra(mixed, 6), 0xff, 0x80, 0) << 1) as u16,
            );
            m.sh(puff + 0x16, 0x18e);
            m.sh(puff + 0x18, 0x1a02);
            store_point(m, puff, point);
        }
    });
}

/// 80080C08 (x, y, z): a water splash: a rising column flash with seven
/// spray flashes around it, a shock wave and ten falling spray puffs above
/// the surface.
pub fn water_splash_7f5c0(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        'flashes: {
            let column = particle(m, 0x600, 3, 7);
            if column == 0 {
                break 'flashes;
            }
            let random = sdk::rand(m);
            m.sh(column, 0xffff);
            m.sh(column + 2, ((random & 7) + 0x28) as u16);
            m.sh(column + 0x16, 0x95);
            m.sh(
                column + 0x12,
                (-0x200_i32 as u32).wrapping_sub(random & 0x3f) as u16,
            );
            m.sh(column + 0x18, 0x210);
            store_point(m, column, [x, y, z]);
            m.sh(column + 0x10, 0);
            m.sh(column + 0x14, 0);
            m.sh(column + 0x1a, ((random & 0x7f) + 0x2d0) as u16);
            let mut head = 0xffffu32;
            for _ in 0..7 {
                let spray = particle(m, 0x600, 3, 7);
                if spray == 0 {
                    break 'flashes;
                }
                let random = sdk::rand(m);
                m.sh(spray, head as u16);
                head = head.wrapping_sub(2);
                m.sh(spray + 2, ((random & 3) + 0xc) as u16);
                m.sw(spray + 4, x.wrapping_add(spread(random, 0xff, 0x80, 14)));
                m.sw(
                    spray + 0xc,
                    z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 14)),
                );
                m.sh(spray + 0x18, 0x100);
                m.sh(spray + 0x1a, ((sra(random, 7) & 0x7f) + 0x1f4) as u16);
                m.sw(spray + 8, y);
                m.sh(spray + 0x16, 0);
            }
        }
        let wave = particle(m, 0x33c0, 1, 6);
        if wave != 0 {
            let random = sdk::rand(m);
            m.sh(wave + 2, ((random & 3) + 0x18) as u16);
            m.sh(wave + 0x14, 1);
            m.sh(wave + 0x16, ((random & 0x7f) + 0x4b0) as u16);
            m.sh(wave, 0);
            store_point(m, wave, [x, y, z]);
            m.sw(wave + 0x10, 0x400);
        }
        let height = y.wrapping_add(0xfffb_0000);
        for _ in 0..10 {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                return;
            }
            let random = sdk::rand(m);
            m.sh(puff + 2, ((random & 0xf) + 0x1e) as u16);
            m.sw(puff + 4, x.wrapping_add(spread(random, 0xff, 0x80, 13)));
            let pz = z.wrapping_add(spread(sra(random, 4), 0xff, 0x80, 13));
            m.sh(puff, 0);
            m.sw(puff + 8, height);
            m.sw(puff + 0xc, pz);
            let random = sdk::rand(m);
            m.sh(puff + 0x10, (spread(random, 0xff, 0x80, 0) << 1) as u16);
            m.sh(
                puff + 0x12,
                (sra(random, 2) & 0x1fe).wrapping_sub(0x240) as u16,
            );
            m.sh(
                puff + 0x14,
                (spread(sra(random, 6), 0xff, 0x80, 0) << 1) as u16,
            );
            m.sh(puff + 0x16, 0x8d);
            m.sh(puff + 0x18, 0x1c04);
            m.sh(puff + 0x1a, ((sra(random, 8) & 0x7f) + 0x320) as u16);
        }
    });
}

// ---- phase 4: the destruction effects (table 80024874) and their timer events ---------------
//
// effects.rs's 80089BB4 calls the table entries with the position's x, y, z
// (and table entry 20 in a3). Many leave a timer event (pool +0x36C4,
// 0x14-byte record: count, limit, point, callback) that entities.rs's
// 80089D70 runs with (count, record).

/// The flight is live (gameplay word 0 bit 1): the follow-up events and
/// sounds only run then.
fn flight_live(m: &mut Machine) -> bool {
    let gameplay = m.gw(common::GAMEPLAY);
    m.lw(gameplay) & 2 != 0
}

/// A timer event: header word (count, limit), the point (when given) and
/// the callback.
fn timer_event(m: &mut Machine, header: u32, point: Option<[u32; 3]>, callback: u32) {
    let record = particle(m, 0x36c4, 1, 5);
    if record == 0 {
        return;
    }
    m.sw(record, header);
    if let Some(point) = point {
        store_point(m, record, point);
    }
    m.sw(record + 0x10, callback);
}

/// The follow-up trail event of the destruction effects (live flights
/// only).
fn trail_event(m: &mut Machine, header: u32, point: [u32; 3], callback: u32) {
    if flight_live(m) {
        timer_event(m, header, Some(point), callback);
    }
}

/// The crash sound 0x18 at the event's point (8002F460, shift 1).
fn crash_sound(m: &mut Machine, record: u32) {
    if flight_live(m) {
        script_sound_2f044(m, 0x18, record + 4, 1);
    }
}

/// 80087584 (x, y, z): flash and sparks, sixteen debris chunks and a slow
/// shock wave.
pub fn blast_85e14(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        flash_sparks_85aac(m, x, y, z);
        debris_85c70(m, x, y, z);
        let wave = particle(m, 0x33c0, 1, 6);
        if wave == 0 {
            return;
        }
        let random = sdk::rand(m);
        m.sh(wave, 0);
        m.sh(wave + 2, ((random & 3) + 0xc) as u16);
        store_point(m, wave, [x, y, z]);
        m.sh(wave + 0x14, 0);
        m.sh(wave + 0x16, ((random & 0xff) + 0x4b0) as u16);
    });
}

/// 80087C7C (x, y, z): a fireball: three stacked flashes (<< 13 scatter)
/// and nine rising sparks above (<< 14).
pub fn fireball_8650c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        let mut rise: u32 = (-0x180_i32) as u32;
        let mut height = y;
        for index in 0..3u32 {
            let flash = particle(m, 0x600, 3, 7);
            if flash == 0 {
                return;
            }
            let random = sdk::rand(m);
            m.sh(flash, 0xffff);
            m.sh(flash + 2, (index + 0x2a) as u16);
            store_point(m, flash, scatter([x, height, z], random, 13));
            let random = sdk::rand(m);
            let lift = rise.wrapping_sub(0x80).wrapping_add(sra(random, 3) & 0xff);
            rise = rise.wrapping_sub(0x64);
            height = height.wrapping_add(0xfff0_0000);
            let low = random & 0xff;
            m.sh(flash + 0x10, low.wrapping_sub(0x80) as u16);
            m.sh(flash + 0x14, spread(sra(random, 6), 0xff, 0x80, 0) as u16);
            m.sh(flash + 0x16, 0x86);
            m.sh(flash + 0x18, 0x210);
            m.sh(flash + 0x12, lift as u16);
            m.sh(flash + 0x1a, (low + 0x384) as u16);
        }
        let mut rise: u32 = (-0x154_i32) as u32;
        let mut height = y.wrapping_add(0x1f_4000);
        for index in 0..9u32 {
            let spark = particle(m, 0x600, 3, 7);
            if spark == 0 {
                return;
            }
            let random = sdk::rand(m);
            m.sh(spark, 0xffff);
            store_point(m, spark, scatter([x, height, z], random, 14));
            m.sh(spark + 2, ((index << 1) + (random & 7) + 0x18) as u16);
            let random = sdk::rand(m);
            m.sh(spark + 0x12, rise as u16);
            rise = rise.wrapping_sub(0x62).wrapping_sub(random & 7);
            height = height.wrapping_add(0xfffb_0000);
            m.sh(spark + 0x10, (spread(random, 0xff, 0x80, 0) << 1) as u16);
            m.sh(
                spark + 0x14,
                (spread(sra(random, 7), 0xff, 0x80, 0) << 1) as u16,
            );
            m.sh(spark + 0x16, 0x85);
            m.sh(spark + 0x18, 0x100);
            let life = (index << 7) + (sra(random, 2) & 0x7f) + 0x1f0;
            m.sh(spark + 0x1a, life as u16);
        }
    });
}

/// 80087EE0 (x, y, z): a smoke ring: sixteen large slow puffs (<< 12
/// scatter) and eight smaller ones.
pub fn smoke_ring_86770(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        let mut rise: u32 = (-0x12c_i32) as u32;
        for index in 0..16u32 {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                break;
            }
            let random = sdk::rand(m);
            let mixed = scramble(random);
            let lift = (spread(sra(mixed, 3), 0xff, 0x80, 0) << 2).wrapping_add(rise);
            rise = rise.wrapping_sub(0x64);
            let point = scatter([x, y, z], random, 12);
            m.sh(puff, 0xa);
            m.sh(puff + 0x10, (spread(mixed, 0xff, 0x80, 0) << 2) as u16);
            m.sh(
                puff + 0x14,
                (spread(sra(mixed, 6), 0xff, 0x80, 0) << 2) as u16,
            );
            m.sh(puff + 0x16, 0x96);
            m.sh(puff + 0x18, 0x1c04);
            let life = (index << 5) + (sra(mixed, 1) & 0xff) + 0x4b0;
            m.sh(puff + 2, (index + 0x4a) as u16);
            store_point(m, puff, point);
            m.sh(puff + 0x12, lift as u16);
            m.sh(puff + 0x1a, life as u16);
        }
        for index in 0..8u32 {
            let puff = particle(m, 0x1098, 10, 7);
            if puff == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mixed = scramble(random);
            let point = scatter([x, y, z], random, 12);
            m.sh(puff + 2, (index + 0x32) as u16);
            m.sh(puff + 0x10, (spread(mixed, 0xff, 0x80, 0) << 1) as u16);
            let lift = (spread(sra(mixed, 3), 0xff, 0x80, 0) << 1).wrapping_sub(0x100);
            m.sh(puff + 0x12, lift as u16);
            m.sh(
                puff + 0x14,
                (spread(sra(mixed, 6), 0xff, 0x80, 0) << 1) as u16,
            );
            m.sh(puff + 0x16, 0x95);
            let life = (index << 1) + (mixed & 0x1f) + 0x28;
            m.sh(puff, 0);
            store_point(m, puff, point);
            m.sh(puff + 0x18, 0);
            m.sh(puff + 0x1a, life as u16);
        }
    });
}

/// 8008812C (count, record): the screen flash event of 800881BC: the
/// white-out level `(24 - count) * 2560 / 24`; below 256 the event ends,
/// else the full-screen fade 80093C90 at the OT's +8 and effect block
/// +0x5C8 = 1.
pub fn flash_event_869bc(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x18, |m, _| {
        let scaled = 0x18u32.wrapping_sub(count).wrapping_mul(5) << 9;
        let level = sra(hi(scaled, 0x2aaa_aaab), 2).wrapping_sub(sra(scaled, 31));
        if lt(level, 0x100) {
            let limit = m.lhu(record + 2);
            m.sh(record, limit);
            return;
        }
        let gameplay = m.gw(common::GAMEPLAY);
        let ot = m.lw(gameplay + 0x10);
        let prim = m.lw(gameplay + 0x14);
        let prim = original(m, 0x8009_3c90, [ot + 8, prim, level, 0]);
        let gameplay = m.gw(common::GAMEPLAY);
        m.sw(gameplay + 0x14, prim);
        let effects = m.gw(GP_EFFECTS);
        m.sw(effects + 0x5c8, 1);
    });
}

/// 800881BC (x, y, z): the huge blast: fireball, smoke ring, a large shock
/// wave (0x0BB8001E) and the screen flash event (limit 0x18).
pub fn blast_86a4c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        fireball_8650c(m, x, y, z);
        smoke_ring_86770(m, x, y, z);
        shock_wave_859f8(m, x, y, z, 0x0bb8_001e);
        timer_event(m, 0x0018_ffff, None, 0x8008_812c);
    });
}

/// 80088254 (x, y, z): a burning debris burst: a flash and three large
/// rising smoke particles (<< 12 scatter).
pub fn debris_burst_86ae4(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        let flash = particle(m, 0x600, 3, 7);
        if flash != 0 {
            let random = sdk::rand(m);
            m.sh(flash, !(random & 3) as u16);
            m.sh(flash + 2, ((random & 0x1f) + 0x20) as u16);
            store_point(m, flash, scatter([x, y, z], random, 12));
            let random = sdk::rand(m);
            m.sh(flash + 0x10, (random & 0x7f).wrapping_sub(0x40) as u16);
            m.sh(
                flash + 0x12,
                (sra(random, 3) & 0x7f).wrapping_sub(0x140) as u16,
            );
            m.sh(
                flash + 0x14,
                (sra(random, 6) & 0x7f).wrapping_sub(0x40) as u16,
            );
            m.sh(flash + 0x16, 0x84);
            m.sh(flash + 0x18, 0x210);
            m.sh(flash + 0x1a, ((random & 0xff) + 0x258) as u16);
        }
        for _ in 0..3 {
            let smoke = particle(m, 0x1098, 10, 7);
            if smoke == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mixed = scramble(random);
            let point = scatter([x, y, z], random, 12);
            m.sh(smoke, 8);
            m.sh(smoke + 0x10, (mixed & 0x7f).wrapping_sub(0x40) as u16);
            m.sh(
                smoke + 0x12,
                (sra(mixed, 3) & 0x7f).wrapping_sub(0x230) as u16,
            );
            m.sh(
                smoke + 0x14,
                (sra(mixed, 6) & 0x7f).wrapping_sub(0x40) as u16,
            );
            m.sh(smoke + 0x16, 0x8d);
            m.sh(smoke + 0x18, 0x1804);
            m.sh(smoke + 0x1a, ((mixed & 0xff) + 0x2bc) as u16);
            m.sh(smoke + 2, ((random & 0x1f) + 0x30) as u16);
            store_point(m, smoke, point);
        }
    });
}

/// 80088454 (count, record): the burning wreck event of 800885DC: a smoke
/// puff every fourth frame before 38, and at 90 five debris bursts along
/// the wreck with the crash sound.
pub fn burning_event_86ce4(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x30, |m, _| {
        if lt(count, 0x26) && count & 3 == 0 {
            let random = sdk::rand(m);
            let smoke = particle(m, 0x1098, 10, 7);
            if smoke != 0 {
                m.sh(smoke, 0);
                m.sh(smoke + 2, ((random & 0x1f) + 0x1e) as u16);
                let x = m.lw(record + 4).wrapping_add(0xff80_0000);
                m.sw(smoke + 4, x);
                let y = m.lw(record + 8);
                m.sw(smoke + 8, y);
                let z = m.lw(record + 0xc);
                let low = random & 0xff;
                m.sh(smoke + 0x10, low.wrapping_sub(0x80) as u16);
                m.sh(
                    smoke + 0x12,
                    (sra(random, 3) & 0xff).wrapping_sub(0x90) as u16,
                );
                m.sh(smoke + 0x14, spread(sra(random, 6), 0xff, 0x80, 0) as u16);
                m.sh(smoke + 0x16, 0x88);
                m.sh(smoke + 0x18, 0x1704);
                m.sh(smoke + 0x1a, (low + 0xc8) as u16);
                m.sw(smoke + 0xc, z);
            }
        }
        if count != 0x5a {
            return;
        }
        let z = m.lw(record + 0xc);
        let mut x = m.lw(record + 4).wrapping_add(0xffe0_0000);
        let mut y = m.lw(record + 8).wrapping_add(0x005e_6000);
        for _ in 0..5 {
            debris_burst_86ae4(m, x, y, z);
            x = x.wrapping_add(0xffec_0000);
            y = y.wrapping_add(0xfff8_8000);
        }
        crash_sound(m, record);
    });
}

/// 800885DC (x, y, z): the downed aircraft: a blast (80087584) to the
/// side, eight drifting smoke puffs alternating left and right, and the
/// burning wreck event (limit 0x5B).
pub fn blast_86e6c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        blast_85e14(m, x.wrapping_add(0x1_e000), y.wrapping_add(0x17_c000), z);
        let right = x.wrapping_add(0x40_0000);
        let left = x.wrapping_add(0xff80_0000);
        for index in 0..8u32 {
            let random = sdk::rand(m);
            let smoke = particle(m, 0x1098, 10, 7);
            if smoke == 0 {
                break;
            }
            m.sh(smoke + 2, ((random & 0xf) + 0x28) as u16);
            m.sh(smoke, 0);
            m.sw(smoke + 4, if index & 1 != 0 { right } else { left });
            let low = random & 0xff;
            m.sh(smoke + 0x10, low.wrapping_sub(0x80) as u16);
            m.sh(
                smoke + 0x12,
                (sra(random, 3) & 0xff).wrapping_sub(0xb0) as u16,
            );
            m.sh(smoke + 0x14, spread(sra(random, 6), 0xff, 0x80, 0) as u16);
            m.sh(smoke + 0x16, 0x88);
            m.sh(smoke + 0x18, 0x1804);
            m.sw(smoke + 8, y);
            m.sw(smoke + 0xc, z);
            m.sh(smoke + 0x1a, (low + 0x12c) as u16);
        }
        timer_event(m, 0x005b_ffff, Some([x, y, z]), 0x8008_8454);
    });
}

/// 80088748 (count, record): the crash site event of 80088968: the crash
/// sound at 80, flashes marching across the site for frames 80..95, and
/// before 80 random flashes or debris around it.
pub fn crash_event_86fd8(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x38, |m, _| {
        if count == 0x50 {
            crash_sound(m, record);
        }
        let step = count.wrapping_sub(0x50);
        if step < 0x10 {
            let stride = step.wrapping_mul(225) << 11;
            let mut z = m.lw(record + 0xc);
            let mut x = m.lw(record + 4).wrapping_sub(stride);
            let y = m.lw(record + 8).wrapping_add(0x3e_8000);
            for index in 0..4u32 {
                let flash = particle(m, 0x600, 3, 7);
                if flash == 0 {
                    break;
                }
                let random = sdk::rand(m);
                m.sh(flash, 0xffff);
                m.sh(flash + 2, ((random & 0xf) + 0x14) as u16);
                let low = random & 0xff;
                let px = x.wrapping_add(low.wrapping_sub(0x80) << 11);
                let pz = z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 11));
                m.sw(flash + 0xc, pz);
                m.sh(flash + 0x18, 0x130);
                m.sw(flash + 4, px);
                m.sw(flash + 8, y);
                m.sh(flash + 0x16, 0);
                m.sh(flash + 0x1a, (low + 0x140) as u16);
                if index == 1 {
                    x = x.wrapping_add(stride << 1);
                }
                z = z.wrapping_add(if index & 1 != 0 {
                    0x005d_c000
                } else {
                    0xffa2_4000
                });
            }
        }
        if !lt(count, 0x50) {
            return;
        }
        let random = sdk::rand(m);
        let lift = count.wrapping_mul(3) << 14;
        let mut x = m.lw(record + 4);
        let y = m.lw(record + 8).wrapping_add(lift);
        let mut z = m.lw(record + 0xc);
        if random & 1 != 0 {
            z = z.wrapping_add(0xffa2_4000);
        }
        x = x.wrapping_add((random & 0xfff).wrapping_sub(0x800) << 11);
        if random & 2 != 0 {
            flash_85fec(m, x, y, z);
        } else if count & 3 != 0 {
            debris_85ebc(m, x, y, z);
        } else {
            debris_85c70(m, x, y.wrapping_add(0x5_0000), z);
        }
    });
}

/// 80088968 (x, y, z): the large crash: two rows of a blast with two
/// flashes beside it, and the crash site event (limit 0x61).
pub fn blast_871f8(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x38, |m, _| {
        let height = y.wrapping_add(0xc_8000);
        let mut row = z;
        for _ in 0..2 {
            blast_85e14(m, x, height.wrapping_add(0xc_8000), row);
            flash_sparks_85aac(m, x.wrapping_add(0xff8f_8000), height, row);
            flash_sparks_85aac(m, x.wrapping_add(0x70_8000), height, row);
            row = row.wrapping_add(0xffa2_4000);
        }
        timer_event(m, 0x0061_ffff, Some([x, y, z]), 0x8008_8748);
    });
}

/// 80088A84 (count, record): the collapse event: on its last frame four
/// debris bursts in a row and the crash sound.
pub fn collapse_event_87314(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x30, |m, _| {
        let last = (m.lh(record + 2) as i32 as u32).wrapping_sub(1);
        if count != last {
            return;
        }
        let y = m.lw(record + 8);
        let mut x = m.lw(record + 4).wrapping_add(0xffe0_c000);
        let z = m.lw(record + 0xc);
        for _ in 0..4 {
            debris_burst_86ae4(m, x, y, z);
            x = x.wrapping_add(0xf_a000);
        }
        crash_sound(m, record);
    });
}

/// 80088B48 (x, y, z): flames: two stacked flashes and five rising sparks
/// (<< 12 scatter).
pub fn flames_873d8(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x30, |m, _| {
        let mut rise: u32 = (-0x64_i32) as u32;
        let mut height = y;
        for index in 0..2u32 {
            let flash = particle(m, 0x600, 3, 7);
            if flash == 0 {
                return;
            }
            let random = sdk::rand(m);
            m.sh(flash + 0x12, rise as u16);
            rise = rise.wrapping_sub(0xdc);
            m.sw(flash + 8, height);
            height = height.wrapping_add(0xfff0_0000);
            m.sh(flash, 0xffff);
            m.sh(flash + 2, ((index << 1) + (random & 7) + 0x28) as u16);
            let low = random & 0xff;
            m.sh(flash + 0x10, low.wrapping_sub(0x80) as u16);
            m.sh(flash + 0x14, spread(sra(random, 7), 0xff, 0x80, 0) as u16);
            m.sh(flash + 0x16, 0x87);
            m.sh(flash + 0x18, 0x210);
            m.sw(flash + 4, x);
            m.sw(flash + 0xc, z);
            m.sh(flash + 0x1a, ((index << 8) + low + 0x1f4) as u16);
        }
        let mut rise: u32 = (-0x64_i32) as u32;
        let mut height = y;
        for index in 0..5u32 {
            let spark = particle(m, 0x600, 3, 7);
            if spark == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mixed = scramble(random);
            m.sh(spark + 0x12, rise as u16);
            rise = rise.wrapping_sub(0x6e).wrapping_sub(mixed & 7);
            let py = height.wrapping_add(spread(sra(random, 3), 0xff, 0x80, 12));
            height = height.wrapping_add(0xfffa_6000);
            let age = index + (random & 3) + 0x18;
            let px = x.wrapping_add(spread(random, 0xff, 0x80, 12));
            let pz = z.wrapping_add(spread(sra(random, 6), 0xff, 0x80, 12));
            m.sh(spark, 0xffff);
            m.sh(spark + 0x10, spread(mixed, 0xff, 0x80, 0) as u16);
            m.sh(spark + 0x14, spread(sra(mixed, 7), 0xff, 0x80, 0) as u16);
            m.sh(spark + 0x16, 0x85);
            m.sh(spark + 0x18, 0x100);
            let life = (index << 7) + (sra(mixed, 4) & 0x7f) + 0xf0;
            m.sh(spark + 2, age as u16);
            store_point(m, spark, [px, py, pz]);
            m.sh(spark + 0x1a, life as u16);
        }
    });
}

/// 80088D60 (x, y, z): eighteen debris chunks, the first twelve large and
/// thrown up harder in turn.
pub fn debris_875f0(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x28, |m, _| {
        for index in 0..18u32 {
            let chunk = particle(m, 0x1098, 10, 7);
            if chunk == 0 {
                return;
            }
            let random = sdk::rand(m);
            let mut lift = blast_chunk(m, chunk, [x, y, z], random, 0x80).wrapping_sub(0x180);
            let size = if index < 12 {
                lift = lift.wrapping_sub(index << 5);
                m.sh(chunk, 8);
                m.sh(chunk + 2, ((random & 0xf) + index + 0x2c) as u16);
                m.sh(chunk + 0x18, 0x1804);
                m.sh(chunk + 0x1a, ((random & 0xff) + 0x258) as u16);
                0x8d
            } else {
                let low = random & 0xf;
                m.sh(chunk + 0x1a, (low + 0x19) as u16);
                m.sh(chunk, 0);
                m.sh(chunk + 2, (low + 0x24) as u16);
                m.sh(chunk + 0x18, 0);
                0x195
            };
            m.sh(chunk + 0x16, size);
            m.sh(chunk + 0x12, lift as u16);
        }
    });
}

/// 80088EA8 (x, y, z): the building collapse: flames, eighteen debris
/// chunks, a shock wave (0x07080018), the collapse event (limit 0x79)
/// beside it and the smoke trail event (884D0, limit 0x384).
pub fn blast_87738(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        flames_873d8(m, x, y, z);
        debris_875f0(m, x, y, z);
        shock_wave_859f8(m, x, y, z, 0x0708_0018);
        let side = [x.wrapping_add(0x38_4000), y.wrapping_add(0x3e_8000), z];
        timer_event(m, 0x0079_ffff, Some(side), 0x8008_8a84);
        trail_event(m, 0x0384_0001, [x, y, z], 0x8008_9c40);
    });
}

/// The medium structure collapses 80088FBC/800890D8: flash and sparks,
/// sixteen debris chunks, a shock wave (0x07080018), the collapse event at
/// `side` and the long smoke trail event (884D0, limit 0xE10).
fn collapse_blast(m: &mut Machine, point: [u32; 3], header: u32, side: [u32; 3]) {
    let [x, y, z] = point;
    flash_sparks_85aac(m, x, y, z);
    debris_85c70(m, x, y, z);
    shock_wave_859f8(m, x, y, z, 0x0708_0018);
    timer_event(m, header, Some(side), 0x8008_8a84);
    trail_event(m, 0x0e10_0000, point, 0x8008_9c40);
}

/// 80088FBC (x, y, z): a tower collapse (collapse event 0x8D frames).
pub fn blast_8784c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        let side = [
            x.wrapping_add(0xffda_8000),
            y.wrapping_add(0x3e_8000),
            z.wrapping_add(0x19_0000),
        ];
        collapse_blast(m, [x, y, z], 0x008d_ffff, side);
    });
}

/// 800890D8 (x, y, z): a hangar collapse (collapse event 0x97 frames).
pub fn blast_87968(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        let side = [x, y.wrapping_add(0x1e_a000), z.wrapping_add(0xffda_8000)];
        collapse_blast(m, [x, y, z], 0x0097_ffff, side);
    });
}

/// The fire events 800891EC/80089378: debris every fourth frame before 40
/// (every other one after 20) scattered along one axis, the crash sound at
/// 82, and on odd frames 82..92 debris bursts marching along the other
/// axis (`along_z`: the scatter is along x and the march along z).
fn fire_event(m: &mut Machine, count: u32, record: u32, along_z: bool) {
    if lt(count, 0x28) && count & 3 == 0 {
        let random = sdk::rand(m);
        if lt(count, 0x14) || random & 1 != 0 {
            let scatter = (random & 0xff).wrapping_sub(0x80) << 11;
            let shift = (random & 1) << 22;
            let (dx, dz) = if along_z {
                (scatter, shift)
            } else {
                (shift, scatter)
            };
            let x = m.lw(record + 4).wrapping_add(dx);
            let z = m.lw(record + 0xc).wrapping_add(dz);
            let y = m.lw(record + 8);
            debris_85ebc(m, x, y, z);
        }
    }
    if count == 0x52 {
        crash_sound(m, record);
    }
    let step = count.wrapping_sub(0x52);
    if step < 0xb && count & 1 != 0 {
        let scaled = step << 22;
        let offset = sra(hi(scaled, 0x6666_6667), 2).wrapping_sub(sra(scaled, 31));
        let y = m.lw(record + 8).wrapping_add(0x2d_0000);
        let (dx, dz) = if along_z { (0, offset) } else { (offset, 0) };
        let x = m.lw(record + 4).wrapping_add(dx);
        let z = m.lw(record + 0xc).wrapping_add(dz);
        debris_burst_86ae4(m, x, y, z);
    }
}

/// 800891EC (count, record): the fire event of 800892FC (march along z).
pub fn fire_event_87a7c(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x20, |m, _| fire_event(m, count, record, true));
}

/// 80089378 (count, record): the fire event of 80089488 (march along x).
pub fn fire_event_87c08(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x20, |m, _| fire_event(m, count, record, false));
}

/// 800892FC (x, y, z): the medium blast and fire event 800891EC.
pub fn blast_87b8c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        blast_85db0(m, x, y, z);
        timer_event(m, 0x005d_ffff, Some([x, y, z]), 0x8008_91ec);
    });
}

/// 80089488 (x, y, z): the medium blast and fire event 80089378.
pub fn blast_87d18(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        blast_85db0(m, x, y, z);
        timer_event(m, 0x005d_ffff, Some([x, y, z]), 0x8008_9378);
    });
}

/// The medium blast with a collapse event at `side` and a smoke trail
/// event (8008978C, 80089880).
fn blast_with_collapse(m: &mut Machine, point: [u32; 3], header: u32, side: [u32; 3], trail: u32) {
    let [x, y, z] = point;
    blast_85db0(m, x, y, z);
    timer_event(m, header, Some(side), 0x8008_8a84);
    trail_event(m, trail, point, 0x8008_9c40);
}

/// 8008978C (x, y, z): blast, collapse event (0x5B frames), trail 0x12C.
pub fn blast_8801c(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        let side = [
            x.wrapping_add(0xc_8000),
            y.wrapping_add(0xb_4000),
            z.wrapping_add(0xffe7_0000),
        ];
        blast_with_collapse(m, [x, y, z], 0x005b_ffff, side, 0x012c_0001);
    });
}

/// 80089880 (x, y, z): blast, collapse event (0x59 frames), trail 0x384.
pub fn blast_88110(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        let side = [
            x.wrapping_add(0xf_a000),
            y.wrapping_add(0x22_6000),
            z.wrapping_add(0xffe7_0000),
        ];
        blast_with_collapse(m, [x, y, z], 0x0059_ffff, side, 0x0384_0001);
    });
}

/// 80089974 (x, y, z): the small blast and a smoke trail (884D0, 0x384).
pub fn blast_88204(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        blast_8608c(m, x, y, z);
        trail_event(m, 0x0384_0000, [x, y, z], 0x8008_9c40);
    });
}

/// 80089A04 (x, y, z): the medium blast and a fire trail (88568, 0x384).
pub fn blast_88294(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        blast_85db0(m, x, y, z);
        trail_event(m, 0x0384_0000, [x, y, z], 0x8008_9cd8);
    });
}

/// 80089A94 (x, y, z): the medium blast and a long smoke trail (884D0,
/// 0x2328).
pub fn blast_88324(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        blast_85db0(m, x, y, z);
        trail_event(m, 0x2328_0000, [x, y, z], 0x8008_9c40);
    });
}

/// 80089B24 (x, y, z): the large blast and a long fire trail (88568,
/// 0x1518).
pub fn blast_883b4(m: &mut Machine, x: u32, y: u32, z: u32) {
    with_frame(m, 0x20, |m, _| {
        blast_864a8(m, x, y, z);
        trail_event(m, 0x1518_0000, [x, y, z], 0x8008_9cd8);
    });
}

/// The smoke trail events 80089C40/80089CD8: every eighth frame the trail
/// handler `mode` (variant from bit 3 of the count) at the event point:
/// in a live flight the unit trail 80083CA4 when the player is within
/// 0x7FFF (80083DFC), else the trail handler 80083C3C.
fn smoke_trail_event(m: &mut Machine, count: u32, record: u32, mode: u32) {
    if count & 7 != 0 {
        return;
    }
    let variant = sra(count, 3) & 1;
    let point = record + 4;
    if flight_live(m) {
        let player = entity(m, 0);
        let distance = original(m, 0x8008_3dfc, [point, player + 8, 0, 0]);
        if !lt(0x7fff, distance) {
            original(m, 0x8008_3ca4, [point, variant, mode, 0]);
        }
    } else {
        original(m, 0x8008_3c3c, [point, variant, mode, 0]);
    }
}

/// 80089C40 (count, record): smoke trail event (mode 3).
pub fn trail_event_884d0(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x20, |m, _| smoke_trail_event(m, count, record, 3));
}

/// 80089CD8 (count, record): fire trail event (mode 2).
pub fn trail_event_88568(m: &mut Machine, count: u32, record: u32) {
    with_frame(m, 0x20, |m, _| smoke_trail_event(m, count, record, 2));
}

// ---- phase 4: the formatted-text helpers 80027C24..80027E80 ----------------------------------
//
// Helpers of the resident formatter at 80028074, which nothing in the
// resident code or the overlays calls; translated for completeness.

/// 80027C24 (c, cursor) -> c: store `c` at `[cursor]` and advance it.
pub fn put_char_27784(m: &mut Machine, c: u32, cursor: u32) -> u32 {
    let at = m.lw(cursor);
    m.sb(at, c as u8);
    m.sw(cursor, at + 1);
    c
}

/// 800C1350: the end of the number buffer 80027C3C fills backwards.
const NUMBER_END: u32 = 0x800c_1350;

/// 80027C3C (low, high, flags, nonzero; base, length out and digit table
/// as the caller's stack words +0x10..+0x18) -> first digit: the digits of
/// the 32-bit value `low` (64-bit `high:low` with flags bit 2) in `base`,
/// written backwards ending at 800C1350 (none when `nonzero` is 0),
/// through shifts for bases 2, 8 and 16 and the 64-bit division 8009EA48
/// (`sdk::udivdi3`, called directly for its full 64-bit quotient) otherwise.
pub fn format_number_2779c(m: &mut Machine, low: u32, high: u32, flags: u32, nonzero: u32) -> u32 {
    with_frame(m, 0x50, |m, frame| {
        let base = m.lw(frame + 0x60);
        let digits = m.lw(frame + 0x68);
        let (shift, mask) = match base {
            8 => (3u32, 7u32),
            16 => (4, 0xf),
            2 => (1, 1),
            _ => (0, 0),
        };
        let mut cursor = NUMBER_END;
        if nonzero != 0 {
            if flags & 4 != 0 {
                if shift == 0 {
                    // 80027D54: quotient by 8009EA48 (__udivdi3, divisor the
                    // sign-extended base), digit from the low word of
                    // `value - quotient * base`.
                    let divisor = u64::from(base) | (u64::from((base as i32 >> 31) as u32) << 32);
                    let (mut low, mut high) = (low, high);
                    loop {
                        let value = (u64::from(high) << 32) | u64::from(low);
                        let quotient = crate::sdk::udivdi3(m, value, divisor);
                        let (q_low, q_high) = (quotient as u32, (quotient >> 32) as u32);
                        let remainder = low.wrapping_sub(q_low.wrapping_mul(base));
                        cursor -= 1;
                        let digit = lbu(m, digits.wrapping_add(remainder));
                        low = q_low;
                        high = q_high;
                        m.sb(cursor, digit as u8);
                        if high | low == 0 || m.fault().is_some() {
                            break;
                        }
                    }
                } else {
                    cursor -= 1;
                    let (mut low, mut high) = (low, high);
                    loop {
                        let digit = lbu(m, digits.wrapping_add(mask & low));
                        let next_low = (low >> shift) | (high << (32 - shift));
                        high >>= shift;
                        low = next_low;
                        m.sb(cursor, digit as u8);
                        cursor -= 1;
                        if high | low == 0 {
                            break;
                        }
                    }
                    cursor += 1;
                }
            } else if shift != 0 {
                let mut value = low;
                loop {
                    cursor -= 1;
                    let digit = lbu(m, digits.wrapping_add(value & mask));
                    value >>= shift;
                    m.sb(cursor, digit as u8);
                    if value == 0 {
                        break;
                    }
                }
            } else {
                let mut value = low;
                loop {
                    let quotient = match value.checked_div(base) {
                        Some(quotient) => quotient,
                        None => {
                            m.raise(crate::machine::Fault::Trap {
                                address: 0x8002_7e08,
                                code: 7,
                            });
                            u32::MAX
                        }
                    };
                    cursor -= 1;
                    let product = lo(quotient, base);
                    let digit = lbu(m, digits.wrapping_add(value.wrapping_sub(product)));
                    value = quotient;
                    m.sb(cursor, digit as u8);
                    if value == 0 {
                        break;
                    }
                }
            }
        }
        let length = m.lw(frame + 0x64);
        m.sw(length, NUMBER_END.wrapping_sub(cursor));
        cursor
    })
}

/// 80027E80 (cursor, text, flags, sign; width, precision and length as
/// the caller's stack words +0x10..+0x18) -> characters written: one
/// formatted field: padding spaces (unless left-justified or zero-padded,
/// flags 0x20/0x40), the sign or the 0/0x/0X/0b prefix (flags 0x380/0x400),
/// zero padding to the width (0x40) and to the precision, the `length`
/// characters of `text`, and trailing spaces when left-justified (0x20).
pub fn format_field_279e0(m: &mut Machine, cursor: u32, text: u32, flags: u32, sign: u32) -> u32 {
    with_frame(m, 0x38, |m, frame| {
        let length = m.lw(frame + 0x50);
        let width = m.lw(frame + 0x48);
        let precision = m.lw(frame + 0x4c);
        let mut prefix = ((flags >> 9) & 1)
            .wrapping_add(u32::from(sign != 0))
            .wrapping_add(length);
        if flags & 0x180 != 0 {
            prefix = prefix.wrapping_add(2);
        }
        let total = if lt(prefix, precision) {
            precision
        } else {
            prefix
        };
        let put = |m: &mut Machine, c: u32| {
            put_char_27784(m, c, cursor);
        };
        if flags & 0x60 == 0 {
            let mut column = total;
            while lt(column, width) {
                put(m, 0x20);
                column = column.wrapping_add(1);
            }
        }
        if sign != 0 {
            put(m, sign);
        } else if flags & 0x380 != 0 {
            put(m, 0x30);
            if flags & 0x80 != 0 {
                put(m, 0x78);
            } else if flags & 0x100 != 0 {
                put(m, 0x58);
            } else if flags & 0x400 != 0 {
                put(m, 0x62);
            }
        }
        if flags & 0x60 == 0x40 {
            let mut column = total;
            while lt(column, width) {
                put(m, 0x30);
                column = column.wrapping_add(1);
            }
        }
        let mut column = prefix;
        while lt(column, precision) {
            put(m, 0x30);
            column = column.wrapping_add(1);
        }
        let mut text = text;
        let mut left = length;
        while lt(0, left) {
            let c = lbu(m, text);
            text = text.wrapping_add(1);
            put(m, c);
            left = left.wrapping_sub(1);
        }
        if flags & 0x20 != 0 {
            let mut column = total;
            while lt(column, width) {
                put(m, 0x20);
                column = column.wrapping_add(1);
            }
        }
        if lt(total, width) { width } else { total }
    })
}

// ==== dispatch ========================================================================

/// The routines of this module by original address, with the original
/// argument registers; `v0` is the routine's result (0 for routines whose
/// callers ignore it).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let v0 = match target {
        // ---- movement states ----
        0x8004_ee68 => {
            formation_4d860(m, a0);
            0
        }
        0x8004_ef80 => {
            wingman_4d978(m, a0);
            0
        }
        0x8004_f0c8 => {
            attack_run_4dac0(m, a0);
            0
        }
        0x8004_f570 => evade_4df64(m, a0),
        0x8004_fba4 => {
            state_11_4e580(m, a0);
            0
        }
        0x8004_fc04 => {
            destroyed_4e5e0(m, a0);
            0
        }
        0x8004_fcf0 => {
            state_14_4e6d8(m, a0);
            0
        }
        0x8004_fd38 => {
            state_15_4e720(m, a0);
            0
        }
        0x8004_8a80 => latest_event_47d78(m),
        0x8004_fd9c => {
            launch_4e784(m, a0);
            0
        }
        0x8004_ff88 => {
            escorted_4e970(m, a0);
            0
        }
        0x8005_00ec => {
            state_18_4ead4(m, a0);
            0
        }
        0x8005_0134 => {
            hover_4eb1c(m, a0);
            0
        }
        0x8005_0184 => {
            timed_flight_4eb6c(m, a0);
            0
        }
        0x8005_02d4 => {
            escort_duty_4ecbc(m, a0);
            0
        }
        0x8005_0534 => {
            peel_off_4ef14(m, a0);
            0
        }
        // ---- steering helpers and action states ----
        0x8004_4034 => level_off_434ec(m, a0),
        0x8004_409c => relax_attitude_43554(m, a0),
        0x8004_4188 => aim_43640(m, a0, a1, a2, a3),
        0x8004_4360 => climb_to_43818(m, a0, a1),
        0x8004_4474 => turn_to_4392c(m, a0, a1),
        0x8004_960c => aim_47ed4(m, a0),
        0x8004_966c => {
            approach_47f34(m, a0);
            0
        }
        0x8004_9a1c => {
            attack_aim_482e4(m, a0);
            0
        }
        0x8004_9af8 => {
            fly_to_483c0(m, a0);
            0
        }
        0x8004_9c4c => {
            hover_turn_48514(m, a0);
            0
        }
        0x8004_986c => {
            dock_48134(m, a0);
            0
        }
        0x8004_9e28 => {
            level_486f0(m, a0);
            0
        }
        0x8004_9ec4 => {
            cruise_4878c(m, a0);
            0
        }
        0x8004_9f38 => {
            strafe_48800(m, a0);
            0
        }
        0x8004_a2d8 => {
            loop_48ba0(m, a0);
            0
        }
        0x8004_a394 => {
            half_loop_48c5c(m, a0);
            0
        }
        0x8004_a474 => {
            knife_edge_48d3c(m, a0);
            0
        }
        0x8004_a56c => {
            knife_slide_48e34(m, a0);
            0
        }
        0x8004_a678 => {
            attitude_48f40(m, a0);
            0
        }
        0x8004_a7e8 => {
            landing_490ac(m, a0);
            0
        }
        0x8004_aa3c => {
            catapult_49300(m, a0);
            0
        }
        0x8004_ac90 => {
            climb_out_49554(m, a0);
            0
        }
        0x8004_ad98 => {
            shadow_4965c(m, a0);
            0
        }
        0x8004_afac => {
            height_band_49870(m, a0);
            0
        }
        0x8004_b108 => {
            turn_back_499cc(m, a0);
            0
        }
        0x8004_b308 => {
            blow_up_49bc4(m, a0);
            0
        }
        0x8004_b3c8 => {
            burn_49c84(m, a0);
            0
        }
        0x8004_b4d8 => {
            spin_fall_49d94(m, a0);
            0
        }
        0x8004_b6e4 => {
            glide_fall_49f9c(m, a0);
            0
        }
        0x8004_a018 => {
            halt_488e0(m, a0);
            0
        }
        0x8004_a0c0 => {
            taxi_48988(m, a0);
            0
        }
        0x8004_b88c => {
            take_off_4a144(m, a0);
            0
        }
        // ---- behaviour selection ----
        0x8005_0720 => select_default_4f100(m, a0),
        0x8005_08d0 => select_escort_4f2b0(m, a0),
        0x8005_0ac0 => {
            follow_leader_4f4a0(m, a0);
            0
        }
        0x8005_0bb4 => select_formation_4f594(m, a0),
        0x8005_0fa8 => load_waypoint_4f988(m, a0, a1),
        0x8005_0de8 => follow_route_4f7c8(m, a0),
        0x8005_1010 => keyframes_4f9f0(m, a0),
        0x8005_1304 => special_state_4fce4(m, a0),
        0x8005_13d8 => check_finished_4fdb0(m, a0),
        // ---- spawning, kill log, formations, weapons, sound ----
        0x8004_6928 => next_spawn_45ce4(m, a0),
        0x8003_82d4 => {
            clear_slot_flags_37fc4(m, a0);
            0
        }
        0x8004_bd18 => formation_offset_4a5c8(m, a0, a1, a2),
        0x8004_bda8 => {
            disband_group_4a658(m, a0);
            0
        }
        0x8004_ba7c => {
            update_group_4a334(m, a0);
            0
        }
        0x8004_7a54 => fire_control_46e0c(m),
        0x8004_7b00 => gun_46ebc(m, a0),
        0x8004_7d68 => missiles_47124(m, a0),
        // ---- projectile behaviours ----
        0x8006_903c => {
            rocket_6786c(m, a0);
            0
        }
        0x8006_9158 => {
            drop_rocket_67988(m, a0);
            0
        }
        0x8006_9d18 => {
            bomb_68548(m, a0);
            0
        }
        0x8006_9f48 => {
            accelerate_68778(m, a0);
            0
        }
        0x8006_9d6c => {
            arm_6859c(m, a0);
            0
        }
        0x8003_19c8 => aim_31784(m, a0, a1, a2),
        0x8006_92c4 => {
            homing_67af4(m, a0);
            0
        }
        0x8006_94cc => {
            homing_67cfc(m, a0);
            0
        }
        0x8006_9a9c => {
            homing_682cc(m, a0);
            0
        }
        0x8006_96d4 => {
            sea_skimmer_67f04(m, a0);
            0
        }
        0x8006_9900 => {
            cluster_68130(m, a0);
            0
        }
        0x8006_8e28 => submunition_67658(m, args),
        0x8006_9c8c => {
            impact_684bc(m, a0);
            0
        }
        0x8006_9d88 => {
            splash_685b8(m, a0);
            0
        }
        0x8006_9e10 => {
            burst_on_target_68640(m, a0);
            0
        }
        0x8006_9ec0 => {
            splash_686f0(m, a0);
            0
        }
        0x8006_8454 => queue_damage_66c84(m, a0, a1, a2, a3),
        0x8006_a978 => {
            blast_691a8(m, a0, a1);
            0
        }
        0x8006_aafc => {
            direct_hit_6932c(m, a0);
            0
        }
        // ---- effect spawners ----
        0x8007_fe28 => {
            explosion_particles_7e7e0(m, a0, a1);
            0
        }
        0x8008_00e0 => {
            explosion_7ea98(m, a0, a1);
            0
        }
        0x8008_0140 => {
            debris_particles_7eaf8(m, a0, a1, a2);
            0
        }
        0x8008_04b8 => {
            debris_7ee70(m, a0, a1, a2);
            0
        }
        0x8007_eeac => vector_command_7d830(m, a0, a1, a2),
        0x8008_9e48 => direction_angles_886d8(m, a0),
        0x8008_09f0 => {
            impact_burst_7f3a8(m, a0, a1, a2, a3);
            0
        }
        0x8008_074c => {
            impact_sparks_7f104(m, a0, a1, a2);
            0
        }
        0x8008_0f10 => {
            splash_7f8c8(m, a0, a1);
            0
        }
        0x8008_0f70 => {
            explosion_particles_7f928(m, a0, a1, a2);
            0
        }
        0x8008_1298 => {
            explosion_7fc50(m, a0, a1, a2);
            0
        }
        0x8008_2d80 => {
            dust_81738(m, a0, a1);
            0
        }
        0x8008_2ff0 => claim_tracer_819a8(m),
        0x8008_3054 => tracer_81a0c(m, a0, a1, a2, a3),
        0x8008_30dc => {
            tracer_command_81a94(m, a0, a1, a2, a3);
            0
        }
        0x8008_31d0 => {
            gun_fire_81b88(m, a0, a1, a2, a3);
            0
        }
        0x8008_2c1c => {
            spark_815d4(m, a0, a1);
            0
        }
        0x8008_2d38 => {
            sparks_816f0(m, a0, a1);
            0
        }
        0x8008_4e6c => {
            wake_83814(m, a0, a1);
            0
        }
        0x8009_3b2c => fade_92350(m, a0, a1, a2, a3),
        // ---- trails ----
        0x8007_bc88 => push_missile_trail_7a60c(m, a0, a1),
        0x8007_be3c => rewrite_missile_trail_7a7c0(m, a0, a1),
        0x8007_cc6c => push_entity_trail_7b5f0(m, a0, a1),
        0x8007_cdf8 => rewrite_entity_trail_7b77c(m, a0, a1),
        // ---- phase 4 ----
        0x8008_136c => {
            spark_7fd24(m, a0, a1);
            0
        }
        0x8008_158c => {
            spark_7ff44(m, a0, a1);
            0
        }
        0x8008_1760 => {
            spark_80118(m, a0, a1);
            0
        }
        0x8008_1994 => {
            spark_8034c(m, a0, a1);
            0
        }
        0x8008_3554 => {
            trail_smoke_81f0c(m, a0, a1);
            0
        }
        0x8008_3764 => {
            trail_puffs_8211c(m, a0);
            0
        }
        0x8008_3844 => {
            trail_fire_821fc(m, a0, a1);
            0
        }
        0x8008_3a40 => {
            trail_fire_823f8(m, a0, a1);
            0
        }
        0x8008_0a94 => {
            water_ring_7f44c(m, a0, a1, a2);
            0
        }
        0x8008_0c08 => {
            water_splash_7f5c0(m, a0, a1, a2);
            0
        }
        0x8002_7c24 => put_char_27784(m, a0, a1),
        0x8002_7c3c => format_number_2779c(m, a0, a1, a2, a3),
        0x8002_7e80 => format_field_279e0(m, a0, a1, a2, a3),
        0x8008_7584 => {
            blast_85e14(m, a0, a1, a2);
            0
        }
        0x8008_7c7c => {
            fireball_8650c(m, a0, a1, a2);
            0
        }
        0x8008_7ee0 => {
            smoke_ring_86770(m, a0, a1, a2);
            0
        }
        0x8008_81bc => {
            blast_86a4c(m, a0, a1, a2);
            0
        }
        0x8008_8254 => {
            debris_burst_86ae4(m, a0, a1, a2);
            0
        }
        0x8008_85dc => {
            blast_86e6c(m, a0, a1, a2);
            0
        }
        0x8008_8968 => {
            blast_871f8(m, a0, a1, a2);
            0
        }
        0x8008_8b48 => {
            flames_873d8(m, a0, a1, a2);
            0
        }
        0x8008_8d60 => {
            debris_875f0(m, a0, a1, a2);
            0
        }
        0x8008_8ea8 => {
            blast_87738(m, a0, a1, a2);
            0
        }
        0x8008_8fbc => {
            blast_8784c(m, a0, a1, a2);
            0
        }
        0x8008_90d8 => {
            blast_87968(m, a0, a1, a2);
            0
        }
        0x8008_92fc => {
            blast_87b8c(m, a0, a1, a2);
            0
        }
        0x8008_9488 => {
            blast_87d18(m, a0, a1, a2);
            0
        }
        0x8008_978c => {
            blast_8801c(m, a0, a1, a2);
            0
        }
        0x8008_9880 => {
            blast_88110(m, a0, a1, a2);
            0
        }
        0x8008_9974 => {
            blast_88204(m, a0, a1, a2);
            0
        }
        0x8008_9a04 => {
            blast_88294(m, a0, a1, a2);
            0
        }
        0x8008_9a94 => {
            blast_88324(m, a0, a1, a2);
            0
        }
        0x8008_9b24 => {
            blast_883b4(m, a0, a1, a2);
            0
        }
        0x8008_812c => {
            flash_event_869bc(m, a0, a1);
            0
        }
        0x8008_8454 => {
            burning_event_86ce4(m, a0, a1);
            0
        }
        0x8008_8748 => {
            crash_event_86fd8(m, a0, a1);
            0
        }
        0x8008_8a84 => {
            collapse_event_87314(m, a0, a1);
            0
        }
        0x8008_91ec => {
            fire_event_87a7c(m, a0, a1);
            0
        }
        0x8008_9378 => {
            fire_event_87c08(m, a0, a1);
            0
        }
        0x8008_9c40 => {
            trail_event_884d0(m, a0, a1);
            0
        }
        0x8008_9cd8 => {
            trail_event_88568(m, a0, a1);
            0
        }
        // ---- phase 3 ----
        0x8002_f460 => script_sound_2f044(m, a0, a1, a2),
        0x8004_dc38 => {
            phase_entities_4c644(m);
            0
        }
        0x8004_e80c => phase_ground_4d204(m),
        0x8008_7168 => {
            shock_wave_859f8(m, a0, a1, a2, a3);
            0
        }
        0x8008_721c => {
            flash_sparks_85aac(m, a0, a1, a2);
            0
        }
        0x8008_73e0 => {
            debris_85c70(m, a0, a1, a2);
            0
        }
        0x8008_7520 => {
            blast_85db0(m, a0, a1, a2);
            0
        }
        0x8008_762c => {
            debris_85ebc(m, a0, a1, a2);
            0
        }
        0x8008_775c => {
            flash_85fec(m, a0, a1, a2);
            0
        }
        0x8008_77fc => {
            blast_8608c(m, a0, a1, a2);
            0
        }
        0x8008_78b4 => {
            flashes_86144(m, a0, a1, a2);
            0
        }
        0x8008_7ad0 => {
            debris_86360(m, a0, a1, a2);
            0
        }
        0x8008_7c18 => {
            blast_864a8(m, a0, a1, a2);
            0
        }
        _ => return None,
    };
    Some(v0)
}
