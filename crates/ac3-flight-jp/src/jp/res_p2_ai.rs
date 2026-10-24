//! Patches of `ai` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! Most differences: JP reads the mission from the mission index (context
//! half +0x38C, signed) where PAL read the mission number byte 0x800BD02C
//! (the compared values are the same); the context fields from +0x390 are 4
//! bytes lower (the difficulty +0x394, PAL +0x398); the JP entity kinds
//! (PAL 0x14 and 0x3E are JP 0x1A and 0x2C). Others: the dock eases its
//! attitude by 1/4 per frame (PAL 1/64), some routines leave other values
//! in `v0`, and the missile routine adds radio calls (JP-only 80048C0C).
//!
//! The private items after the imports are copies of the generated module's
//! (same semantics), the ones these routines use.
use crate::ai::{explosion_7fc50, load_waypoint_4f988};
use crate::{
    common, dispatch,
    machine::{Machine, SmallData},
    sdk, shared,
};

/// The mission index: context half +0x38C, sign-extended (PAL read the
/// mission number byte 0x800BD02C).
fn mission_index(m: &mut Machine) -> u32 {
    let context = m.gw(common::CONTEXT);
    lh(m, context + 0x38c)
}

/// 80048C0C (JP-only, a `resident-game` routine of another part): the radio
/// chatter `a0` (0..9; `a2` the pilot's voice byte), when gameplay +4 bit 4
/// and the radio flag 800C7194 allow it.
const RADIO_CHATTER: u32 = 0x8004_8c0c;

/// The voice byte of `slot`: byte +3 of the block at slot info +0x10.
fn voice(m: &mut Machine, slot: u32) -> u32 {
    let info = slot_info(m, slot);
    let block = m.lw(info + 0x10);
    lbu(m, block + 3)
}

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

