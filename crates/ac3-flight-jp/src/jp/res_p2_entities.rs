//! Patches of `entities` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! JP reads the mission from the mission index (context half +0x38C, a
//! signed halfword) where PAL read the mission number byte 0x800BD02C; the
//! compared values are the same. The entity kind numbers differ (PAL 0x13,
//! 0x14, 0x15, 0x3C, 0x3E, 0x3F are JP 0x19, 0x1A, 0x1B, 0x27, 0x2C, 0x2D),
//! the context fields from +0x390 are 4 bytes lower, and JP calls its own
//! routines where PAL called PAL-only ones (the message 8004CEB0 for PAL
//! 8004B760, REVIEW.md §1; the slot pass 80049438). The tracked sounds lost
//! PAL's context +0xF7C volume modes and scale by 30/100 (PAL 60/100).
//!
//! Two exact routines, the projectile and particle passes (8006A11C,
//! 8006B504), are patched only for their detailed hits: JP's model ray
//! 8009A944 reads the s4 they leave, through 8009B4EC, so they call
//! jp-res1's `object_ray_jp` with it.
//!
//! The helpers below are copies of the generated module's private ones.
use crate::{
    common, dispatch,
    entities::{
        age_timers_88768, ceiling_check_46828, choose_lock_3a820, floor_check_46968,
        goal_flags_46bd4, move_sprites_836ac, nearest_targets_47598, proximity_effect_81838,
        reset_idle_45c10, run_movement_461b8, run_timers_88600, steer_43f24, switch_lock_3a450,
        throttle_440e0, toggle_bits_8fde4, update_shake_8e8d8, write_expiry_event_7d284,
    },
    machine::{Machine, SmallData},
    script, sdk, shared,
};
use ac3_gte::reg;

/// 800C6FD0: per-slot status words (32 slots).
const SLOT_FLAGS: u32 = 0x800c_6fd0;
/// 800C6ED0: per-slot object (type description) pointers.
const SLOT_OBJECTS: u32 = 0x800c_6ed0;
/// 800C6D80: per-slot lock links, 4 bytes: +1 the slot's target, +2/+3 the
/// slots tracking it (0xFF = none).
const LINKS: u32 = 0x800c_6d80;
/// 800C6D60: lock-on block: +0 timer, +4 level, +0xE player target.
const TARGETING: u32 = 0x800c_6d60;
/// 800C6EA0: HUD status block (+0x20, +0x24, +0x2C flags).
const STATUS: u32 = 0x800c_6ea0;
/// 800C6610: tracked-sound channel count; voice bytes at +4/+8/+0xC + index.
const TRACKS: u32 = 0x800c_6610;
/// 800C6630: tracked-sound channel records (0x5C bytes).
const TRACK_RECORDS: u32 = 0x800c_6630;
/// 800C5830: sound voice records (0x14 bytes).
const VOICES: u32 = 0x800c_5830;
/// Scratchpad base.
const SCRATCH: u32 = 0x1f80_0000;
/// `[gp+0x34C]`: per-slot AI records (0xB8 bytes, indexed by slot).
const GP_SLOT_RECORDS: u32 = 0x34c;
/// `[gp+0x50C]`: the shots block.
const GP_SHOTS: u32 = 0x50c;
/// `[gp+0x53C]`: trail queues.
const GP_TRAILS: u32 = 0x53c;
/// 8004CEB0 (JP-only): show text message `a0` (PAL 8004B760).
const SHOW_MESSAGE: u32 = 0x8004_ceb0;
/// 80049438 (JP-only): a per-frame slot pass after 8004833C.
const SLOT_PASS: u32 = 0x8004_9438;

/// The mission index: context half +0x38C, sign-extended (PAL read the
/// mission number byte 0x800BD02C).
fn mission_index(m: &mut Machine) -> u32 {
    let context = m.gw(common::CONTEXT);
    lh(m, context + 0x38c)
}

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// `slt`: signed less-than.
fn lt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}

/// `mult` + `mfhi`.
fn hi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}

fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}

fn lhu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lhu(address))
}

fn lb(m: &mut Machine, address: u32) -> u32 {
    m.lb(address) as i32 as u32
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

/// Both 0x4000 (present) and 0x8000 set in the entity word.
fn alive(m: &mut Machine, entity: u32) -> bool {
    m.lw(entity) & 0xc000 == 0xc000
}

/// The per-slot AI record of `slot` (`[gp+0x34C] + slot * 0xB8`).
fn slot_record(m: &mut Machine, slot: u32) -> u32 {
    m.gw(GP_SLOT_RECORDS).wrapping_add(slot.wrapping_mul(0xb8))
}

/// A routine with no native owner in this module, by original address.
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

/// `RTV0` (MVMVA sf=1, rotation, V0, no translation).
fn rtv0(m: &mut Machine) {
    m.gte.mvmva(true, false, 0, 0, 3);
}

/// `ldv0`: VXY0/VZ0 from two words.
fn load_v0(m: &mut Machine, address: u32) {
    let xy = m.lw(address);
    let z = m.lw(address + 4);
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
}

/// `read_mt`: MAC1..MAC3.
fn read_mac(m: &mut Machine) -> [u32; 3] {
    let a = m.gte.mfc2(reg::MAC1);
    let b = m.gte.mfc2(reg::MAC2);
    let c = m.gte.mfc2(reg::MAC3);
    [a, b, c]
}

/// `stlvnl`: MAC1..MAC3 to three words.
fn store_mac(m: &mut Machine, address: u32) {
    let [a, b, c] = read_mac(m);
    m.sw(address, a);
    m.sw(address + 4, b);
    m.sw(address + 8, c);
}

/// `x / 100` as compiled (magic 0x51EB851F, shift 5).
fn div100(value: u32) -> u32 {
    sra(hi(value, 0x51eb_851f), 5).wrapping_sub(sra(value, 31))
}

/// Reset an entity's angular rates (+0x30/+0x32/+0x34).
fn stop_turning(m: &mut Machine, entity: u32) {
    m.sh(entity + 0x30, 0);
    m.sh(entity + 0x32, 0);
    m.sh(entity + 0x34, 0);
}

/// The trail handle byte of `slot` at `[gp+0x53C] + offset + slot`.
fn trail_handle(m: &mut Machine, offset: u32, slot: u32) -> u32 {
    m.gw(GP_TRAILS).wrapping_add(offset + slot)
}

/// 80038100 (JP; PAL 80037DFC, near, `imm:3 ins:3`): clear entity bit
/// 0x8000 for slots 2..31 and set it again for present entities that are (or
/// are tracked by) something noteworthy; also refresh slot flag 0x40000
/// (hit points of at least 10000, flagged 0x80, and in JP slot flag 0x800000).
pub fn mark_entities_37dfc(m: &mut Machine) {
    let records = m.gw(GP_SLOT_RECORDS).wrapping_add(0x170);
    let table = m.gw(common::ENTITIES);
    for slot in 2..32u32 {
        let entry = table.wrapping_add(slot << 2);
        let entity = m.lw(entry);
        let word = m.lw(entity) & 0xffff_7fff;
        m.sw(entity, word);
        if word & 0x4000 == 0 {
            continue;
        }
        let flags_at = SLOT_FLAGS + (slot << 2);
        let flags = m.lw(flags_at) & 0xfffb_ffff;
        m.sw(flags_at, flags);
        if !lt(lh(m, entity + 0x4e), 0x2710) && flags & 0x80_0000 != 0 && m.lw(entity) & 0x80 != 0 {
            m.sw(flags_at, flags | 0x4_0000);
        }
        let flags = m.lw(flags_at);
        if flags & 0x40_0000 != 0 {
            continue;
        }
        let record = records.wrapping_add((slot - 2) * 0xb8);
        let mark = if flags & 1 != 0 {
            let object_at = SLOT_OBJECTS + (slot << 2);
            let mark_self = flags & 0x10 != 0
                || {
                    let object = m.lw(object_at);
                    m.lw(object) & 0x80 != 0
                }
                || {
                    let again = m.lw(entry);
                    !lt(0, lh(m, again + 0x4e))
                }
                || lbu(m, record + 0x60) == 0;
            if mark_self {
                set_word_bits(m, entity, 0x8000);
            }
            let links = LINKS + (slot << 2);
            let tracker = lbu(m, links + 2);
            if tracker != 0xff {
                let other = m.lw(table.wrapping_add(tracker << 2));
                if m.lw(other) & 0x40 != 0 {
                    set_word_bits(m, entity, 0x8000);
                }
            }
            let tracker = lbu(m, links + 3);
            if tracker == 0xff {
                continue;
            }
            let other = m.lw(table.wrapping_add(tracker << 2));
            m.lw(other) & 0x40 != 0
        } else {
            flags & 2 != 0
        };
        if mark {
            set_word_bits(m, entity, 0x8000);
        }
    }
}

/// 8003809C (JP; PAL 80037DA0, near, `call+:1 ins:2`): clear status bit 0
/// and run the slot AI passes, then JP 80049438.
pub fn run_entity_logic_37da0(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        clear_word_bits(m, STATUS + 0x2c, 1);
        retire_entities_459c0(m);
        reset_idle_45c10(m);
        select_behaviour_45da8(m);
        run_movement_461b8(m);
        run_actions_463d8(m);
        nearest_targets_47598(m);
        dispatch::call(m, SLOT_PASS, [0; 4]);
    });
}

/// 80046600 (JP; PAL 800459C0, near, `del:3 imm:8 ins:4`, one call changed):
/// handle slots whose entity ran out of hit points (+0x4E <= 0): kill counters,
/// the respawn script (flag 0x80000; kind 0x1B, PAL 0x15, in missions 0x21 and
/// 0x3A by the mission index), destruction effects (flag 0x800; not kind 0x2C,
/// PAL 0x3E; context byte +0x39D, PAL +0x3A1; message 6/7 through JP 8004CEB0,
/// PAL 8004B760), the per-slot death script.
pub fn retire_entities_459c0(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        for slot in 2..32u32 {
            let scripts = slot << 6;
            let flags_at = SLOT_FLAGS + (slot << 2);
            let entity = entity(m, slot);
            let flags = m.lw(flags_at);
            if flags & 1 == 0 || lt(0, lh(m, entity + 0x4e)) {
                continue;
            }
            if flags & 0x100 != 0 {
                let player = m.gw(0x340);
                let count = lbu(m, player + 0x20).wrapping_add(1);
                m.sb(player + 0x20, count as u8);
            }
            if m.lw(flags_at) & 0x200 != 0 {
                let player = m.gw(0x340);
                let count = lbu(m, player + 0x21).wrapping_add(1);
                m.sb(player + 0x21, count as u8);
            }
            let flags = m.lw(flags_at);
            if flags & 0x8_0000 != 0 {
                let mission = m.gw(0x338);
                let respawn = m.lw(mission + 0x154);
                script::interpret(m, respawn);
                let table = m.gw(0x384);
                let info = m.lw(table.wrapping_add(scripts) + 0x10);
                let points = m.lhu(info);
                m.sh(entity + 0x4e, points);
                let flags = m.lw(flags_at) & 0xfff7_ffff;
                m.sw(flags_at, flags);
                if lh(m, entity + 0x48) != 0x1b {
                    continue;
                }
                let mission = mission_index(m);
                if mission != 0x21 && mission != 0x3a {
                    continue;
                }
                m.sw(flags_at, flags | 0x800);
                original(m, 0x8004_8758, [slot, 0, 0, 0]);
                continue;
            }
            if flags & 0x800 != 0
                && m.lw(entity + 0x50) & 0x3f == 0
                && m.lw(entity) & 0x40 == 0
                && lh(m, entity + 0x48) != 0x2c
            {
                let context = m.gw(common::CONTEXT);
                if lbu(m, context + 0x39d) & 1 != 0 {
                    original(m, 0x8007_97fc, [0; 4]);
                }
                if m.lw(entity) & 0x80 != 0 {
                    let gameplay = m.gw(common::GAMEPLAY);
                    let sound = if m.lw(gameplay + 0xc) & 7 != 0 { 7 } else { 6 };
                    original(m, SHOW_MESSAGE, [sound, 0, 0, 0]);
                }
            }
            original(m, 0x8004_8758, [slot, 0, 0, 0]);
            original(m, 0x8003_82d4, [slot, 0, 0, 0]);
            let table = m.gw(0x384);
            let death = m.lw(table.wrapping_add(scripts) + 0x34);
            if death != 0 {
                script::interpret(m, death);
            }
        }
    });
}

/// 800469EC (JP; PAL 80045DA8, near, `imm:1`): choose each present slot's
/// next movement state (+0x3D) and lock target (+0x60); the difficulty word is
/// context +0x394 (PAL +0x398).
pub fn select_behaviour_45da8(m: &mut Machine) {
    with_frame(m, 0x38, |m, _| {
        for slot in 2..32u32 {
            let entity = entity(m, slot);
            let record = slot_record(m, slot);
            if !alive(m, entity) {
                continue;
            }
            let flags_at = SLOT_FLAGS + (slot << 2);
            let flags = m.lw(flags_at);
            if flags & 2 != 0 {
                m.sw(flags_at, flags & 0xffef_ffff);
                let state = lbu(m, record + 0x1c);
                m.sb(record + 0x3d, 0xc);
                if state == 6 {
                    m.sb(record + 0x1c, 0);
                }
                continue;
            }
            match lbu(m, record + 0x1c) {
                5 => {
                    m.sw(flags_at, flags & 0xffef_ffff);
                    original(m, 0x8005_0de8, [slot, 0, 0, 0]);
                    continue;
                }
                6 => {
                    m.sw(flags_at, flags & 0xffef_ffff);
                    original(m, 0x8005_1010, [slot, 0, 0, 0]);
                    continue;
                }
                7 => {
                    original(m, 0x8005_1304, [slot, 0, 0, 0]);
                    continue;
                }
                8 => {
                    original(m, 0x8005_13d8, [slot, 0, 0, 0]);
                    continue;
                }
                _ => {}
            }
            if flags & 0x10_0000 != 0 {
                continue;
            }
            goal_flags_46bd4(m, slot);
            if lbu(m, record + 0x3c) == 0xb {
                continue;
            }
            if original(m, 0x8004_76f0, [slot, 0, 0, 0]) != 0 {
                m.sb(record + 0x3d, 0xb);
                let target = shared::release_lock(m, slot);
                let previous = lbu(m, record + 0x1f);
                m.sb(record + 0x60, target as u8);
                m.sb(record + 0x3f, previous as u8);
                let words: [u32; 4] = std::array::from_fn(|i| m.lw(record + 0x20 + i as u32 * 4));
                for (i, word) in words.into_iter().enumerate() {
                    m.sw(record + 0x44 + i as u32 * 4, word);
                }
                continue;
            }
            if lbu(m, record + 0x3c) == 0xa {
                continue;
            }
            if floor_check_46968(m, slot) != 0 || ceiling_check_46828(m, slot) != 0 {
                m.sb(record + 0x3d, 0xa);
                let target = shared::release_lock(m, slot);
                let previous = lbu(m, record + 0x1f);
                m.sb(record + 0x60, target as u8);
                m.sb(record + 0x3f, previous as u8);
                continue;
            }
            let current = lbu(m, record + 0x3c);
            if current == 7 {
                continue;
            }
            let shots = m.gw(GP_SHOTS);
            if m.lw(shots) & bit(slot) != 0 {
                m.sb(record + 0x3d, 7);
                let target = shared::release_lock(m, slot);
                m.sb(record + 0x60, target as u8);
                continue;
            }
            if current == 5 {
                continue;
            }
            if m.lw(entity) & 0x1c0 != 0x40 {
                let object = m.lw(SLOT_OBJECTS + (slot << 2));
                if m.lw(object) & 0x200_0000 != 0 {
                    let context = m.gw(common::CONTEXT);
                    if m.lw(context + 0x394) != 3 {
                        m.sb(record + 0x3d, 5);
                        let target = shared::release_lock(m, slot);
                        m.sb(record + 0x60, target as u8);
                        continue;
                    }
                }
            }
            let routine = match lbu(m, record + 0x1c).wrapping_sub(1) {
                1 | 3 => 0x8005_08d0,
                8 | 9 => 0x8005_0bb4,
                _ => 0x8005_0720,
            };
            original(m, routine, [slot, 0, 0, 0]);
            let target = lbu(m, record + 0x60);
            if target == 0xff || lbu(m, record + 0x1c).wrapping_sub(9) < 2 {
                continue;
            }
            let table = m.gw(0x384);
            let info = m.lw(table.wrapping_add(slot << 6) + 0x18);
            let range = lh(m, record + 0xb6);
            if lt(lh(m, info + 0x14), range) {
                let target = shared::release_lock(m, slot);
                m.sb(record + 0x60, target as u8);
                continue;
            }
            if target != 0 {
                let current = lbu(m, record + 0x60);
                if m.lw(SLOT_FLAGS + (current << 2)) & 1 == 0 {
                    m.sb(record + 0x60, 0xff);
                    continue;
                }
            }
            let own = lbu(m, LINKS + (slot << 2) + 1);
            if own == 0xff {
                m.sb(record + 0x60, 0xff);
                continue;
            }
            set_word_bits(m, flags_at, 0x1_0000);
        }
    });
}

/// 8004701C (JP; PAL 800463D8, near, `imm:6 ins:1 op:2`): switch each present
/// slot to its requested action (+0x63 into +0x62), run the action routine and
/// count its frames (+0x66); then the per-slot passes 80044A6C and 80044C28.
/// Action 4 checks the mission index (PAL the mission number).
pub fn run_actions_463d8(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        for slot in 2..32u32 {
            let entity = entity(m, slot);
            let record = slot_record(m, slot);
            if !alive(m, entity) || lbu(m, record + 0x1c) == 6 {
                continue;
            }
            if lbu(m, record + 0x62) != lbu(m, record + 0x63) {
                let word = m.lw(record);
                let next = lbu(m, record + 0x63);
                m.sb(record + 0x64, 0);
                m.sh(record + 0x66, 0);
                m.sw(record, word & 0xffbf_ffff);
                m.sb(record + 0x62, next as u8);
                stop_turning(m, entity);
            }
            let action = lbu(m, record + 0x62);
            if let Some(routine) = action_routine(m, slot, record, action) {
                original(m, routine, [slot, 0, 0, 0]);
            }
            let frames = lhu(m, record + 0x66).wrapping_add(1);
            let next = lbu(m, record + 0x63);
            m.sh(record + 0x66, frames as u16);
            if next == 0xff {
                let word = m.lw(record);
                let next = lbu(m, record + 0x63);
                m.sw(record, word | 0x40_0000);
                m.sb(record + 0x62, next as u8);
                stop_turning(m, entity);
            }
        }
        for pass in 0..2 {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity) {
                    let record = slot_record(m, slot);
                    if lbu(m, record + 0x1c) != 6 {
                        if pass == 0 {
                            steer_43f24(m, slot);
                        } else {
                            throttle_440e0(m, slot);
                        }
                    }
                }
            }
        }
    });
}