fn set_byte_bits(m: &mut Machine, address: u32, bits: u32) {
    let value = lbu(m, address) | bits;
    m.sb(address, value as u8);
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

/// 8004291C (`shared`): queue radio message `message`.
fn queue_message(m: &mut Machine, message: u32) {
    original(m, 0x8004_291c, [message, 0, 0, 0]);
}

/// `rand() % 3` as compiled (magic 0x55555556).
fn rand_mod3(m: &mut Machine) -> u32 {
    let value = sdk::rand(m);
    let third = hi(value, 0x5555_5556).wrapping_sub(sra(value, 31));
    value.wrapping_sub(third.wrapping_mul(3))
}

/// Action-state preamble: slot flag 0x20 (manoeuvring).
fn manoeuvring(m: &mut Machine, slot: u32) {
    set_word_bits(m, SLOT_FLAGS.wrapping_add(slot << 2), 0x20);
}

/// Clear an entity's velocity (+0x38..+0x40) and its speed components
/// (+0x20..+0x28), in the original store order.
fn stop_moving(m: &mut Machine, mover: u32) {
    for offset in [0x38, 0x3c, 0x40, 0x20, 0x24, 0x28] {
        m.sw(mover + offset, 0);
    }
}

/// The end of a falling wreck: explosion, sound 0x1B, phase 2.
fn wreck_explodes(m: &mut Machine, mover: u32, record: u32) {
    explosion_7fc50(m, mover + 8, mover + 0x38, 0);
    sound_at(m, 0x1b, mover + 8, 0);
    m.sb(record + 0x64, 2);
}

/// 800C7110: waypoint route pointers by route index (spawn record +2).
const ROUTES: u32 = 0x800c_7110;

/// 800C6E90: keyframe track pointers by route index.
const KEYFRAMES: u32 = 0x800c_6e90;

/// Wrap a 14-bit angle difference into `[-0x2000, 0x2000)` (one step).
fn wrap_angle(angle: u32) -> u32 {
    let angle = if lt(angle, 0x2000) {
        angle
    } else {
        angle.wrapping_sub(0x4000)
    };
    if lt(angle, 0xffff_e000) {
        angle.wrapping_add(0x4000)
    } else {
        angle
    }
}

/// `value * fraction / 8`, rounded toward zero.
fn eighths(value: u32, fraction: u32) -> u32 {
    let product = lo(value, fraction);
    sra(
        if lt(product, 0) {
            product.wrapping_add(7)
        } else {
            product
        },
        3,
    )
}

/// 800C6ECC: the missile alert word (bit 0).
const MISSILE_ALERT: u32 = 0x800c_6ecc;

/// 800B1FC4: the mission phase table (4 bytes per phase: kind, unused,
/// two entity slots), indexed by `[gp+0x334]`.
const PHASES: u32 = 0x800b_1fc4;

/// `[gp+0x334]`: the current mission phase.
const GP_PHASE: u32 = 0x334;

/// `[gp+0x38C]`: the phase spawn points (44 bytes per slot).
const GP_SPAWN_POINTS: u32 = 0x38c;

/// The break-off choice of 8004F0C8 phase 2: the manoeuvre action (with
/// its target attitude at +0x78..+0x7C), or `None` when the type keeps
/// flying (kind 3) or the pattern selects nothing.
fn break_off(m: &mut Machine, record: u32, mover: u32, params: u32, pilot: u32) -> Option<u32> {
    let kind = m.lw(params);
    if kind == 3 {
        return None;
    }
    let gameplay = m.gw(common::GAMEPLAY);
    if kind == 1 {
        let mut roll = lh(m, params + 0x14);
        if roll == 0 {
            roll = 0x1000;
        }
        if m.lw(gameplay + 0xc) & 1 == 0 {
            roll = roll.wrapping_neg();
        }
        m.sh(record + 0x78, 0x800);
        m.sh(record + 0x7a, 0);
        m.sh(record + 0x7c, roll as u16);
        return Some(0xc);
    }
    if matches!(mission_index(m), 0x20 | 0x2f | 0x35) {
        m.sh(record + 0x78, 0x2000);
        m.sh(record + 0x7a, 0);
        let base: u32 = if m.lw(gameplay + 0xc) & 1 != 0 {
            0xffff_f000
        } else {
            0x1000
        };
        let yaw = lhu(m, mover + 0x1c);
        m.sh(record + 0x7c, base.wrapping_sub(yaw) as u16);
        return Some(0xc);
    }
    let style = lbu(m, pilot);
    if !lt(style, 2) && m.lw(gameplay + 0xc) & 7 == 0 {
        match m.lw(mover + 8) & 3 {
            0 => {
                m.sh(record + 0x78, 0x1000);
                m.sh(record + 0x7a, 0);
                m.sh(record + 0x7c, 0);
            }
            1 => {
                m.sh(record + 0x78, 0x2000);
                m.sh(record + 0x7a, 0);
                m.sh(record + 0x7c, 0);
            }
            2 => {
                m.sh(record + 0x78, 0x1000);
                m.sh(record + 0x7a, 0);
                m.sh(record + 0x7c, 0x1000);
            }
            _ => {
                let pitch = lhu(m, mover + 0xc);
                m.sh(record + 0x7a, 0);
                m.sh(record + 0x78, ((pitch & 0x1fff) + 0x2000) as u16);
                let roll = lhu(m, mover + 0x10) & 0x1fff;
                m.sh(record + 0x7c, roll as u16);
            }
        }
        return Some(0xf);
    }
    let style = lbu(m, pilot);
    if style != 0 && m.lw(gameplay + 0xc) & 7 == 1 {
        return match m.lw(mover + 8) & 3 {
            0 => Some(0xa),
            1 => Some(0xb),
            2 => Some(0xd),
            _ => Some(0xe),
        };
    }
    m.sh(record + 0x78, 0x2000);
    m.sh(record + 0x7a, 0);
    let random = sdk::rand(m);
    m.sh(
        record + 0x7c,
        ((random & 0x3fff).wrapping_sub(0x2000)) as u16,
    );
    Some(0xc)
}

/// The mission-dependent radio message of 800502D4.
fn mission_message(m: &mut Machine, normal: u32, mission_7: u32, mission_2c: u32) -> u32 {
    match mission_index(m) {
        7 => mission_7,
        0x2c => mission_2c,
        _ => normal,
    }
}

/// Ease the attitude angle at `offset` of `mover` toward `target`'s by a
/// quarter of the (wrapped, 14-bit) difference, rounded toward zero (PAL
/// 1/64).
fn ease_angle_quarter(m: &mut Machine, mover: u32, target: u32, offset: u32) {
    let mut difference = lh(m, target + offset).wrapping_sub(lh(m, mover + offset)) & 0x3fff;
    let current = lhu(m, mover + offset);
    if !lt(difference, 0x2001) {
        difference = difference.wrapping_sub(0x4000);
    }
    let step = sra(
        if lt(difference, 0) {
            difference.wrapping_add(3)
        } else {
            difference
        },
        2,
    );
    m.sh(mover + offset, current.wrapping_add(step) as u16);
}

/// The homing flags (fifth argument of 80068800) of 80047D68 by skill.
fn homing_flags(m: &mut Machine, slot: u32, shooter: u32) -> u32 {
    let context = m.gw(common::CONTEXT);
    let players = m.lw(context + 0x394);
    let skill = if players == 0 && m.lw(shooter) & 0x80 != 0 {
        0
    } else {
        let info = slot_info(m, slot);
        let pilot = m.lw(info + 0x18);
        lbu(m, pilot)
    };
    match skill {
        0 => 0x800_0002,
        1 => 0x102,
        2 => 0x1_0102,
        3 => {
            if m.lw(shooter) & 0x40 != 0 {
                0x2_0202
            } else {
                let context = m.gw(common::CONTEXT);
                if m.lw(context + 0x394) != 3 {
                    0x1_0202
                } else {
                    0x2_0202
                }
            }
        }
        _ => 0x1_0202,
    }
}

/// 8004F0C8 (JP; PAL 8004DAC0, near): movement state 5, attack run on the player: phase 0 sets the
/// run timer (+0x40), 1 flies at the player's predicted position (with a
/// circling offset from difficulty 2), 2 picks the break-off manoeuvre,
/// 3 ends on record flag 0x400000.
///
/// JP: the mission index and the difficulty at context +0x394 (PAL +0x398).
pub fn attack_run_4dac0(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        let player = entity(m, 0);
        let info = slot_info(m, slot);
        let pilot = m.lw(info + 0x18);
        let params = m.lw(info + 0x14);
        let context = m.gw(common::CONTEXT);
        match lbu(m, record + 0x3e) {
            0 => {
                m.sb(record + 0x3e, 1);
                let time = lhu(m, pilot + 0xe);
                m.sh(record + 0x40, time as u16);
                if !lt(m.lw(context + 0x394), 2) {
                    m.sh(record + 0x40, sra(sext16(time), 1) as u16);
                }
                let height = m.lw(player + 0xc);
                m.sw(record + 0x6c, height);
            }
            1 => {
                m.sb(record + 0x3f, 0);
                let gameplay = m.gw(common::GAMEPLAY);
                let phase = lbu(m, gameplay + 0xc);
                let x = m.lw(player + 8).wrapping_add(m.lw(player + 0x38) << 12);
                m.sw(record + 0x68, x);
                let z = m.lw(player + 0x10).wrapping_add(m.lw(player + 0x40) << 12);
                m.sw(record + 0x70, z);
                if !lt(m.lw(context + 0x394), 2) {
                    let angle = (phase << 6) & 0x3fff;
                    let sine = sine_at(m, angle);
                    let skill = lbu(m, pilot + 1);
                    let x = m.lw(record + 0x68).wrapping_add(lo(sine, skill) << 12);
                    m.sw(record + 0x68, x);
                    let cosine = sine_at(m, angle + 0x1000);
                    let skill = lbu(m, pilot + 1);
                    let z = m.lw(record + 0x70).wrapping_add(lo(cosine, skill) << 12);
                    m.sw(record + 0x70, z);
                }
                crate::entities::clamp_destination_4efbc(m, slot, record + 0x68);
                m.sb(record + 0x63, 4);
                let time = lhu(m, record + 0x40);
                m.sb(record + 0x61, 3);
                let time = time.wrapping_sub(1);
                m.sh(record + 0x40, time as u16);
                if time & 0xffff == 0 {
                    m.sb(record + 0x3e, 2);
                }
            }
            2 => {
                m.sb(record + 0x3e, 3);
                if let Some(action) = break_off(m, record, mover, params, pilot) {
                    m.sb(record + 0x63, action as u8);
                    m.sb(record + 0x61, 0);
                }
            }
            3 if m.lw(record) & 0x40_0000 != 0 => m.sb(record + 0x3d, 0xff),
            _ => {}
        }
    });
}