/// Action routines of 8004701C (jump table 80023EB8).
fn action_routine(m: &mut Machine, slot: u32, record: u32, action: u32) -> Option<u32> {
    let kind = |m: &mut Machine| {
        let info = m.gw(0x384).wrapping_add(slot << 6);
        let pointer = m.lw(info + 0x14);
        (info, m.lw(pointer))
    };
    Some(match action {
        0 => 0x8004_9e28,
        4 => {
            let (info, kind) = kind(m);
            if kind == 1 {
                0x8004_9af8
            } else {
                let special = lbu(m, record + 0x3c) == 3 || {
                    let pointer = m.lw(info + 0x18);
                    lbu(m, pointer) != 0
                };
                if special && !matches!(mission_index(m), 0x20 | 0x2f | 0x35) {
                    0x8004_960c
                } else {
                    0x8004_966c
                }
            }
        }
        5 => match kind(m).1 {
            1 => 0x8004_9af8,
            3 => 0x8004_9c4c,
            _ => 0x8004_9a1c,
        },
        6 => 0x8004_9af8,
        7 => 0x8004_986c,
        8 => 0x8004_9ec4,
        9 => 0x8004_9f38,
        10 => 0x8004_a2d8,
        11 => 0x8004_a394,
        12 => 0x8004_a678,
        13 => 0x8004_a474,
        14 => 0x8004_a56c,
        15..=17 => 0x8004_a7e8,
        25 => 0x8004_aa3c,
        26 => 0x8004_ac90,
        27 => 0x8004_ad98,
        30 => 0x8004_afac,
        31 => 0x8004_b108,
        40 => 0x8004_b308,
        41 => 0x8004_b3c8,
        42 => 0x8004_b4d8,
        43 => 0x8004_b6e4,
        50 => 0x8004_a018,
        51 => 0x8004_a0c0,
        52 => 0x8004_b88c,
        _ => return None,
    })
}

/// 8002F9AC (JP; PAL 8002F590, changed): per tracked-sound channel with three
/// voices: start (flag 1), stop (flag 2 or no entity), else position the sound
/// relative to `listener` and set its voice volumes ([`track_sound`]).
pub fn update_track_sounds_2f590(m: &mut Machine, listener: u32) {
    with_frame(m, 0x58, |m, frame| {
        // `sw a0, 0x58(sp)`: the argument's home slot in the caller's frame.
        m.sw(frame + 0x58, listener);
        let mut index = 0u32;
        while lt(index, m.lw(TRACKS)) {
            let record = TRACK_RECORDS.wrapping_add(index.wrapping_mul(0x5c));
            let kind = m.lw(record + 4);
            let params = kind.wrapping_mul(20).wrapping_add(0x800b_0900);
            let voice_bytes = TRACKS.wrapping_add(index);
            let voices = [
                lbu(m, voice_bytes + 4),
                lbu(m, voice_bytes + 8),
                lbu(m, voice_bytes + 0xc),
            ];
            if !voices.contains(&0xff) {
                track_sound(m, frame, record, params, voices, listener);
            }
            index += 1;
        }
    });
}

/// 8002F9AC, one channel (JP): as PAL, but with the wide-field flag (gameplay
/// +4 bit 0x20) the voice volumes are scaled by 30/100 (PAL 60/100) and PAL's
/// context +0xF7C modes (4: fade by 0x800BCF74 / 80; 10: silence) are gone.
fn track_sound(
    m: &mut Machine,
    frame: u32,
    record: u32,
    params: u32,
    voices: [u32; 3],
    listener: u32,
) {
    let [first, middle, last] = voices;
    let position = record + 0xc;
    let work = record + 0x24;
    let shape = record + 0x30;
    let near = record + 0x38;
    let far = record + 0x44;
    let silence = |m: &mut Machine| {
        for voice in voices {
            shared::f_8002daa0(m, [voice, 0, 0, 0]);
        }
    };
    let word = m.lw(record);
    if word & 1 != 0 {
        m.sw(record, word & !1);
        for (i, voice) in voices.into_iter().enumerate() {
            let program = lbu(m, params + i as u32);
            original(m, 0x8002_dc7c, [program, voice, 0, 0]);
        }
        for voice in voices {
            let at = VOICES.wrapping_add(voice * 20);
            let pitch = lh(m, at + 0xe);
            let pitch = original(m, 0x8002_e64c, [pitch, voice << 4, 0, 0]);
            m.sh(at + 0xe, pitch as u16);
        }
        let stack_argument = lbu(m, params + 5);
        m.sw(frame + 0x10, stack_argument);
        let args = [
            shape,
            lbu(m, params + 3),
            lbu(m, params + 4),
            lh(m, params + 6),
        ];
        original(m, 0x8002_ed08, args);
        original(m, 0x8002_ee7c, [near, first, 0, 0]);
        let args = [near, lbu(m, params + 8), lbu(m, params + 0xc), 0];
        original(m, 0x8002_ee88, args);
        let args = [near, lbu(m, params + 9), lbu(m, params + 0xe), 0];
        original(m, 0x8002_eea8, args);
        original(m, 0x8002_ee7c, [far, last, 0, 0]);
        let args = [far, lbu(m, params + 0xa), lbu(m, params + 0x10), 0];
        original(m, 0x8002_ee88, args);
        let args = [far, lbu(m, params + 0xb), lbu(m, params + 0x12), 0];
        original(m, 0x8002_eea8, args);
    } else if word & 2 != 0 {
        m.sw(record, word & !2);
        silence(m);
        return;
    }
    if m.lw(record + 8) == u32::MAX {
        silence(m);
        return;
    }
    original(m, 0x8002_e6dc, [work, listener, position, 0]);
    shared::f_8002daa0(m, [last, 0x1000, 0x1000, 0]);
    original(m, 0x8002_eec8, [far, 0, 0, 0]);
    original(m, 0x8002_e84c, [last, work, 4, 0]);
    original(m, 0x8002_e8dc, [last, work, 0, 0]);
    original(m, 0x8002_e67c, [first, 0, 0, 0]);
    original(m, 0x8002_eec8, [near, 0, 0, 0]);
    original(m, 0x8002_e84c, [first, work, 2, 0]);
    original(m, 0x8002_e8dc, [first, work, 0, 0]);
    original(m, 0x8002_ec48, [first, work, 0, 0]);
    original(m, 0x8002_ea74, [first, middle, work, 0]);
    let mut range = 0x200;
    let wide = |m: &mut Machine| {
        let gameplay = m.gw(common::GAMEPLAY);
        m.lw(gameplay + 4) & 0x20 != 0
    };
    let mut scaled = true;
    if lt(m.lw(position + 4), 0xff40_0000) {
        original(m, 0x8002_ed2c, [work, shape, 0, 0]);
        if lbu(m, shape) == 1 {
            let voice = lbu(m, shape + 3);
            original(m, 0x8002_e67c, [voice, 0, 0, 0]);
            let voice = lbu(m, shape + 3);
            original(m, 0x8002_e8dc, [voice, work, 0, 0]);
            let voice = lbu(m, shape + 3);
            original(m, 0x8002_ec48, [voice, work, 0, 0]);
            if wide(m) {
                for offset in [4, 6] {
                    let voice = lbu(m, shape + 3);
                    let at = VOICES.wrapping_add(voice * 20) + offset;
                    let volume = m.lh(at) / 2;
                    m.sh(at, volume as u16);
                }
            } else {
                scaled = false;
            }
        }
    }
    if scaled && wide(m) {
        for voice in voices {
            let at = VOICES.wrapping_add(voice * 20);
            let left = lh(m, at + 4).wrapping_mul(30);
            let right = lh(m, at + 6).wrapping_mul(30);
            m.sh(at + 4, div100(left) as u16);
            m.sh(at + 6, div100(right) as u16);
        }
        range = 0x800;
    }
    for (i, voice) in voices.into_iter().enumerate() {
        let source = VOICES.wrapping_add(voice * 20) + 4;
        original(
            m,
            0x8003_00e0,
            [voice, source, record + 0x50 + i as u32 * 4, range],
        );
    }
    for (i, voice) in voices.into_iter().enumerate() {
        // lwl/lwr + swl/swr: an unaligned-safe word copy.
        let source = VOICES.wrapping_add(voice * 20) + 4;
        for byte in 0..4 {
            let value = m.lbu(source + byte);
            m.sb(record + 0x50 + i as u32 * 4 + byte, value);
        }
    }
}

/// 800383AC (JP; PAL 8003809C, near, `imm:6 ins:1 op:2`): per-entity motion
/// requests (+0x54 flags: angle and velocity updates through 80030F64/80030EF8,
/// bit 31 integrates the velocity), then mission-specific entity effects by the
/// mission index (PAL the mission number), with the JP entity kinds 0x2D, 0x1A,
/// 0x1B, 0x19, 0x27 (PAL 0x3F, 0x14, 0x15, 0x13, 0x3C).
pub fn integrate_entities_3809c(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        for slot in 2..32u32 {
            let entity = entity(m, slot);
            if !alive(m, entity) {
                continue;
            }
            if m.lw(entity + 0x54) & 0x80_0000 != 0 {
                original(m, 0x8003_0f64, [entity + 0x18, entity + 0x30, 0, 0]);
            }
            if m.lw(entity + 0x54) & 0x40_0000 != 0 {
                original(
                    m,
                    0x8003_0ef8,
                    [entity + 0x20, entity + 0x30, entity + 0x38, 0],
                );
            }
            if m.lw(entity + 0x54) & 0x20_0000 != 0 {
                original(
                    m,
                    0x8003_0ef8,
                    [entity + 0x20, entity + 0x18, entity + 0x38, 0],
                );
            }
            let mut request = m.lw(entity + 0x54);
            if lt(request, 0) {
                let x = m.lw(entity + 8);
                let vx = m.lw(entity + 0x38);
                let vy = m.lw(entity + 0x3c);
                let vz = m.lw(entity + 0x40);
                m.sw(entity + 8, x.wrapping_add(vx));
                let y = m.lw(entity + 0xc);
                let z = m.lw(entity + 0x10);
                m.sw(entity + 0xc, y.wrapping_add(vy));
                m.sw(entity + 0x10, z.wrapping_add(vz));
                request = m.lw(entity + 0x54);
            }
            m.sw(entity + 0x54, request & 0xff00_0000);
        }
        let gameplay = m.gw(common::GAMEPLAY);
        if m.lw(gameplay + 4) == 4 {
            original(m, 0x8004_7a54, [0; 4]);
        }
        let mission = mission_index(m);
        if matches!(mission, 0x20 | 0x2f | 0x35) {
            let table = m.gw(common::ENTITIES);
            for slot in 2..32u32 {
                let entity = m.lw(table.wrapping_add(slot << 2));
                if !alive(m, entity) {
                    continue;
                }
                if lt(m.lw(entity + 0xc), 0xff4a_c000) {
                    m.sw(entity + 0xc, 0xff4a_c000);
                }
                if lt(0xfff9_c000, m.lw(entity + 0xc)) {
                    m.sw(entity + 0xc, 0xfff9_c000);
                }
            }
        }
        if mission == 6 {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity) {
                    let view = m.gw(0x33c);
                    let height = m.lw(view + 4).wrapping_neg();
                    proximity_effect_81838(m, entity + 8, entity + 0x38, height);
                }
            }
        }
        let player = m.gw(0x340);
        if matches!(mission, 0x11 | 0x1f | 0x26 | 0x2e) && lbu(m, player + 0x3c) >= 3 {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity)
                    && lh(m, entity + 0x48) == 0x2d
                    && m.lw(SLOT_FLAGS + (slot << 2)) & 0x20_0000 != 0
                {
                    let gameplay = m.gw(common::GAMEPLAY);
                    let seed = m.lw(gameplay + 0xc).wrapping_add(slot);
                    original(m, 0x8008_1c30, [entity + 8, entity + 0x38, seed, 3]);
                    break;
                }
            }
        }
        if mission.wrapping_sub(0x1f) < 3 || matches!(mission, 0x26 | 0x2e | 0x2f | 0x30 | 0x35) {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if !alive(m, entity) {
                    continue;
                }
                if lh(m, entity + 0x48) == 0x1a {
                    burning_effect(m, slot, entity, mission);
                }
                if lh(m, entity + 0x48) == 0x1b && matches!(mission, 0x21 | 0x2e | 0x35) {
                    let points = lh(m, entity + 0x4e);
                    if lt(points, 0xc9) && lt(0, points) {
                        let gameplay = m.gw(common::GAMEPLAY);
                        let seed = m.lw(gameplay + 0xc).wrapping_add(slot);
                        original(m, 0x8008_1c30, [entity + 8, entity + 0x38, seed, 2]);
                        let flags = lbu(m, entity + 5) & 0xf7;
                        m.sb(entity + 5, flags as u8);
                    }
                }
            }
        }
        if mission == 0x22 || mission == 0x36 {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity) && lh(m, entity + 0x48) == 0x19 && lbu(m, entity + 5) & 8 != 0 {
                    original(m, 0x8008_29bc, [entity + 8, entity + 0x38, 3, 0]);
                    let flags = lbu(m, entity + 5) & 0xf7;
                    m.sb(entity + 5, flags as u8);
                }
            }
        }
        if mission == 0x1b {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if !alive(m, entity) {
                    continue;
                }
                let flags = m.lw(SLOT_FLAGS + (slot << 2));
                if flags & 1 != 0 && lh(m, entity + 0x48) == 0x27 && flags & 0x20_0000 != 0 {
                    original(m, 0x8008_2d38, [entity + 8, entity + 0x38, 0, 0]);
                }
            }
        }
        if mission == 0x3a {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity) {
                    original(m, 0x8008_29bc, [entity + 8, entity + 0x38, 4, 0]);
                }
            }
        }
    });
}

/// 800383AC, entity kind 0x1A (PAL 0x14): damaged-structure smoke and sparks.
fn burning_effect(m: &mut Machine, slot: u32, entity: u32, mission: u32) {
    let points = lh(m, entity + 0x4e);
    let damaged = matches!(mission, 0x21 | 0x30 | 0x35) && lt(points, 0xc9) && lt(0, points);
    let forced = !damaged && mission == 0x26 && {
        let gameplay = m.gw(common::GAMEPLAY);
        m.lw(gameplay + 8) == 1
    };
    if damaged || forced {
        let gameplay = m.gw(common::GAMEPLAY);
        let seed = m.lw(gameplay + 0xc).wrapping_add(slot);
        original(m, 0x8008_1c30, [entity + 8, entity + 0x38, seed, 2]);
    } else {
        let player = m.gw(0x340);
        if m.lw(player) & 2 != 0 {
            return;
        }
        if mission == 0x26 {
            let gameplay = m.gw(common::GAMEPLAY);
            if lt(0, m.lw(gameplay + 8)) {
                return;
            }
        }
        if mission == 0x30 {
            let gameplay = m.gw(common::GAMEPLAY);
            if !lt(m.lw(gameplay + 8), 2) {
                return;
            }
        }
        if lbu(m, entity + 5) & 8 == 0 {
            return;
        }
        original(m, 0x8008_29bc, [entity + 8, entity + 0x38, 2, 0]);
    }
    let flags = lbu(m, entity + 5) & 0xf7;
    m.sb(entity + 5, flags as u8);
}

/// 8007D7E0 (JP; PAL 8007C164, near, `imm:1`): contrail points of present
/// entities with trail flag (+5 bit 3) and kind below 0x1F (`kind * 2 < 0x3E`;
/// PAL 0x30): two model points (800B4144 + kind * 0x10) rotated by the entity
/// angles into scratch +0x20/+0x30, then a new trail (8007CC6C) or the next
/// segment (8007CDF8) on the frame parity.
pub fn entity_trails_7c164(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        for slot in 0..32u32 {
            let entity = entity(m, slot);
            if m.lw(entity) & 0x1_4000 != 0x1_4000 || lbu(m, entity + 5) & 8 == 0 {
                let handle = trail_handle(m, 0x1_c228, slot);
                m.sb(handle, 0xff);
                continue;
            }
            let kind = lh(m, entity + 0x48);
            if !lt(kind << 1, 0x3e) || lt(kind, 0) {
                continue;
            }
            let points = (kind << 4).wrapping_add(0x800b_4144);
            shared::f_80030534(m, [entity + 0x18, SCRATCH, 0, 0]);
            sdk::set_rot_matrix(m, SCRATCH);
            let origin = [m.lw(entity + 8), m.lw(entity + 0xc), m.lw(entity + 0x10)];
            let mut far = [0u32; 3];
            for (point, out) in [(points, 0x20u32), (points + 8, 0x30)] {
                load_v0(m, point);
                rtv0(m);
                store_mac(m, SCRATCH + 0x40);
                for (offset, base) in (0..3u32).zip(origin) {
                    let scaled = m.lw(SCRATCH + 0x40 + offset * 4) << 3;
                    m.sw(SCRATCH + out + offset * 4, scaled.wrapping_add(base));
                    far[offset as usize] = scaled;
                }
            }
            let z = far[2].wrapping_add(origin[2]);
            let gameplay = m.gw(common::GAMEPLAY);
            if m.lw(gameplay + 0xc) & 1 == slot & 1 {
                let handle = trail_handle(m, 0x1_c228, slot);
                let current = lb(m, handle);
                let next = original(m, 0x8007_cc6c, [current, SCRATCH + 0x20, z, far[1]]);
                let handle = trail_handle(m, 0x1_c228, slot);
                m.sb(handle, next as u8);
            } else {
                let handle = trail_handle(m, 0x1_c228, slot);
                let current = lb(m, handle);
                if !lt(current, 0) {
                    original(m, 0x8007_cdf8, [current, SCRATCH + 0x20, z, far[1]]);
                }
            }
        }
    });
}

/// 800850D8 (JP; PAL 80083A64, changed): player proximity effects (the
/// approach effect with context halves +0x38C/+0x38E, PAL +0x390/+0x392; the
/// mission-0x2A effect, PAL 0x11), then the effect timer tables of `[gp+0x41C]`,
/// reloaded before each (PAL kept it in a register).
pub fn update_effects_83a64(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        let gameplay = m.gw(common::GAMEPLAY);
        let flags = m.lw(gameplay);
        if flags & 2 != 0 {
            let player = entity(m, 0);
            let view = m.gw(0x33c);
            let height = m.lw(view + 4).wrapping_neg();
            proximity_effect_81838(m, player + 8, player + 0x38, height);
            let player = entity(m, 0);
            if lt(0, lh(m, player + 0x4e)) {
                let context = m.gw(common::CONTEXT);
                let kind = lh(m, context + 0x38c);
                let variant = lh(m, context + 0x38e);
                approach_effect_83914(m, kind, variant);
            }
        }
        let context = m.gw(common::CONTEXT);
        if lh(m, context + 0x38c) == 0x2a {
            let player = entity(m, 0);
            if lt(0, lh(m, player + 0x4e)) {
                original(m, 0x8008_4e6c, [player + 8, player + 0x38, 0, 0]);
            }
        }
        let effects = m.gw(0x41c);
        run_timers_88600(m, effects + 0x36c4);
        let effects = m.gw(0x41c);
        move_sprites_836ac(m, effects + 0x600, 3);
        let effects = m.gw(0x41c);
        move_sprites_836ac(m, effects + 0x1098, 10);
        let effects = m.gw(0x41c);
        age_timers_88768(m, effects + 0x33c0);
        let effects = m.gw(0x41c);
        update_shake_8e8d8(m, effects + 0x3948);
        let effects = m.gw(0x41c);
        toggle_bits_8fde4(m, effects + 0x3964);
        let effects = m.gw(0x41c);
        update_explosion_91084(m, effects + 0x3b28);
    });
}