/// 8004A678 (JP; PAL 80048F40, near): action 12, manoeuvre to the attitude at +0x78..+0x7C: novice
/// pilots (and missions 0x20/0x2F/0x35) roll first then pitch; others
/// request roll and pitch together.
///
/// JP: the mission index.
pub fn attitude_48f40(m: &mut Machine, slot: u32) {
    let info = slot_info(m, slot);
    let pilot = m.lw(info + 0x18);
    let record = slot_record(m, slot);
    let sequential = lbu(m, pilot) < 2 || matches!(mission_index(m), 0x20 | 0x2f | 0x35);
    let phase = lbu(m, record + 0x64);
    if sequential {
        match phase {
            0 | 2 => {
                if phase == 0 {
                    let roll = lhu(m, record + 0x7c);
                    m.sh(record + 0x82, 0);
                    m.sh(record + 0x80, 0);
                    m.sb(record + 0x64, 2);
                    m.sh(record + 0x84, roll as u16);
                }
                if lh(m, record + 0x84) == 0 {
                    let pitch = lhu(m, record + 0x78);
                    m.sb(record + 0x64, 3);
                    m.sh(record + 0x80, pitch as u16);
                }
            }
            3 => {
                if lh(m, record + 0x80) == 0 {
                    m.sb(record + 0x64, 4);
                }
            }
            4 => m.sb(record + 0x63, 0xff),
            _ => m.sb(record + 0x64, 0),
        }
        return;
    }
    match phase {
        0 | 1 => {
            if phase == 0 {
                // lwl/lwr + swl/swr: +0x78..+0x7F to +0x80..+0x87.
                let attitude = m.read_bytes(record + 0x78, 8);
                m.write_bytes(record + 0x80, &attitude);
                m.sb(record + 0x64, 1);
            }
            if lh(m, record + 0x80) == 0 && lh(m, record + 0x84) == 0 {
                m.sb(record + 0x64, 4);
            }
        }
        4 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

/// 8004AA3C (JP; PAL 80049300, near): action 25, catapult launch: placed on the deck, swung round a
/// circle of radius 0x800 (1/8 turn per `length` frames; 0xA8C frames in
/// mission 0x1A, else 0x1C20) while rising to the deck height, then run
/// out along -x for 0x130 frames.
///
/// JP: the mission index.
pub fn catapult_49300(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    let mover = entity(m, slot);
    let (length, rise) = if mission_index(m) == 0x1a {
        (0xa8c, 0x5000)
    } else {
        (0x1c20, 0x4000)
    };
    m.sb(record + 0x61, 9);
    match lbu(m, record + 0x64) {
        0 => {
            m.sw(mover + 8, 0xfdbe_0000);
            m.sw(mover + 0x10, 0xf15c_c000);
            m.sh(mover + 0x18, 0);
            m.sh(mover + 0x1a, 0);
            m.sh(mover + 0x1c, 0x400);
            stop_moving(m, mover);
            m.sw(mover + 0xc, 0xff59_a000u32.wrapping_sub(lo(rise, length)));
            m.sb(record + 0x64, 1);
        }
        1 => {
            let angle = div(lh(m, record + 0x66) << 15, length) & 0x3fff;
            let sine = sine_at(m, angle);
            let cosine = sine_at(m, angle + 0x1000);
            m.sw(mover + 8, 0xfdbe_0000u32.wrapping_sub(sine << 11));
            m.sw(mover + 0x10, 0xf35c_c000u32.wrapping_sub(cosine << 11));
            let frames = lh(m, record + 0x66);
            if frames == length {
                m.sw(mover + 0xc, 0xff59_a000);
                m.sb(record + 0x64, 2);
            } else {
                let height = 0xff59_a000u32.wrapping_sub(lo(rise, length.wrapping_sub(frames)));
                m.sw(mover + 0xc, height);
            }
            m.sh(mover + 0x1a, angle.wrapping_sub(0x1000) as u16);
            let left = length.wrapping_sub(lh(m, record + 0x66));
            if lt(left, lh(m, mover + 0x1c)) {
                m.sh(mover + 0x1c, left as u16);
            }
        }
        2 => {
            let frames = lh(m, record + 0x66);
            let height = m.lw(mover + 0xc);
            let left = 0x130u32.wrapping_sub(frames.wrapping_sub(length));
            let x = m.lw(mover + 8).wrapping_sub(left << 9);
            m.sw(mover + 8, x);
            if lt(0xffb9_a000, height) {
                m.sw(mover + 0xc, 0xffb9_a000);
            } else if lt(height, 0xffb9_a000) {
                m.sw(mover + 0xc, height.wrapping_add(0x8000));
            }
            m.sw(mover + 0x10, 0xf15c_c000);
            if left == 0 {
                m.sb(record + 0x64, 3);
            }
        }
        3 => {}
        _ => m.sb(record + 0x64, 0),
    }
}

/// 8004B108 (JP; PAL 800499CC, near): action 31, turn back toward the origin (offset by (-0x2000,
/// 0x2000) in missions 0x20/0x2F/0x35): roll 0x1000 toward the side of
/// the turn, then pitch through it (+0x78); speed mode 2 meanwhile.
///
/// JP: the mission index.
pub fn turn_back_499cc(m: &mut Machine, slot: u32) {
    with_frame(m, 0x30, |m, frame| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                let offset = matches!(mission_index(m), 0x20 | 0x2f | 0x35);
                let mut x = sra(m.lw(mover + 8), 12);
                if offset {
                    x = x.wrapping_add(0xe000);
                }
                m.sw(frame + 0x10, x);
                let mut z = sra(m.lw(mover + 0x10), 12);
                if offset {
                    z = z.wrapping_add(0x2000);
                }
                m.sw(frame + 0x18, z);
                let x = m.lw(frame + 0x10);
                let z = m.lw(frame + 0x18);
                let bearing = shared::atan2(m, x, z);
                let heading = lhu(m, mover + 0x1a);
                let turn = bearing.wrapping_sub(heading).wrapping_sub(0x2000) & 0x3fff;
                let turn = if lt(turn, 0x2000) {
                    turn
                } else {
                    turn | 0xc000
                };
                m.sh(record + 0x78, turn as u16);
                m.sh(record + 0x82, 0);
                m.sh(record + 0x80, 0);
                let signed = sext16(turn);
                let roll = if lt(0, signed) {
                    0x1000u32.wrapping_sub(lhu(m, mover + 0x1c))
                } else if lt(signed, 0) {
                    0xffff_f000u32.wrapping_sub(lhu(m, mover + 0x1c))
                } else {
                    0
                };
                m.sh(record + 0x84, roll as u16);
                let roll = lhu(m, record + 0x84) & 0x3fff;
                m.sh(record + 0x84, roll as u16);
                if !lt(roll, 0x2000) {
                    m.sh(record + 0x84, roll.wrapping_sub(0x4000) as u16);
                }
                m.sb(record + 0x64, 1);
                m.sb(record + 0x61, 2);
            }
            1 => {
                if lh(m, record + 0x84) == 0 {
                    let pitch = lhu(m, record + 0x78);
                    m.sb(record + 0x64, 2);
                    m.sh(record + 0x80, pitch as u16);
                }
                m.sb(record + 0x61, 2);
            }
            2 => {
                let pitch = lh(m, record + 0x80);
                m.sb(record + 0x61, 2);
                if pitch == 0 {
                    m.sb(record + 0x64, 3);
                    m.sb(record + 0x61, 0);
                }
            }
            3 => {
                m.sb(record + 0x61, 0);
                m.sb(record + 0x63, 0xff);
            }
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 8004B4D8 (JP; PAL 80049D94, near): action 42, spinning fall: requests and speed cleared, a
/// random spin about one axis chosen by `[gp+0x2E0]` +0xC; then fall
/// (+0x3C += 0x400) trailing smoke until below the ground height (0, or
/// 0xFF800000 outside missions 0x20/0x2F/0x35), explode and retire.
///
/// JP: the mission index.
pub fn spin_fall_49d94(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                for offset in [0x84, 0x82, 0x80, 8, 6, 4] {
                    m.sh(record + offset, 0);
                }
                let flags = m.lw(mover + 0x54);
                for offset in [0x28, 0x24, 0x20] {
                    m.sw(mover + offset, 0);
                }
                for offset in [0x30, 0x32, 0x34] {
                    m.sh(mover + offset, 0);
                }
                m.sw(mover + 0x54, flags | 0x80_0000);
                let random = sdk::rand(m);
                let gameplay = m.gw(common::GAMEPLAY);
                match m.lw(gameplay + 0xc) & 3 {
                    0 => m.sh(mover + 0x34, (random & 0x3ff).wrapping_sub(0x200) as u16),
                    1 => m.sh(mover + 0x32, (random & 0xff).wrapping_sub(0x80) as u16),
                    2 => m.sh(mover + 0x30, (random & 0xff).wrapping_sub(0x80) as u16),
                    _ => m.sh(mover + 0x30, 0xfff0),
                }
                m.sb(record + 0x64, 1);
            }
            1 => {
                let flags = m.lw(mover + 0x54) | 0x80_0000;
                let fall = m.lw(mover + 0x3c).wrapping_add(0x400);
                m.sw(mover + 0x54, flags);
                m.sw(mover + 0x3c, fall);
                let frames = lh(m, record + 0x66);
                smoke(m, mover + 8, mover + 0x38, frames, 0);
                let ground = if matches!(mission_index(m), 0x20 | 0x2f | 0x35) {
                    0
                } else {
                    0xff80_0000
                };
                if lt(ground, m.lw(mover + 0xc)) {
                    wreck_explodes(m, mover, record);
                }
            }
            2 => mark_destroyed(m, slot),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 80050184 (JP; PAL 8004EB6C, near): movement state 20, timed scripted flight: the hold time
/// (+0x40) depends on the mission (and slot in mission 0x29); then action
/// 0x34 toward the origin; record flag 0x400000 ends it and invalidates
/// the slot info +0x24.
///
/// JP: the mission index.
pub fn timed_flight_4eb6c(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    match lbu(m, record + 0x3e) {
        0 => {
            let mission = mission_index(m);
            m.sb(record + 0x63, 0x32);
            let time = match mission {
                7 => 0x96,
                0x1f => 0x168,
                0x29 => {
                    if slot == 2 {
                        0xf0
                    } else {
                        0x1a4
                    }
                }
                0x32 => 0xd2,
                _ => 0x28,
            };
            m.sh(record + 0x40, time);
            m.sb(record + 0x3e, 1);
        }
        1 => {
            if lh(m, record + 0x40) == 0 {
                m.sb(record + 0x3e, 2);
            }
        }
        2 => {
            m.sb(record + 0x63, 0x34);
            let mission = mission_index(m);
            m.sw(record + 0x6c, 0);
            m.sw(record + 0x68, 0);
            let z = if mission == 0x1f {
                0x20_0000
            } else {
                0x80_0000
            };
            m.sw(record + 0x70, z);
            m.sb(record + 0x3e, 3);
        }
        _ => {
            if m.lw(record) & 0x40_0000 != 0 {
                m.sb(record + 0x3d, 0xff);
                let info = slot_info(m, slot);
                m.sw(info + 0x24, u32::MAX);
            }
        }
    }
    m.sb(record + 0x61, 9);
    if lt(0, lh(m, record + 0x40)) {
        let time = lhu(m, record + 0x40).wrapping_sub(1);
        m.sh(record + 0x40, time as u16);
    }
}

/// 800502D4 (JP; PAL 8004ECBC, near): movement state 21, escort duty: count the escort time
/// (+0x40, 150 frames) and turn away after it; while gameplay +4 bit
/// 0x800, track whether the escorted object is safe (player +8/+0xA, radio
/// calls every 16 frames), and nag (gameplay bit 0x2000) when the player
/// strays.
///
/// JP: the mission index (in the radio message choice).
pub fn escort_duty_4ecbc(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        match lbu(m, record + 0x3e) {
            0 => {
                m.sb(record + 0x63, 0);
                let gameplay = m.gw(common::GAMEPLAY);
                if m.lw(gameplay + 4) & 0x800 != 0 {
                    let time = lhu(m, record + 0x40).wrapping_add(1);
                    m.sh(record + 0x40, time as u16);
                }
                if !lt(lh(m, record + 0x40), 0x97) {
                    m.sb(record + 0x3e, 1);
                }
            }
            1 => {
                m.sb(record + 0x63, 0xc);
                m.sh(record + 0x7c, 0x800);
                m.sh(record + 0x78, 0x400);
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
        let gameplay = m.gw(common::GAMEPLAY);
        if m.lw(gameplay + 4) & 0x800 != 0 {
            let object = m.lw(SLOT_OBJECTS.wrapping_add(slot << 2));
            let safe = lt(m.lw(object + 4), 0x1001) || m.lw(object) & 2 != 0;
            if safe {
                let player = m.gw(0x340);
                if lhu(m, player + 8) & 0xf == 0 {
                    let message = mission_message(m, 6, 7, 8);
                    queue_message(m, message);
                }
                let player = m.gw(0x340);
                m.sb(player + 0xa, 1);
                let player = m.gw(0x340);
                let count = lhu(m, player + 8).wrapping_add(1);
                m.sh(player + 8, count as u16);
            } else {
                let player = m.gw(0x340);
                m.sb(player + 0xa, 0);
                let player = m.gw(0x340);
                let count = lhu(m, player + 8).wrapping_sub(1);
                m.sh(player + 8, count as u16);
            }
        }
        let gameplay = m.gw(common::GAMEPLAY);
        if m.lw(gameplay + 4) & 0x2000 != 0 {
            let player = m.gw(0x340);
            if lbu(m, player + 0x2f) == 1 && m.lw(gameplay + 0xc) & 0xf == 0 {
                let message = mission_message(m, 0xe, 0xf, 0x10);
                queue_message(m, message);
            }
        }
        m.sb(record + 0x61, 8);
    });
}

/// 80051010 (JP; PAL 8004F9F0, near): keyframe flight (track of the spawn record +2, 20-byte keys
/// of position and angles, 8 steps per key): velocity toward the
/// interpolated position, attitude interpolated (a yaw flip near 180°
/// becomes a pitch through the vertical); movement state 13, action 8,
/// speed mode 9. Retires at the end key (-1; returns 0). Mission 3 sets
/// entity byte +5 bit 8.
///
/// JP: the mission index.
pub fn keyframes_4f9f0(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x10, |m, frame| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        if mission_index(m) == 3 {
            set_byte_bits(m, mover + 5, 8);
        }
        let step = m.lw(record + 0x38);
        let info = slot_info(m, slot);
        let spawn = m.lw(info + 0x30);
        let route = lbu(m, spawn + 2);
        let track = m.lw(KEYFRAMES + (route << 2));
        let key = sra(
            if lt(step, 0) {
                step.wrapping_add(7)
            } else {
                step
            },
            3,
        );
        let fraction = step.wrapping_sub(key << 3);
        let from = track.wrapping_add(key.wrapping_mul(20));
        let to = from.wrapping_add(0x14);
        if m.lw(to) == u32::MAX {
            m.sb(record + 0x1c, 0xff);
            return 0;
        }
        copy_block(m, frame, mover + 8, 4);
        for axis in [0u32, 4, 8] {
            let start = m.lw(from + axis);
            let delta = eighths(m.lw(to + axis).wrapping_sub(start), fraction);
            let position = m.lw(mover + 8 + axis);
            m.sw(
                mover + 0x38 + axis,
                delta.wrapping_add(start).wrapping_sub(position),
            );
        }
        let mut pitch = lh(m, to + 0xc).wrapping_sub(lh(m, from + 0xc));
        let mut yaw = lh(m, to + 0xe).wrapping_sub(lh(m, from + 0xe));
        let mut roll = lh(m, to + 0x10).wrapping_sub(lh(m, from + 0x10));
        if (yaw & 0x3fff).wrapping_sub(0x1c01) < 0x7ff {
            yaw = if lt(0, yaw) {
                yaw.wrapping_sub(0x2000)
            } else {
                yaw.wrapping_add(0x2000)
            };
            pitch = 0x2000u32
                .wrapping_sub(lh(m, from + 0xc))
                .wrapping_sub(lh(m, to + 0xc));
            roll = lh(m, to + 0x10)
                .wrapping_add(lh(m, from + 0x10))
                .wrapping_sub(0x2000);
        }
        let (pitch, yaw, roll) = (wrap_angle(pitch), wrap_angle(yaw), wrap_angle(roll));
        let value = lhu(m, from + 0xc).wrapping_add(eighths(pitch, fraction));
        m.sh(mover + 0x18, value as u16);
        let value = lhu(m, from + 0xe).wrapping_add(eighths(yaw, fraction));
        m.sh(mover + 0x1a, value as u16);
        let value = lhu(m, from + 0x10).wrapping_add(eighths(roll, fraction));
        m.sh(mover + 0x1c, value as u16);
        m.sb(record + 0x3d, 0xd);
        m.sb(record + 0x63, 8);
        let step = m.lw(record + 0x38);
        m.sb(record + 0x61, 9);
        m.sw(record + 0x38, step.wrapping_add(1));
        1
    })
}

/// 80051304 (JP; PAL 8004FCE4, near): the movement state of special types: kind 7 -> 14, kind 8 ->
/// 15, kind 6 -> 16 (mission 0x1C) or 17 (mission 0x24) on the leader,
/// others -> 18 on the leader in missions 6 and 0x37. Returns the
/// leftover `v0` (the state, or the last compared constant).
///
/// JP: the mission index.
pub fn special_state_4fce4(m: &mut Machine, slot: u32) -> u32 {
    let info = slot_info(m, slot);
    let params = m.lw(info + 0x14);
    let record = slot_record(m, slot);
    let state = match m.lw(params) {
        7 => {
            m.sb(record + 0x3d, 0xe);
            return 0xe;
        }
        8 => {
            m.sb(record + 0x3d, 0xf);
            return 0xf;
        }
        6 => match mission_index(m) {
            0x1c => 0x10,
            0x24 => 0x11,
            _ => return 0x11,
        },
        _ => match mission_index(m) {
            6 | 0x37 => 0x12,
            _ => return 0x37,
        },
    };
    let leader = lbu(m, record + 0x1f);
    m.sb(record + 0x3d, state as u8);
    m.sb(record + 0x3f, leader as u8);
    state
}

/// 8004DC38 (JP; PAL 8004C644, changed in the map, the same code): enter the current mission phase: for phase kinds 0-3 (and 7
/// outside mission 7) strip the live flags (0x400 from slots under a
/// shared lock 0x400000, else 0x4000) of slots 2..31, then bring in the
/// phase's two entity slots: live (0x4000), reset (8004E0FC, 8004E124),
/// AI record cleared, and placed by the kind: kinds 2/3 on the player
/// (raised 0x1000000, back 0x100000, +-0x400000 sideways at difficulty
/// 2+; attitude and velocity copied; state 0x13), kinds 5/6 at the spawn
/// point (state 0x14), kind 7 150 steps ahead of the player (state 0x15
/// for the first, 0x16 for the second).
///
/// JP: the mission index and the difficulty at context +0x394 (PAL +0x398).
pub fn phase_entities_4c644(m: &mut Machine) {
    with_frame(m, 0x30, |m, _| {
        let phase = (m.gw(GP_PHASE) << 2).wrapping_add(PHASES);
        let kind = lbu(m, phase);
        let strip = match kind {
            0..=3 => true,
            7 => mission_index(m) != 7,
            _ => false,
        };
        if strip {
            for slot in 2..32u32 {
                let table = m.gw(common::ENTITIES);
                let entity = m.lw(table + slot * 4);
                let flags = m.lw(entity);
                let value = if flags & 0x40 != 0 && m.lw(SLOT_FLAGS + slot * 4) & 0x40_0000 != 0 {
                    flags & !0x400
                } else {
                    m.lw(entity) & !0x4000
                };
                m.sw(entity, value);
            }
        }
        for index in 0..2u32 {
            let slot = lbu(m, phase + 2 + index);
            if slot == 0 {
                continue;
            }
            let mover = entity(m, slot);
            set_word_bits(m, mover, 0x4000);
            let record = slot_record(m, slot);
            original(m, 0x8004_e0fc, [slot, 0, 0, 0]);
            original(m, 0x8004_e124, [slot, 0, 0, 0]);
            m.sb(record + 0x1c, 8);
            m.sb(record + 0x1d, 0);
            m.sb(record + 0x1e, 0);
            m.sw(record + 0x38, 0);
            m.sb(record + 0x3e, 0);
            m.sb(record + 0x64, 0);
            clear_word_bits(m, SLOT_FLAGS + slot * 4, 0x10_0000);
            for offset in [4u32, 6, 8] {
                m.sh(record + offset, 0);
            }
            for offset in [0xcu32, 0x10, 0x14] {
                m.sw(record + offset, 0);
            }
            for offset in [0x80u32, 0x82, 0x84] {
                m.sh(record + offset, 0);
            }
            let state = match lbu(m, phase) {
                2 | 3 => {
                    let target = entity(m, slot);
                    let player = entity(m, 0);
                    let context = m.gw(common::CONTEXT);
                    copy_block(m, target + 8, player + 8, 4);
                    let y = m.lw(target + 0x10).wrapping_add(0x100_0000);
                    m.sw(target + 0x10, y);
                    let z = m.lw(target + 0xc).wrapping_add(0xfff0_0000);
                    m.sw(target + 0xc, z);
                    if !lt(m.lw(context + 0x394), 2) {
                        let side = if sdk::rand(m) & 1 != 0 {
                            0x40_0000
                        } else {
                            0xffc0_0000
                        };
                        let x = m.lw(target + 8).wrapping_add(side);
                        m.sw(target + 8, x);
                    }
                    let attitude = m.read_bytes(player + 0x18, 8);
                    m.write_bytes(target + 0x18, &attitude);
                    copy_block(m, target + 0x38, player + 0x38, 4);
                    let speed = m.lw(player + 0x40);
                    m.sw(target + 0x28, speed);
                    0x13
                }
                5 | 6 => {
                    let point = m.gw(GP_SPAWN_POINTS).wrapping_add(slot.wrapping_mul(44));
                    copy_block(m, mover + 8, point + 4, 4);
                    copy_block(m, mover + 0x18, point + 0x14, 2);
                    0x14
                }
                7 => {
                    let target = entity(m, slot);
                    let player = entity(m, 0);
                    copy_block(m, target + 8, player + 8, 4);
                    let step = lo(m.lw(player + 0x38), 150);
                    let x = m.lw(target + 8).wrapping_add(step);
                    m.sw(target + 8, x);
                    let step = lo(m.lw(player + 0x40), 150);
                    let z = m.lw(target + 0x10).wrapping_add(step);
                    m.sw(target + 0x10, z);
                    let attitude = m.read_bytes(player + 0x18, 8);
                    m.write_bytes(target + 0x18, &attitude);
                    if index != 0 { 0x16 } else { 0x15 }
                }
                _ => continue,
            };
            m.sb(record + 0x3d, state);
        }
    });
}

/// 80048A80 (JP; PAL 80047D78, near): the event value (+6) of the most recent of the context's
/// pending entries (+0x748, 8 bytes, count +0x3C2) whose +1 flag is clear,
/// or -1.
///
/// JP: the entries at context +0x748 and the count +0x3C2 (PAL +0x74C, +0x3C6).
pub fn latest_event_47d78(m: &mut Machine) -> u32 {
    let context = m.gw(common::CONTEXT);
    let mut index = lbu(m, context + 0x3c2).wrapping_sub(1);
    let mut offset = (index << 3).wrapping_add(0x748);
    while !lt(index, 0) {
        let entry = context.wrapping_add(offset);
        if lbu(m, entry + 1) == 0 {
            return lbu(m, entry + 6);
        }
        index = index.wrapping_sub(1);
        offset = offset.wrapping_sub(8);
    }
    u32::MAX
}

/// 8004B308 (JP; PAL 80049BC4, near): action 40, explode (large for object type 0x3E), then retire.
///
/// JP: the large explosion for entity kind 0x2C (PAL 0x3E).
pub fn blow_up_49bc4(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        match lbu(m, record + 0x64) {
            0 => {
                m.sb(record + 0x64, 1);
                let large = u32::from(lh(m, mover + 0x48) == 0x2c);
                explosion_7fc50(m, mover + 8, mover + 0x38, large);
            }
            1 => m.sb(record + 0x64, 2),
            2 => mark_destroyed(m, slot),
            _ => m.sb(record + 0x64, 0),
        }
    });
}

/// 80050DE8 (JP; PAL 8004F7C8, near): scripted route (route of the spawn record +2, 0x20 bytes per
/// waypoint): load the first waypoint; for type 0x3E abort to action 50
/// once the player is down; advance (record flag 0x400000) when the
/// waypoint's condition holds (kind 1: distance +0x88 below its value,
/// kind 2: frames +0x66 reached it); retire at action 0xFF (returns 0).
/// Movement state 13; returns 1.
///
/// JP: the entity kind 0x2C (PAL 0x3E).
pub fn follow_route_4f7c8(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        let info = slot_info(m, slot);
        let spawn = m.lw(info + 0x30);
        let route = lbu(m, spawn + 2);
        let points = m.lw(ROUTES + (route << 2));
        if lbu(m, record + 0x1d) == 0 {
            m.sw(record + 0x38, 0);
            let index = m.lw(record + 0x38);
            m.sb(record + 0x1d, 1);
            load_waypoint_4f988(m, record, points.wrapping_add(index << 5));
            return 1;
        }
        let mover = entity(m, slot);
        let player = entity(m, 0);
        if lh(m, mover + 0x48) == 0x2c && lt(lh(m, player + 0x4e), 0) {
            m.sb(record + 0x63, 0x32);
            m.sb(record + 0x61, 9);
            return 0;
        }
        let index = m.lw(record + 0x38);
        let word = m.lw(record);
        let point = points.wrapping_add(index << 5);
        if word & 0x40_0000 == 0 {
            match lbu(m, point + 3) {
                1 if lt(m.lw(record + 0x88), m.lw(point + 4)) => m.sw(record, word | 0x40_0000),
                2 if !lt(lh(m, record + 0x66), m.lw(point + 4)) => m.sw(record, word | 0x40_0000),
                _ => {}
            }
            if m.lw(record) & 0x40_0000 == 0 {
                m.sb(record + 0x3d, 0xd);
                return 1;
            }
        }
        let index = m.lw(record + 0x38).wrapping_add(1);
        m.sw(record + 0x38, index);
        load_waypoint_4f988(m, record, points.wrapping_add(index << 5));
        if lbu(m, record + 0x63) == 0xff {
            m.sb(record + 0x1c, 0xff);
            return 0;
        }
        clear_word_bits(m, record, 0x40_0000);
        m.sb(record + 0x3d, 0xd);
        1
    })
}