/// 80084F6C (JP; PAL 80083914, changed): approach effect (kind 0x3A, PAL
/// 0x25, or variants 0x19/0x1A): when the view is high enough and the player
/// fast enough, effect 800829BC at the player (slightly behind it unless
/// variant 0x19..0x1B). JP stores the player position to the scratchpad first
/// and the lead-corrected one over it; 800829BC reads no `a3`.
pub fn approach_effect_83914(m: &mut Machine, kind: u32, variant: u32) {
    with_frame(m, 0x30, |m, _| {
        let (limit, speed_limit, effect) = if kind == 0x3a {
            (0x780, 0x1e, 5)
        } else if variant.wrapping_sub(0x19) < 2 {
            (0xc00, 0x3c, variant.wrapping_sub(0x13))
        } else {
            return;
        };
        let view = m.lw(0x800b_d090);
        if lt(lh(m, view + 6), limit) {
            return;
        }
        let player = entity(m, 0);
        let velocity_at = player + 0x38;
        let velocity = [
            m.lw(player + 0x38),
            m.lw(velocity_at + 4),
            m.lw(velocity_at + 8),
        ];
        let args = [
            sra(velocity[0], 12),
            sra(velocity[1], 12),
            sra(velocity[2], 12),
            0,
        ];
        let speed = original(m, 0x8008_3f40, args);
        if lt(speed, speed_limit) {
            return;
        }
        let mut point = [0u32; 3];
        for (i, coordinate) in point.iter_mut().enumerate() {
            let player = entity(m, 0);
            *coordinate = m.lw(player + 8 + i as u32 * 4);
            if i < 2 {
                m.sw(SCRATCH + i as u32 * 4, *coordinate);
            }
        }
        m.sw(SCRATCH + 8, point[2]);
        if variant.wrapping_sub(0x19) >= 3 {
            for (i, (coordinate, speed)) in point.into_iter().zip(velocity).enumerate() {
                let lead = speed.wrapping_sub(sra(speed, 2));
                m.sw(SCRATCH + i as u32 * 4, coordinate.wrapping_sub(lead));
            }
        }
        original(m, 0x8008_29bc, [SCRATCH, velocity_at, effect, 0]);
    });
}

/// 800928F4 (JP; PAL 80091084, near, `imm:2`): the large explosion sequence
/// (+6 age up to 0x30): light ramp (80083054), flash over the 320x240 screen
/// rectangle (-160, -120; PAL -128 and 256 lines, 80093B2C), debris at age 9
/// (8007FDB4), fading glow (80084474).
pub fn update_explosion_91084(m: &mut Machine, state: u32) {
    with_frame(m, 0x20, |m, _| {
        if m.lw(state) == 0 {
            return;
        }
        let age = lh(m, state + 6);
        if !lt(age, 0x31) {
            m.sw(state, 0);
            return;
        }
        m.sh(state + 6, age.wrapping_add(1) as u16);
        if age.wrapping_sub(4) < 0xd {
            let level = if lt(age, 8) {
                8u32.wrapping_sub(age) << 8
            } else {
                age.wrapping_sub(8) << 6
            };
            original(m, 0x8008_3054, [state + 0x20, state + 0x14, level, 2]);
        }
        let phase = age.wrapping_sub(4);
        if phase < 5 {
            let size = sra(0x400, phase).wrapping_add(0x100);
            m.sh(SCRATCH, 0xff60);
            m.sh(SCRATCH + 2, 0xff88);
            m.sh(SCRATCH + 4, 0x140);
            m.sh(SCRATCH + 6, 0xf0);
            let gameplay = m.gw(common::GAMEPLAY);
            let list = m.lw(gameplay + 0x10).wrapping_add(8);
            let next = m.lw(gameplay + 0x14);
            let next = original(m, 0x8009_3b2c, [list, next, SCRATCH, size]);
            let gameplay = m.gw(common::GAMEPLAY);
            m.sw(gameplay + 0x14, next);
        }
        if age == 9 {
            let pool = m.gw(0x41c).wrapping_add(0x33c0);
            let debris = original(m, 0x8007_fdb4, [pool, 1, 6, 0]);
            if debris != 0 {
                m.sh(debris, 0);
                let random = sdk::rand(m);
                m.sh(debris + 2, ((random & 7) + 0x3c) as u16);
                let value = m.lw(state + 8);
                m.sw(debris + 4, value);
                let value = m.lw(state + 0xc);
                m.sw(debris + 8, value);
                let z = m.lw(state + 0x10);
                m.sh(debris + 0x10, 0x400);
                m.sh(debris + 0x12, 0);
                m.sh(debris + 0x14, 1);
                m.sw(debris + 0xc, z);
                let random = sdk::rand(m);
                m.sh(debris + 0x16, ((random & 0xff) + 0x7d0) as u16);
            }
        }
        if !lt(age, 0x10) {
            let base = 0x30u32.wrapping_sub(age) << 5;
            let random = sdk::rand(m);
            let gameplay = m.gw(common::GAMEPLAY);
            let list = m.lw(gameplay + 0x10).wrapping_add(8);
            let next = m.lw(gameplay + 0x14);
            let glow = base.wrapping_add(random & 0x1ff);
            let next = original(m, 0x8008_4474, [list, next, glow, 0]);
            let gameplay = m.gw(common::GAMEPLAY);
            m.sw(gameplay + 0x14, next);
        }
    });
}

/// 8003AA60 (JP; PAL 8003A2C4, near, `del:4 imm:2 ins:4`): keep the player's
/// lock target valid and apply the forced targets (gameplay +4 bit 2: mission
/// index 3 (PAL the mission number) target 2 or 8003ABEC/8003AC8C; bit 0x800:
/// the scripted target table 800B1054; else none).
pub fn update_player_lock_3a2c4(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        let current = lbu(m, TARGETING + 0xe);
        if current != 0xff {
            let flags = m.lw(SLOT_FLAGS + (current << 2));
            let lost = flags & 1 == 0
                || {
                    let object = m.lw(SLOT_OBJECTS + (current << 2));
                    m.lw(object) & 1 == 0
                }
                || flags & 0x4_0000 != 0;
            if lost {
                let none = shared::release_lock(m, 0);
                m.sb(TARGETING + 0xe, none as u8);
            }
        }
        let gameplay = m.gw(common::GAMEPLAY);
        let mode = m.lw(gameplay + 4);
        if mode & 4 != 0 {
            if mission_index(m) == 3 && m.lw(gameplay + 8) == 0 {
                if lbu(m, TARGETING + 0xe) != 2 {
                    m.sb(TARGETING + 0xe, 2);
                    shared::release_lock(m, 0);
                    shared::set_lock(m, 0, 2);
                }
            } else {
                switch_lock_3a450(m);
                auto_lock_3a4f0(m);
            }
        } else if mode & 0x800 != 0 {
            let table = m.gw(0x334).wrapping_add(0x800b_1054);
            let target = lbu(m, table);
            if target != lbu(m, TARGETING + 0xe) {
                m.sb(TARGETING + 0xe, target as u8);
                shared::release_lock(m, 0);
                shared::set_lock(m, 0, target);
            }
        } else {
            m.sb(TARGETING + 0xe, 0xff);
            shared::release_lock(m, 0);
            shared::set_lock(m, 0, 0xff);
        }
    });
}

/// 8003AC8C (JP; PAL 8003A4F0, near, `del:4 imm:6 ins:5 op:3`): automatic
/// targeting: count frames on an ordinary target (`gp+0x424`), and switch to the
/// best target (8003AFC0 mode 1) when none is locked, or after 31 frames when
/// the new target's object +0x1E is at least 31. JP returns what its registers
/// hold where PAL returned 0: the target when it is already locked, 1 (the
/// `slti` result) when the switch waits, 800463B8's `v0` after a switch, else
/// 0.
pub fn auto_lock_3a4f0(m: &mut Machine) -> u32 {
    with_frame(m, 0x18, |m, _| {
        if lbu(m, TARGETING + 0xe) != 0xff {
            let current = lbu(m, TARGETING + 0xe);
            let object = m.lw(SLOT_OBJECTS + (current << 2));
            if m.lw(object) & 2 != 0 || m.lw(object + 0x20) & 0x2_0000 != 0 {
                m.set_gw(0x424, 0);
                return 0;
            }
            let frames = m.gw(0x424).wrapping_add(1);
            m.set_gw(0x424, frames);
        }
        let target = choose_lock_3a820(m, 0xff, 0, 1);
        if target == 0xff {
            return 0;
        }
        let current = lbu(m, TARGETING + 0xe);
        if target == current {
            return current;
        }
        if current != 0xff {
            if lt(m.gw(0x424), 0x1f) {
                return 1;
            }
            let object = m.lw(SLOT_OBJECTS.wrapping_add(target << 2));
            if lt(lh(m, object + 0x1e), 0x1f) {
                return 1;
            }
        }
        m.sb(TARGETING + 0xe, target as u8);
        shared::release_lock(m, 0);
        let v0 = shared::set_lock(m, 0, target);
        m.set_gw(0x424, 0);
        v0
    })
}

/// 800BD0C4: last collision face (written by the projectile collision code).
const LAST_FACE: u32 = 0x800b_d0c4;

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

/// Particle record `index` (0x3C bytes from block +0x15D0).
fn particle(m: &mut Machine, index: u32) -> u32 {
    let block = m.gw(GP_SHOTS);
    block.wrapping_add(index.wrapping_mul(60).wrapping_add(0x15d0))
}

/// Collect the collidable entities (word & 0x4200 == 0x4200) for the
/// collision passes: positions >> 12 and flags at scratch +0x10 each,
/// (radius * 3, radius²) pairs at `radii` + 8 each. Returns the count.
fn collect_targets(m: &mut Machine, radii: u32) -> u32 {
    let mut count = 0u32;
    let table = m.gw(common::ENTITIES);
    for slot in 0..64u32 {
        let entity = m.lw(table.wrapping_add(slot << 2));
        let word = m.lw(entity);
        if word & 0x4200 != 0x4200 {
            continue;
        }
        let x = sra(m.lw(entity + 8), 12);
        let y = sra(m.lw(entity + 0xc), 12);
        let z = sra(m.lw(entity + 0x10), 12);
        let radius = lh(m, entity + 0x4a);
        let at = SCRATCH + count * 0x10;
        m.sw(at, x);
        m.sw(at + 4, y);
        m.sw(at + 8, z);
        m.sw(at + 0xc, word);
        let pair = radii + count * 8;
        m.sw(pair, radius.wrapping_mul(3));
        m.sw(pair + 4, lo(radius, radius));
        count += 1;
    }
    count
}

/// The broad-phase test of the collision passes against collected target
/// `index`: Manhattan distance within radius*3 + `reach`, then squared
/// distance within radius² + `reach2`. Returns the target's flags on a hit.
#[allow(clippy::too_many_arguments)]
fn touches(
    m: &mut Machine,
    radii: u32,
    index: u32,
    exclude: u32,
    position: [u32; 3],
    reach: u32,
    reach2: u32,
) -> Option<u32> {
    let at = SCRATCH + index * 0x10;
    let flags = m.lw(at + 0xc);
    if flags & exclude == 0 {
        return None;
    }
    let dx = m.lw(at).wrapping_sub(position[0]);
    let dz = m.lw(at + 8).wrapping_sub(position[2]);
    let dy = m.lw(at + 4).wrapping_sub(position[1]);
    let manhattan = abs(dx).wrapping_add(abs(dy)).wrapping_add(abs(dz));
    let pair = radii + index * 8;
    if lt(m.lw(pair).wrapping_add(reach), manhattan) {
        return None;
    }
    let square = lo(dx, dx).wrapping_add(lo(dy, dy)).wrapping_add(lo(dz, dz));
    if lt(m.lw(pair + 4).wrapping_add(reach2), square) {
        return None;
    }
    Some(flags)
}

/// `|vx| + |vy| + |vz| >> 13`, its square / 9 (as compiled): the reach terms.
fn reach_of(velocity: [u32; 3]) -> (u32, u32) {
    let reach = sra(
        abs(velocity[0])
            .wrapping_add(abs(velocity[1]))
            .wrapping_add(abs(velocity[2])),
        13,
    );
    let square = lo(reach, reach);
    let ninth = sra(hi(square, 0x38e3_8e39), 1).wrapping_sub(sra(square, 31));
    (reach, ninth)
}

/// Terrain grid cell of a world coordinate pair (x, z) as the collision
/// passes compute it.
fn terrain_cell(x: u32, z: u32) -> u32 {
    let clamp = |value: u32| {
        let mut cell = sra(value, 23).wrapping_add(0x80);
        if lt(cell, 0) {
            cell &= 7;
        }
        if !lt(cell, 0x100) {
            cell = (cell & 7) + 0xf8;
        }
        cell
    };
    let cx = clamp(x);
    let cz = clamp(z);
    cx.wrapping_add(cz << 8)
}

/// The terrain test shared by both collision passes: same-cell moves skip
/// cells whose height is above both old and new heights.
#[allow(clippy::too_many_arguments)]
fn terrain_hit(
    m: &mut Machine,
    terrain: u32,
    heights: u32,
    from: u32,
    old: [u32; 3],
    new: [u32; 3],
    target: u32,
    hit: u32,
) -> Option<u32> {
    let before = terrain_cell(old[0], old[2]);
    let after = terrain_cell(new[0], new[2]);
    m.sw(target, new[0]);
    m.sw(target + 4, new[1]);
    m.sw(target + 8, new[2]);
    let result = if before == after {
        let tile = lhu(m, heights.wrapping_add(before << 1)) & 0x1ff;
        if tile == 0 {
            return None;
        }
        let tiles = m.lw(terrain + 8);
        let info = m.lw(tiles.wrapping_add(tile << 2));
        let top = lh(m, info + 0x16);
        if lt(sra(old[1], 12), top) && lt(sra(new[1], 12), top) {
            return None;
        }
        original(m, 0x8009_a5d0, [from, target, hit, SCRATCH])
    } else {
        original(m, 0x8009_a800, [from, target, hit, SCRATCH])
    };
    (!lt(0x7fff_fffe, result)).then_some(result)
}

/// The sound selection of the particle hits: a0 of 8002F460 and a2.
fn hit_sound(m: &mut Machine, record: u32, first: bool) -> (u32, u32) {
    let flags = m.lw(record);
    if first {
        return (if flags & 0xc_0000 == 0 { 0x1f } else { 0x2a }, 2);
    }
    if flags & 0x4_0000 != 0 {
        (0x28, 2)
    } else if flags & 0x8_0000 != 0 {
        (0x29, 1)
    } else {
        (0x1e, 2)
    }
}

/// The effect variant of a projectile/particle (flags 0x20..0x200 → 1..5).
fn effect_variant(flags: u32) -> u32 {
    if flags & 0x20 != 0 {
        1
    } else if flags & 0x40 != 0 {
        2
    } else if flags & 0x80 != 0 {
        3
    } else if flags & 0x100 != 0 {
        4
    } else if flags & 0x200 != 0 {
        5
    } else {
        0
    }
}

/// The impact sound of a terrain or bounds hit (8002F460 a0/a2).
fn impact_sound(m: &mut Machine, flags: u32) -> (u32, u32) {
    if flags & 0x2_0000 != 0 {
        let face = m.lw(LAST_FACE) & 0xf;
        (if face == 1 { 0x23 } else { 0x1e }, 2)
    } else if flags & 0x4_0000 != 0 {
        let face = m.lw(LAST_FACE) & 0xf;
        (if face == 1 { 0x23 } else { 0x28 }, 2)
    } else if flags & 0x8_0000 != 0 {
        (0x29, 1)
    } else {
        (0x1e, 2)
    }
}

/// 8006B504, third pass: world bounds (gameplay +0x2A0) and terrain.
fn bounds_and_terrain_particles(m: &mut Machine, frame: u32) {
    let masks = frame + 0x280;
    let gameplay = m.gw(common::GAMEPLAY);
    let bounds = gameplay + 0x2a0;
    let terrain = gameplay + 0x244;
    let block = m.gw(GP_SHOTS);
    m.sw(masks, block + 0x33d0);
    let heights = m.lw(terrain + 4);
    for group in (0..0x80u32).step_by(0x20) {
        let mask_at = m.lw(masks);
        let mask = m.lw(mask_at);
        for index in 0..32u32 {
            if mask == 0 || mask & bit(index) == 0 {
                continue;
            }
            let record = particle(m, group + index);
            let old = [m.lw(record + 8), m.lw(record + 0xc), m.lw(record + 0x10)];
            let mut new = [
                m.lw(record + 0x28).wrapping_add(old[0]),
                m.lw(record + 0x2c).wrapping_add(old[1]),
                m.lw(record + 0x30).wrapping_add(old[2]),
            ];
            let face = clamp_to_bounds(m, bounds, &mut new).1;
            let point = frame + 0x270;
            if face != 0 {
                m.sw(record + 4, 0);
                m.sw(point, new[0]);
                m.sw(point + 4, new[1]);
                m.sw(point + 8, new[2]);
                let word = m.lw(record);
                m.sw(LAST_FACE, face);
                let (sound, mode) = if word & 0x2_0000 != 0 {
                    (if face & 0xf == 1 { 0x23 } else { 0x1e }, 2)
                } else if word & 0x4_0000 != 0 {
                    (0x28, 2)
                } else if word & 0x8_0000 != 0 {
                    (0x29, 1)
                } else {
                    (0x1e, 2)
                };
                original(m, 0x8002_f460, [sound, point, mode, new[2]]);
            } else {
                let target = frame + 0x260;
                if terrain_hit(m, terrain, heights, record + 8, old, new, target, point).is_none() {
                    continue;
                }
                let word = m.lw(record);
                m.sw(record + 4, 0);
                let (sound, mode) = impact_sound(m, word);
                original(m, 0x8002_f460, [sound, point, mode, 0]);
            }
            let variant = effect_variant(m.lw(record));
            let face = m.lw(LAST_FACE);
            original(m, 0x8008_04b8, [point, face, variant, 0]);
        }
        let mask_at = m.lw(masks);
        m.sw(masks, mask_at + 4);
    }
}