/// 800513D8 (JP; PAL 8004FDB0, near): retire (+0x1C = 0xFF, returns 0) once the movement request
/// is 0xFF; else returns 0xFF.
///
/// JP: it returns 0xFF (PAL 1) while the request is pending.
pub fn check_finished_4fdb0(m: &mut Machine, slot: u32) -> u32 {
    let record = slot_record(m, slot);
    if lbu(m, record + 0x3d) == 0xff {
        m.sb(record + 0x1c, 0xff);
        0
    } else {
        0xff
    }
}

/// 8004986C (JP; PAL 80048134, near): action 7, dock: ease the attitude toward the target (+0x3F)
/// by 1/4 and the position toward the destination by 1/256 per frame.
///
/// JP: the attitude eases by 1/4 (PAL 1/64).
pub fn dock_48134(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    let mover = entity(m, slot);
    let target_slot = lbu(m, record + 0x3f);
    let target = entity(m, target_slot);
    manoeuvring(m, slot);
    match lbu(m, record + 0x64) {
        0 => m.sb(record + 0x64, 1),
        1 => {
            for offset in [0x18, 0x1c, 0x1a] {
                ease_angle_quarter(m, mover, target, offset);
            }
            for (goal, axis) in [(0x68, 8), (0x6c, 0xc), (0x70, 0x10)] {
                let position = m.lw(mover + axis);
                let step = sra(m.lw(record + goal).wrapping_sub(position), 8);
                m.sw(mover + axis, position.wrapping_add(step));
            }
        }
        2 => m.sb(record + 0x63, 0xff),
        _ => m.sb(record + 0x64, 0),
    }
}