/// Clamp a position to the six world bounds; returns (axis code 1..6 of the
/// last clamp or 0, face code of the collision passes).
fn clamp_to_bounds(m: &mut Machine, bounds: u32, position: &mut [u32; 3]) -> (u32, u32) {
    let mut axis = 0;
    let mut face = 0;
    let limits: [(usize, u32, bool, u32); 6] = [
        (0, 0, true, 4),
        (0, 4, false, 4),
        (1, 8, true, 3),
        (1, 0xc, false, 1),
        (2, 0x10, true, 4),
        (2, 0x14, false, 4),
    ];
    for (i, (coordinate, offset, minimum, code)) in limits.into_iter().enumerate() {
        let limit = m.lw(bounds + offset);
        let outside = if minimum {
            lt(position[coordinate], limit)
        } else {
            lt(limit, position[coordinate])
        };
        if outside {
            axis = i as u32 + 1;
            face = code;
            position[coordinate] = limit;
        }
    }
    (axis, face)
}

/// 8006B504, fourth and fifth passes: motion, drag/gravity and expiry.
fn integrate_particles(m: &mut Machine, frame: u32) {
    let masks = frame + 0x280;
    let expired = frame + 0x218;
    let block = m.gw(GP_SHOTS);
    m.sw(masks, block + 0x33d0);
    m.gte.mtc2(reg::IR0, 0xfa0);
    let mut count = 0u32;
    for group in (0..0x80u32).step_by(0x20) {
        let mask_at = m.lw(masks);
        let mut mask = m.lw(mask_at);
        if mask == 0 {
            m.sw(masks, mask_at + 4);
            continue;
        }
        for index in 0..32u32 {
            if mask & bit(index) == 0 {
                continue;
            }
            let number = group + index;
            let record = particle(m, number);
            let life = m.lw(record + 4).wrapping_sub(1);
            m.sw(record + 4, life);
            if lt(life, 0) {
                mask &= !bit(index);
                m.sb(expired + count, number as u8);
                count += 1;
                continue;
            }
            let mut vx = m.lw(record + 0x28);
            let mut vy = m.lw(record + 0x2c);
            let x = m.lw(record + 8);
            let mut vz = m.lw(record + 0x30);
            let z = m.lw(record + 0x10);
            m.sw(record + 8, x.wrapping_add(vx));
            let y = m.lw(record + 0xc);
            m.sw(record + 0x10, z.wrapping_add(vz));
            let ty = m.lw(record + 0x1c);
            m.sw(record + 0xc, y.wrapping_add(vy));
            let tx = m.lw(record + 0x18);
            m.sw(record + 0x1c, ty.wrapping_add(vy));
            let flags = !m.lw(record);
            m.sw(record + 0x18, tx.wrapping_add(vx));
            let tz = m.lw(record + 0x20);
            m.sw(record + 0x20, tz.wrapping_add(vz));
            if flags & 2 != 0 {
                vy = vy.wrapping_add(0x444);
            }
            vx = sra(vx, 6);
            vy = sra(vy, 6);
            vz = sra(vz, 6);
            if flags & 1 != 0 {
                m.gte.mtc2(reg::IR1, vx);
                m.gte.mtc2(reg::IR2, vy);
                m.gte.mtc2(reg::IR3, vz);
                m.gte.gpf(true, false);
                [vx, vy, vz] = read_mac(m);
            }
            m.sw(record + 0x28, vx << 6);
            m.sw(record + 0x2c, vy << 6);
            m.sw(record + 0x30, vz << 6);
        }
        let mask_at = m.lw(masks);
        m.sw(mask_at, mask);
        m.sw(masks, mask_at + 4);
    }
    for index in 0..count {
        let number = lbu(m, expired + index);
        let block = m.gw(GP_SHOTS);
        let flags = m.lw(block.wrapping_add(number.wrapping_mul(60)) + 0x1608);
        if flags & 0x2000 != 0 {
            write_expiry_event_7d284(m, number);
        }
    }
}

fn velocity_words(m: &mut Machine, address: u32) -> [u32; 3] {
    [m.lw(address), m.lw(address + 4), m.lw(address + 8)]
}

/// Projectile entity hit: keep the velocity at +0x8C, stop, state 2.
fn stop_projectile(m: &mut Machine, record: u32, speed: [u32; 3]) {
    m.sw(record + 0x38, 0);
    m.sw(record + 0x3c, 0);
    m.sw(record + 0x40, 0);
    m.sw(record + 0x8c, speed[0]);
    m.sw(record + 0x90, speed[1]);
    m.sw(record + 0x94, speed[2]);
    set_word_bits(m, record, 2);
    m.sh(record + 0x18, 0);
    m.sb(record + 4, 2);
    m.sh(record + 8, 0);
    m.sh(record + 0xa, 0);
}

/// 8006A11C, third pass: world bounds and terrain for projectiles; a hit
/// stores the surface normal (800245C8 table or the terrain normal at
/// `[0x800BD0C0]`) and bounces (state 2).
fn projectile_bounds(m: &mut Machine, frame: u32) {
    let gameplay = m.gw(common::GAMEPLAY);
    let block = m.gw(GP_SHOTS);
    let bounds = gameplay + 0x2a0;
    let terrain = gameplay + 0x244;
    let heights = m.lw(terrain + 4);
    for index in 0..32u32 {
        let active = m.lw(frame + 0x278);
        if !active & bit(index) != 0 {
            continue;
        }
        let record = block + 0xc8 + index * 0xa4;
        let old = [
            m.lw(record + 0x1c),
            m.lw(record + 0x20),
            m.lw(record + 0x24),
        ];
        let mut new = [
            m.lw(record + 0x38).wrapping_add(old[0]),
            m.lw(record + 0x3c).wrapping_add(old[1]),
            m.lw(record + 0x40).wrapping_add(old[2]),
        ];
        let (axis, face) = clamp_to_bounds(m, bounds, &mut new);
        if axis != 0 {
            set_word_bits(m, record, 4);
            let normal = 0x8002_45c8 + axis * 8;
            m.sh(record + 0x18, 0);
            m.sw(record + 0x1c, new[0]);
            m.sw(record + 0x20, new[1]);
            m.sw(record + 0x24, new[2]);
            let value = m.lhu(normal);
            let depth = m.lw(record + 0x40);
            m.sh(record + 0x10, value);
            let value = m.lhu(normal + 2);
            m.sh(record + 0x12, value);
            let last = m.lhu(normal + 4);
            let speed = [m.lw(record + 0x38), m.lw(record + 0x3c)];
            m.sw(LAST_FACE, face);
            m.sw(record + 0xc, face);
            m.sw(record + 0x94, depth);
            bounce(m, record, speed, last);
            continue;
        }
        let target = frame + 0x258;
        let hit = frame + 0x268;
        if terrain_hit(m, terrain, heights, record + 0x1c, old, new, target, hit).is_none() {
            continue;
        }
        set_word_bits(m, record, 4);
        let normal = m.lw(0x800b_d0c0);
        m.sh(record + 0x18, 0);
        let x = m.lw(hit);
        let vx = m.lw(record + 0x38);
        let vz = m.lw(record + 0x40);
        m.sw(record + 0x1c, x);
        let y = m.lw(hit + 4);
        m.sw(record + 0x20, y);
        let z = m.lw(hit + 8);
        let vy = m.lw(record + 0x3c);
        m.sw(record + 0x8c, vx);
        m.sw(record + 0x94, vz);
        m.sw(record + 0x38, 0);
        m.sw(record + 0x3c, 0);
        m.sw(record + 0x40, 0);
        m.sw(record + 0x90, vy);
        m.sw(record + 0x24, z);
        let value = m.lhu(normal);
        m.sh(record + 0x10, value);
        let value = m.lhu(normal + 2);
        m.sh(record + 0x12, value);
        let face = m.lw(LAST_FACE);
        let last = m.lhu(normal + 4);
        m.sh(record + 8, 0);
        m.sh(record + 0xa, 0);
        m.sw(record + 0xc, face);
        m.sb(record + 4, 2);
        m.sh(record + 0x14, last);
    }
}

/// Bounds hit tail of 8006A11C (velocity x/y kept at +0x8C/+0x90).
fn bounce(m: &mut Machine, record: u32, speed: [u32; 2], normal_z: u16) {
    m.sw(record + 0x38, 0);
    m.sw(record + 0x3c, 0);
    m.sw(record + 0x40, 0);
    m.sh(record + 8, 0);
    m.sh(record + 0xa, 0);
    m.sw(record + 0x8c, speed[0]);
    m.sw(record + 0x90, speed[1]);
    m.sb(record + 4, 2);
    m.sh(record + 0x14, normal_z);
}

/// 8006A11C (JP; PAL 8006894C, exact; patched for its model-ray carry):
/// projectile collisions (records of 0xA4 bytes from block +0xC8, active
/// mask +0x1548): entity hits, detailed hits (8009B4EC), world bounds and
/// terrain bounces.
///
/// JP's model ray 8009A944 reads the register s4 its caller's caller left,
/// through 8009B4EC: here the exclusion mask `!word(+0x6C) & 0x1C0` of the
/// last active record (8006A2C4, set per record while there are targets and
/// unchanged up to the call at 8006A4C4), so the detailed hits go through
/// [`object_ray_jp`](crate::jp::res_p1_player::object_ray_jp) with it.
pub fn projectile_collisions_6894c(m: &mut Machine) {
    with_frame(m, 0x2a8, |m, frame| {
        let radii = frame + 0x18;
        let hits = frame + 0x218;
        let block = m.gw(GP_SHOTS);
        let active = m.lw(block + 0x1548);
        m.sw(frame + 0x278, active);
        if active == 0 {
            return;
        }
        let targets = collect_targets(m, radii);
        let mut hit_count = 0u32;
        // s4: the last active record's exclusion mask (set only while there
        // are targets), which 8009B4EC hands to the model ray as its carry.
        let mut carry = 0u32;
        let block = m.gw(GP_SHOTS);
        for index in 0..32u32 {
            if active & bit(index) == 0 {
                continue;
            }
            let record = block + 0xc8 + index * 0xa4;
            let velocity = [
                m.lw(record + 0x38),
                m.lw(record + 0x3c),
                m.lw(record + 0x40),
            ];
            let (reach, reach2) = reach_of(velocity);
            let position = [
                sra(m.lw(record + 0x1c).wrapping_add(velocity[0]), 12),
                sra(m.lw(record + 0x20).wrapping_add(velocity[1]), 12),
                sra(m.lw(record + 0x24).wrapping_add(velocity[2]), 12),
            ];
            let exclude = !m.lw(record + 0x6c) & 0x1c0;
            if targets > 0 {
                carry = exclude;
            }
            for target in 0..targets {
                let Some(flags) = touches(m, radii, target, exclude, position, reach, reach2)
                else {
                    continue;
                };
                let struck = entity(m, flags & 0x3f);
                if flags & 0x1800 != 0 {
                    let entry = hits + hit_count * 8;
                    m.sw(entry, record);
                    m.sw(entry + 4, struck);
                    if lt(hit_count, 8) {
                        hit_count += 1;
                    }
                    continue;
                }
                let damage = lhu(m, record + 0x1a);
                set_word_bits(m, struck + 0x54, damage);
                let colour = m.lw(record + 0x6c);
                m.sw(struck + 0x50, colour);
                let speed = velocity_words(m, record + 0x38);
                m.sw(record + 0x1c, position[0] << 12);
                m.sw(record + 0x20, position[1] << 12);
                m.sw(record + 0x24, position[2] << 12);
                stop_projectile(m, record, speed);
            }
        }
        for index in 0..hit_count {
            let entry = hits + index * 8;
            let struck = m.lw(entry + 4);
            let record = m.lw(entry);
            let point = frame + 0x258;
            let result = if m.lw(struck) & 0x1020 == 0x1020 && lbu(m, struck + 5) & 2 == 0 {
                m.sw(frame + 0x10, SCRATCH);
                crate::jp::res_p1_player::object_ray_jp(
                    m,
                    record + 0x1c,
                    record + 0x38,
                    struck,
                    point,
                    SCRATCH,
                    carry,
                )
            } else {
                for offset in [0, 4, 8] {
                    let value = m.lw(record + 0x1c + offset);
                    m.sw(point + offset, value);
                }
                0
            };
            if lt(0x7fff_fffe, result) {
                continue;
            }
            let damage = lhu(m, record + 0x1a);
            set_word_bits(m, struck + 0x54, damage);
            let colour = m.lw(record + 0x6c);
            m.sw(struck + 0x50, colour);
            let x = m.lw(point);
            let speed = velocity_words(m, record + 0x38);
            m.sw(record + 0x1c, x);
            let y = m.lw(point + 4);
            m.sw(record + 0x20, y);
            let z = m.lw(point + 8);
            let word = m.lw(record);
            m.sw(record + 0x8c, speed[0]);
            m.sw(record + 0x90, speed[1]);
            m.sw(record + 0x94, speed[2]);
            m.sw(record + 0x38, 0);
            m.sw(record + 0x3c, 0);
            m.sw(record + 0x40, 0);
            m.sh(record + 0x18, 0);
            m.sh(record + 8, 0);
            m.sh(record + 0xa, 0);
            m.sw(record, word | 2);
            m.sb(record + 4, 2);
            m.sw(record + 0x24, z);
        }
        projectile_bounds(m, frame);
    });
}

/// 8006B504 (JP; PAL 80069D34, exact; patched for its model-ray carry):
/// particle pass: entity hits (damage flags 0x54/0x50, sounds, 800800E0),
/// detailed hits through 8009B4EC, world bounds and terrain, then motion with
/// gravity/drag (GTE GPF, IR0 = 0xFA0) and expiry.
///
/// JP's model ray 8009A944 reads the register s4 its caller's caller left,
/// through 8009B4EC: here the scratchpad target cursor as the first pass
/// leaves it (0x1F800000 per particle, plus 0x10 per target, so 0x1F800000 +
/// 0x10 * targets), so the detailed hits (call at 8006BA30) go through
/// [`object_ray_jp`](crate::jp::res_p1_player::object_ray_jp) with it.
pub fn update_particles_69d34(m: &mut Machine) {
    with_frame(m, 0x2d0, |m, frame| {
        let radii = frame + 0x18;
        let hits = frame + 0x220;
        let masks = frame + 0x280;
        let targets = collect_targets(m, radii);
        let mut hit_count = 0u32;
        // s4: the scratchpad target cursor, 0x1F800000 per particle
        // (8006B674) plus 0x10 per target (8006B9A0), which 8009B4EC hands
        // to the model ray as its carry.
        let mut carry = SCRATCH;
        let block = m.gw(GP_SHOTS);
        m.sw(masks, block + 0x33d0);
        for group in (0..0x80u32).step_by(0x20) {
            let mask_at = m.lw(masks);
            let mask = m.lw(mask_at);
            for index in 0..32u32 {
                if mask == 0 || mask & bit(index) == 0 {
                    continue;
                }
                let record = particle(m, group + index);
                let velocity = [
                    m.lw(record + 0x28),
                    m.lw(record + 0x2c),
                    m.lw(record + 0x30),
                ];
                let (reach, reach2) = reach_of(velocity);
                let position = [
                    sra(m.lw(record + 8).wrapping_add(velocity[0]), 12),
                    sra(m.lw(record + 0xc).wrapping_add(velocity[1]), 12),
                    sra(m.lw(record + 0x10).wrapping_add(velocity[2]), 12),
                ];
                let exclude = !m.lw(record + 0x38) & 0x1c0;
                carry = SCRATCH.wrapping_add(targets.wrapping_mul(0x10));
                for target in 0..targets {
                    let Some(flags) = touches(m, radii, target, exclude, position, reach, reach2)
                    else {
                        continue;
                    };
                    let struck = entity(m, flags & 0x3f);
                    if flags & 0x1800 != 0 {
                        let entry = hits + hit_count * 8;
                        m.sw(entry, record);
                        m.sw(entry + 4, struck);
                        if lt(hit_count, 8) {
                            hit_count += 1;
                        }
                        continue;
                    }
                    let word = m.lw(record);
                    let damage = [
                        (0x400, 1),
                        (0x800, 2),
                        (0x1000, 4),
                        (0x2000, 8),
                        (0x4000, 0x10),
                        (0x8000, 0x20),
                    ]
                    .into_iter()
                    .find(|&(mask, _)| word & mask != 0);
                    if let Some((_, damage)) = damage {
                        set_word_bits(m, struck + 0x54, damage);
                    }
                    let colour = m.lw(record + 0x38);
                    m.sw(struck + 0x50, colour);
                    m.sw(record + 8, position[0] << 12);
                    m.sw(record + 0xc, position[1] << 12);
                    m.sw(record + 0x10, position[2] << 12);
                    m.sw(record + 4, 0);
                    let (sound, mode) = hit_sound(m, record, target == 0);
                    let dx = m.lw(SCRATCH + target * 0x10).wrapping_sub(position[0]);
                    original(m, 0x8002_f460, [sound, record + 8, mode, dx]);
                    let variant = effect_variant(m.lw(record));
                    original(m, 0x8008_00e0, [record + 8, variant, 0, 0]);
                }
            }
            let mask_at = m.lw(masks);
            m.sw(masks, mask_at + 4);
        }
        for index in 0..hit_count {
            let entry = hits + index * 8;
            let struck = m.lw(entry + 4);
            let record = m.lw(entry);
            let point = frame + 0x260;
            let result = if m.lw(struck) & 0x1020 == 0x1020 && lbu(m, struck + 5) & 2 == 0 {
                m.sw(frame + 0x10, SCRATCH);
                crate::jp::res_p1_player::object_ray_jp(
                    m,
                    record + 8,
                    record + 0x28,
                    struck,
                    point,
                    SCRATCH,
                    carry,
                )
            } else {
                for offset in [0, 4, 8] {
                    let value = m.lw(record + 8 + offset);
                    m.sw(point + offset, value);
                }
                0
            };
            if lt(0x7fff_fffe, result) {
                continue;
            }
            let word = m.lw(record);
            let damage = [(0x400, 1), (0x800, 2), (0x1000, 4)]
                .into_iter()
                .find(|&(mask, _)| word & mask != 0);
            if let Some((_, damage)) = damage {
                set_word_bits(m, struck + 0x54, damage);
            }
            let colour = m.lw(record + 0x38);
            m.sw(struck + 0x50, colour);
            let word = m.lw(record);
            m.sw(record + 4, 0);
            let (sound, mode) = impact_sound(m, word);
            original(m, 0x8002_f460, [sound, point, mode, 0]);
            let variant = effect_variant(m.lw(record));
            let face = m.lw(LAST_FACE);
            original(m, 0x8008_04b8, [point, face, variant, 0]);
        }
        bounds_and_terrain_particles(m, frame);
        integrate_particles(m, frame);
    });
}