/// 8004FC04 (JP; PAL 8004E5E0, near): movement state 12, destroyed: pick the crash action (0x2B for
/// kinds 1/3, 0x2A for entity kind 0x1A, 0x28 for 0x2C, else
/// 0x28 + rand % 3); speed mode 9.
///
/// JP: the entity kinds 0x1A and 0x2C (PAL 0x14/0x15 and 0x3E).
pub fn destroyed_4e5e0(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        if lbu(m, record + 0x3e) == 0 {
            let info = slot_info(m, slot);
            let params = m.lw(info + 0x14);
            let kind = m.lw(params);
            let action = if kind == 1 || kind == 3 {
                0x2b
            } else {
                let kind = lh(m, mover + 0x48);
                if kind == 0x1a {
                    0x2a
                } else if kind == 0x2c {
                    0x28
                } else {
                    rand_mod3(m).wrapping_add(0x28)
                }
            };
            m.sb(record + 0x63, action as u8);
            m.sb(record + 0x3e, 1);
        }
        m.sb(record + 0x61, 9);
    });
}

/// 8004F570 (JP; PAL 8004DF64, near): movement state 7, missile evasion: on entry the evasion time
/// (+0x40) from the pilot block; while a missile tracks the slot, repeated
/// break turns (tables 800B2028/800B2038 by difficulty) with contrails;
/// state 0x10 waits out a decoyed missile, 7 dives to the player. Returns 0,
/// except (JP) 1 when no new turn starts at the end (phase not 1 or the time
/// not yet down to the pilot's +0x12) and 1 or 10 (state 10 or not) after a
/// difficulty-table turn.
///
/// JP: the mission index, the difficulty at context +0x394 (PAL +0x398) and the values left in `v0` (PAL always 0).
pub fn evade_4df64(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x30, |m, _| {
        let info = slot_info(m, slot);
        let shots = shots(m);
        let pilot = m.lw(info + 0x18);
        let params = m.lw(info + 0x14);
        let record = slot_record(m, slot);
        let threat_index = lbu(m, shots.wrapping_add(slot) + 8);
        let threat = shots
            .wrapping_add(threat_index.wrapping_mul(0xa4))
            .wrapping_add(0xc8);
        let mover = entity(m, slot);
        if lbu(m, record + 0x3e) == 0 {
            let kind = m.lw(params);
            if !matches!(kind, 0 | 5 | 2 | 1) {
                m.sb(record + 0x3d, 0xff);
            }
            m.sb(record + 0x3e, 1);
            let skill = lbu(m, pilot + 1).wrapping_sub(0x16);
            let time = lhu(m, pilot + 0x12).wrapping_add(lhu(m, pilot + 0x10).wrapping_sub(skill));
            m.sh(record + 0x40, time as u16);
            if m.lw(SLOT_FLAGS + (slot << 2)) & 0x80_0000 == 0 {
                let shots = self::shots(m);
                let index = lbu(m, shots.wrapping_add(slot) + 8);
                let tracker = shots.wrapping_add(index.wrapping_mul(0xa4));
                let flags = m.lw(tracker + 0x134);
                if flags & 0x3f != 0 && flags & 0x40 != 0 {
                    m.sb(record + 0x3e, 0x10);
                }
            }
            if lbu(m, record + 0x1c) == 0xa {
                let gameplay = m.gw(common::GAMEPLAY);
                if m.lw(gameplay + 0xc) & 3 != 0 {
                    m.sb(record + 0x3e, 7);
                }
            }
        }
        let time = lhu(m, record + 0x40).wrapping_sub(1);
        m.sh(record + 0x40, time as u16);
        if !lt(0, sext16(time)) {
            m.sb(record + 0x3d, 0xff);
        }
        if lbu(m, record + 0x3e) == 0x10 {
            set_byte_bits(m, mover + 5, 8);
            m.sb(record + 0x63, 0);
            m.sb(record + 0x61, 0);
            return 0;
        }
        if lt(lh(m, pilot + 0x12), lh(m, record + 0x40)) {
            return 0;
        }
        set_byte_bits(m, mover + 5, 8);
        let phase = lbu(m, record + 0x3e);
        if phase == 7 {
            let player = entity(m, 0);
            copy_block(m, record + 0x68, player + 8, 4);
            copy_block(m, record + 0x78, player + 0x18, 2);
            m.sb(record + 0x63, 4);
            m.sb(record + 0x61, 6);
            return 0;
        }
        if phase == 3 {
            let context = m.gw(common::CONTEXT);
            let difficulty = m.lw(context + 0x394);
            match difficulty {
                3 => m.sb(record + 0x3e, 1),
                2 => {
                    m.sb(record + 0x63, 0);
                    if lh(m, record + 0x66) == 0x3c {
                        m.sb(record + 0x3e, 1);
                    }
                }
                1 => {
                    m.sb(record + 0x63, 0);
                    if lh(m, record + 0x66) == 0x5a {
                        m.sb(record + 0x3e, 1);
                    }
                }
                0 => m.sb(record + 0x63, 0),
                _ => {}
            }
        }
        if lbu(m, record + 0x3e) == 2 && lbu(m, record + 0x63) == 0xff {
            let state = lbu(m, record + 0x1c);
            m.sb(record + 0x63, 0x32);
            m.sb(record + 0x61, u8::from(state == 0xa));
            m.sb(record + 0x3e, 3);
        }
        if lbu(m, record + 0x3e) != 1 || lt(lh(m, pilot + 0x12), lh(m, record + 0x40)) {
            return 1;
        }
        m.sb(record + 0x3e, 2);
        let context = m.gw(common::CONTEXT);
        let difficulty = m.lw(context + 0x394) & 3;
        let random = (sdk::rand(m) & 0x3fff).wrapping_sub(0x2000);
        let mut roll = random;
        let kind = m.lw(params);
        let gameplay = m.gw(common::GAMEPLAY);
        let in_state_10 = |m: &mut Machine| lbu(m, record + 0x1c) == 0xa;
        if kind == 1 {
            let mut turn = lh(m, params + 0x14);
            if turn == 0 {
                turn = 0x1000;
            }
            if m.lw(gameplay + 0xc) & 1 == 0 {
                turn = turn.wrapping_neg();
            }
            m.sh(record + 0x7c, turn as u16);
            let state_10 = in_state_10(m);
            m.sh(record + 0x78, 0x800);
            m.sb(record + 0x63, 0xc);
            m.sh(record + 0x7a, 0);
            m.sb(record + 0x61, u8::from(state_10));
            return 0;
        }
        if matches!(mission_index(m), 0x20 | 0x2f | 0x35) {
            let base: u32 = if m.lw(gameplay + 0xc) & 1 != 0 {
                0xffff_f000
            } else {
                0x1000
            };
            roll = base.wrapping_sub(lhu(m, mover + 0x1c));
        } else if difficulty == 3 {
            let distance = m.lw(threat + 0x68);
            roll = if lt(distance, 0x400) {
                sra(sext16(random), 3)
            } else if lt(distance, 0x800) {
                sra(sext16(random), 2)
            } else if lt(distance, 0x1000) {
                sra(sext16(random), 1)
            } else {
                random
            };
        } else if (difficulty == 0 && lbu(m, pilot) != 3) || (difficulty != 0 && lbu(m, pilot) == 0)
        {
            let bank = lh(m, mover + 0x1c);
            let bank_bits = lhu(m, mover + 0x1c);
            if !lt(abs(bank), 0x1001) {
                m.sh(record + 0x78, 0);
                m.sh(record + 0x7a, 0);
                let flip = lhu(m, mover + 0x1c);
                m.sb(record + 0x63, 0xc);
                let state_10 = in_state_10(m);
                m.sh(record + 0x7c, flip.wrapping_neg() as u16);
                m.sb(record + 0x61, u8::from(state_10));
                return 0;
            }
            let base: u32 = if lt(0, bank) { 0xffff_f000 } else { 0x1000 };
            roll = base.wrapping_sub(bank_bits);
        }
        let pitch_base = lhu(m, 0x800b_2028 + (difficulty << 2));
        let skill = lbu(m, pilot + 1);
        let shift = m.lw(0x800b_2038 + (difficulty << 2));
        m.sb(record + 0x63, 0xc);
        let pitch = pitch_base.wrapping_add(skill << (shift & 31));
        m.sh(record + 0x78, pitch as u16);
        let state_10 = in_state_10(m);
        m.sh(record + 0x7a, 0);
        m.sh(record + 0x7c, roll as u16);
        m.sb(record + 0x61, u8::from(state_10));
        if state_10 { 1 } else { 0xa }
    })
}

/// 80047D68 (JP; PAL 80047124, changed): fire missiles (info +0x1C kind, range and angle limits) at the
/// target +0x60 every pilot interval (+0x16; wingmen by player count),
/// unless it is already targeted (shots block +0/+4 masks); in mission 0x17
/// only far out and high. Raises the missile alert (800C6ECC bit 0) while
/// the player is about to be fired on.
///
/// JP additions (80048C0C, the radio chatter, in a 0x30-byte frame): after
/// firing, a voiced slot (slot flag 0x800000) calls 3 when its entity has
/// flag 0x40 and the target is a slot below 0x20, and 7 when it has flag
/// 0x80. With the player as the target, when the count reaches a quarter of
/// the interval it calls 6 (voiced, shooter flag 0x80) or 0 and returns that
/// call's `v0`; otherwise it returns the quarter (0 on the early exits).
pub fn missiles_47124(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x30, |m, frame| {
        let record = slot_record(m, slot);
        let table = m.gw(common::ENTITIES);
        let target_slot = lbu(m, record + 0x60);
        let info = slot_info(m, slot);
        let shooter = m.lw(table.wrapping_add(slot << 2));
        let target = m.lw(table.wrapping_add(target_slot << 2));
        let weapon = m.lw(info + 0x1c);
        let pilot = m.lw(info + 0x18);
        if weapon == 0 || lhu(m, weapon) == 0 {
            return 0;
        }
        if mission_index(m) == 0x17 {
            let gameplay = m.gw(common::GAMEPLAY);
            if m.lw(gameplay + 8) == 0
                && (lt(0xfc40_0000, m.lw(shooter + 0xc))
                    || !lt(0x946_ffff, abs(m.lw(shooter + 8)))
                    || !lt(0x946_ffff, abs(m.lw(shooter + 0x10))))
            {
                return 0;
            }
        }
        let target_slot = lbu(m, record + 0x60);
        let block = shots(m);
        let marked = if target_slot < 0x20 {
            m.lw(block) & bit(target_slot)
        } else {
            m.lw(block + 4) & bit(target_slot.wrapping_sub(0x20))
        };
        if marked != 0
            || !lt(lh(m, record + 0xb4), m.lw(weapon + 4))
            || m.lw(weapon + 8) < m.lw(record + 0xa0)
        {
            m.sh(record + 0x5e, 0);
            return 0;
        }
        let count = lhu(m, record + 0x5e).wrapping_add(1);
        m.sh(record + 0x5e, count as u16);
        let mut interval = lh(m, pilot + 0x16);
        if m.lw(shooter) & 0x80 != 0 {
            let context = m.gw(common::CONTEXT);
            let players = m.lw(context + 0x394);
            if players == 0 {
                interval <<= 1;
            }
            if !lt(players, 2) {
                interval = sra(interval, 1);
                if lt(interval, 0x3c) {
                    interval = 0x3c;
                }
            }
        }
        let fire = !lt(lh(m, record + 0x5e), interval) || {
            let current = lbu(m, record + 0x60);
            current != 0 && m.lw(SLOT_FLAGS.wrapping_add(current << 2)) & 0x400 == 0
        };
        if fire {
            let mut kind = lhu(m, weapon);
            if kind == 3 {
                let current = lbu(m, record + 0x60);
                if m.lw(SLOT_FLAGS.wrapping_add(current << 2)) & 0x400 != 0 {
                    kind = 1;
                }
            }
            let launch = |m: &mut Machine| {
                let word = m.lw(shooter);
                let aim = m.lw(target);
                [word, shooter + 8, shooter + 0x38, aim]
            };
            match kind.wrapping_sub(1) {
                0 | 3 => {
                    let flags = homing_flags(m, slot, shooter);
                    m.sw(frame + 0x10, flags);
                    let args = launch(m);
                    original(m, 0x8006_8800, args);
                }
                1 => {
                    let args = launch(m);
                    original(m, 0x8006_8c0c, args);
                }
                2 => {
                    let args = launch(m);
                    original(m, 0x8006_8c80, args);
                }
                7 => {
                    let args = launch(m);
                    original(m, 0x8006_8ba8, args);
                }
                8 => {
                    let args = launch(m);
                    original(m, 0x8006_89e8, args);
                }
                _ => {}
            }
            m.sh(record + 0x5e, 0);
            // JP: the radio calls of a voiced slot (flag 0x800000).
            if m.lw(SLOT_FLAGS.wrapping_add(slot << 2)) & 0x80_0000 != 0 {
                let own = entity(m, slot);
                if m.lw(own) & 0x40 != 0 && lbu(m, record + 0x60) < 0x20 {
                    let voice = voice(m, slot);
                    original(m, RADIO_CHATTER, [3, 0, voice, 0]);
                }
                let own = entity(m, slot);
                if m.lw(own) & 0x80 != 0 {
                    let voice = voice(m, slot);
                    original(m, RADIO_CHATTER, [7, 0, voice, 0]);
                }
            }
        }
        if lbu(m, record + 0x60) != 0 {
            return 0;
        }
        let count = lh(m, record + 0x5e);
        if lt(sra(interval, 1), count) && lt(count, lh(m, pilot + 0x16)) {
            set_word_bits(m, MISSILE_ALERT, 1);
        }
        // JP: the radio call at a quarter of the interval.
        let count = lh(m, record + 0x5e);
        let quarter = sra(interval, 2);
        if count != quarter {
            return quarter;
        }
        if m.lw(SLOT_FLAGS.wrapping_add(slot << 2)) & 0x80_0000 != 0 && m.lw(shooter) & 0x80 != 0 {
            let voice = voice(m, slot);
            original(m, RADIO_CHATTER, [6, 0, voice, 0])
        } else {
            original(m, RADIO_CHATTER, [0; 4])
        }
    })
}
