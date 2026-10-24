//! World and entity updates called from the resident flight update 80032CA4:
//! the 80038728 (entity logic, tracking sounds), 8006ADE4 (missiles and
//! particles), 8007C44C (trails), 80083A64 (timers and effects), 80039D08
//! (lock-on) and 8007DA7C (event recording) trees, plus 8007D214.
//!
//! Besides every routine those trees run in the first flight frames, the
//! module translates routines they reach in later frames: the object event
//! writers (8007CC34 and its wrappers), lock selection (8003A450, 8003A4F0,
//! 8003A820, 8003AB44, 80039FB4), the 80045DA8 checks (80046828, 80046968,
//! 80046BD4), movement states 0/1/4/10 (8004DECC, 8004D794,
//! 8004DA2C, 8004E504 with 8004EFBC/8004F070), steering (80043F24 with
//! 80043AF4/80043BC4/80043D3C/80043E94) and throttle (800440E0 with
//! 8006D614). Other AI state and action routines, missile behaviours,
//! terrain collision, sound positioning and effect spawners stay outside.
//!
//! The lock links (8004577C, 80045874), the event ring (8007C8AC, 8007C7E8
//! with 8007C778/8007C5C0/8007C6DC) and the word copy 800270FC are shared
//! with the mission script and initialization and live in `shared.rs`; the
//! area check 80046AA8 is `hud.rs`'s (reached by original address).
//!
//! Conventions:
//! - Every routine that has a stack frame in the original allocates the same
//!   frame size on the guest stack (`push_frame`), so locals passed by address
//!   (and stale stack words that GTE loads pick up) sit at their original
//!   addresses.
//! - Routines outside this module without a native owner yet are reached by
//!   original address through [`dispatch::call`] (`original`), which raises
//!   `Fault::Untranslated` until a module claims them. Argument registers the
//!   original leaves undefined (clobbered by an earlier call) are passed as 0.
use crate::{
    common, dispatch,
    machine::{GP, Machine, SmallData},
    script, sdk, shared,
};
use ac3_gte::reg;

// ---- globals ------------------------------------------------------------------

/// 800C4D98: per-slot status words (32 slots).
const SLOT_FLAGS: u32 = 0x800c_4d98;
/// 800C4C98: per-slot object (type description) pointers.
const SLOT_OBJECTS: u32 = 0x800c_4c98;
/// 800C4B48: per-slot lock links, 4 bytes: +1 the slot's target, +2/+3 the
/// slots tracking it (0xFF = none).
const LINKS: u32 = 0x800c_4b48;
/// 800C4B28: lock-on block: +0 timer, +4 level, +0xE player target,
/// +0x12 secondary target, +0x13 reported target.
const TARGETING: u32 = 0x800c_4b28;
/// 800C4C68: HUD status block (+0x20, +0x24, +0x2C flags).
const STATUS: u32 = 0x800c_4c68;
/// 800BAEB4: current mission number (byte).
const MISSION: u32 = 0x800b_aeb4;
/// 800C43D8: tracked-sound channel count; voice bytes at +4/+8/+0xC + index.
const TRACKS: u32 = 0x800c_43d8;
/// 800C43F8: tracked-sound channel records (0x5C bytes).
const TRACK_RECORDS: u32 = 0x800c_43f8;
/// 800C35F8: sound voice records (0x14 bytes).
const VOICES: u32 = 0x800c_35f8;
/// 800BAF4C: last collision face (written by the projectile collision code).
const LAST_FACE: u32 = 0x800b_af4c;
/// Scratchpad base.
const SCRATCH: u32 = 0x1f80_0000;

/// `[gp+0x33C]`: per-slot AI records (0xB8 bytes, indexed by slot).
const GP_SLOT_RECORDS: u32 = 0x33c;
/// `[gp+0x514]` (800BAF08, also loaded by absolute address): the shots
/// block: +0/+4 masks of entities targeted by missiles, +8 the missile
/// nearest the player, +0x48/+0x88 per-entity launch/target counts,
/// missiles of 0xA4 bytes from +0xC8 (active mask +0x1548), particle spawn
/// requests (+0x154C), particles of 0x3C bytes from +0x15D0 (masks +0x33D0),
/// delayed hits (+0x33E0).
const GP_SHOTS: u32 = 0x514;
/// `[gp+0x544]`: trail queues.
const GP_TRAILS: u32 = 0x544;
/// `[gp+0x54C]`: event recorder (ring buffer and entity copies).
const GP_EVENTS: u32 = 0x54c;

// ---- helpers ------------------------------------------------------------------

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
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
fn lb(m: &mut Machine, address: u32) -> u32 {
    m.lb(address) as i32 as u32
}
fn lbu(m: &mut Machine, address: u32) -> u32 {
    u32::from(m.lbu(address))
}
fn bit(index: u32) -> u32 {
    1u32 << (index & 31)
}
/// Entity pointer `slot` of the 64-entry table `[gp+0x2D8]`.
fn entity(m: &mut Machine, slot: u32) -> u32 {
    let table = m.gw(common::ENTITIES);
    m.lw(table.wrapping_add(slot << 2))
}
/// Both 0x4000 (present) and 0x8000 set in the entity word.
fn alive(m: &mut Machine, entity: u32) -> bool {
    m.lw(entity) & 0xc000 == 0xc000
}
/// The per-slot AI record of `slot` (`[gp+0x33C] + slot * 0xB8`).
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

// GTE macros as inlined by the original.

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

// ==== 80038728: entity logic ======================================================

/// 80038728: per-frame entity logic: marks, AI, then motion.
pub fn update_entities_38728(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        mark_entities_37dfc(m);
        run_entity_logic_37da0(m);
        integrate_entities_3809c(m);
    });
}

/// 80037DFC: clear entity bit 0x8000 for slots 2..31 and set it again for
/// present entities that are (or are tracked by) something noteworthy;
/// also refresh slot flag 0x40000 (strong, flagged 0x80).
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
        if !lt(lh(m, entity + 0x4e), 0x2710) && m.lw(entity) & 0x80 != 0 {
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

/// 80037DA0: clear status bit 0 and run the slot AI passes.
pub fn run_entity_logic_37da0(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        clear_word_bits(m, STATUS + 0x2c, 1);
        retire_entities_459c0(m);
        reset_idle_45c10(m);
        select_behaviour_45da8(m);
        run_movement_461b8(m);
        run_actions_463d8(m);
        nearest_targets_47598(m);
    });
}

/// 800459C0: handle slots whose entity ran out of hit points (+0x4E <= 0):
/// kill counters, the respawn script (flag 0x80000), destruction effects
/// (flag 0x800), the per-slot death script.
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
                let player = m.gw(0x330);
                let count = lbu(m, player + 0x20).wrapping_add(1);
                m.sb(player + 0x20, count as u8);
            }
            if m.lw(flags_at) & 0x200 != 0 {
                let player = m.gw(0x330);
                let count = lbu(m, player + 0x21).wrapping_add(1);
                m.sb(player + 0x21, count as u8);
            }
            let flags = m.lw(flags_at);
            if flags & 0x8_0000 != 0 {
                let mission = m.gw(0x328);
                let respawn = m.lw(mission + 0x154);
                script::interpret(m, respawn);
                let table = m.gw(0x374);
                let info = m.lw(table.wrapping_add(scripts) + 0x10);
                let points = m.lhu(info);
                m.sh(entity + 0x4e, points);
                let flags = m.lw(flags_at) & 0xfff7_ffff;
                m.sw(flags_at, flags);
                if lh(m, entity + 0x48) != 0x15 {
                    continue;
                }
                let mission = lbu(m, MISSION);
                if mission != 0x21 && mission != 0x3a {
                    continue;
                }
                m.sw(flags_at, flags | 0x800);
                original(m, 0x8004_7a0c, [slot, 0, 0, 0]);
                continue;
            }
            if flags & 0x800 != 0
                && m.lw(entity + 0x50) & 0x3f == 0
                && m.lw(entity) & 0x40 == 0
                && lh(m, entity + 0x48) != 0x3e
            {
                let context = m.gw(common::CONTEXT);
                if lbu(m, context + 0x3a1) & 1 != 0 {
                    original(m, 0x8007_818c, [0; 4]);
                }
                if m.lw(entity) & 0x80 != 0 {
                    let gameplay = m.gw(common::GAMEPLAY);
                    let sound = if m.lw(gameplay + 0xc) & 7 != 0 { 7 } else { 6 };
                    original(m, 0x8004_b760, [sound, 0, 0, 0]);
                }
            }
            original(m, 0x8004_7a0c, [slot, 0, 0, 0]);
            original(m, 0x8003_7fc4, [slot, 0, 0, 0]);
            let table = m.gw(0x374);
            let death = m.lw(table.wrapping_add(scripts) + 0x34);
            if death != 0 {
                script::interpret(m, death);
            }
        }
    });
}

/// 80045C10: reset the behaviour bytes of present slots whose state is 0xFF
/// unless 80045CE4 keeps them, then the group and formation passes.
pub fn reset_idle_45c10(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        for slot in 2..32u32 {
            let entity = entity(m, slot);
            let record = slot_record(m, slot);
            if !alive(m, entity) {
                continue;
            }
            let state = lbu(m, record + 0x1c);
            if state != 0xff || original(m, 0x8004_5ce4, [slot, 0, 0, 0]) != 0 {
                continue;
            }
            m.sb(record + 0x3d, 0xff);
            m.sb(record + 0x3c, 0xff);
            m.sb(record + 0x63, 0xff);
            m.sb(record + 0x62, 0xff);
        }
        let gameplay = m.gw(common::GAMEPLAY);
        if m.lw(gameplay + 4) & 0xe != 0 {
            update_groups_4a2c4(m);
        }
        mark_formations_4a6e4(m);
    });
}

/// 8004A2C4: run 8004A334 for every active group (`[gp+0x370]`, 12 bytes,
/// count `gp+0x35A`).
pub fn update_groups_4a2c4(m: &mut Machine) {
    with_frame(m, 0x20, |m, _| {
        let mut group = 0u32;
        while lt(group, lbu(m, GP + 0x35a)) {
            let groups = m.gw(0x370);
            if m.lw(groups.wrapping_add(group * 0xc)) & 1 != 0 {
                original(m, 0x8004_a334, [group, 0, 0, 0]);
            }
            group += 1;
        }
    });
}

/// 8004A6E4: for each active formation (`[gp+0x394]` words, members at
/// `[gp+0x388]` + 0x14 each, count `gp+0x36D`) set slot flags 0x200000 on
/// member +2 and 0x400000 on member +3.
pub fn mark_formations_4a6e4(m: &mut Machine) {
    let count = lbu(m, GP + 0x36d);
    if count == 0 {
        return;
    }
    let members = m.gw(0x388);
    let active = m.gw(0x394);
    for index in 0..count {
        if m.lw(active.wrapping_add(index << 2)) & 1 == 0 {
            continue;
        }
        let member = members.wrapping_add(index * 0x14);
        let leader = SLOT_FLAGS.wrapping_add(lbu(m, member + 2) << 2);
        set_word_bits(m, leader, 0x20_0000);
        let wing = SLOT_FLAGS.wrapping_add(lbu(m, member + 3) << 2);
        set_word_bits(m, wing, 0x40_0000);
    }
}

/// 80045DA8: choose each present slot's next movement state (+0x3D) and
/// lock target (+0x60).
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
                    original(m, 0x8004_f7c8, [slot, 0, 0, 0]);
                    continue;
                }
                6 => {
                    m.sw(flags_at, flags & 0xffef_ffff);
                    original(m, 0x8004_f9f0, [slot, 0, 0, 0]);
                    continue;
                }
                7 => {
                    original(m, 0x8004_fce4, [slot, 0, 0, 0]);
                    continue;
                }
                8 => {
                    original(m, 0x8004_fdb0, [slot, 0, 0, 0]);
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
            if original(m, 0x8004_6aa8, [slot, 0, 0, 0]) != 0 {
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
                    if m.lw(context + 0x398) != 3 {
                        m.sb(record + 0x3d, 5);
                        let target = shared::release_lock(m, slot);
                        m.sb(record + 0x60, target as u8);
                        continue;
                    }
                }
            }
            let routine = match lbu(m, record + 0x1c).wrapping_sub(1) {
                1 | 3 => 0x8004_f2b0,
                8 | 9 => 0x8004_f594,
                _ => 0x8004_f100,
            };
            original(m, routine, [slot, 0, 0, 0]);
            let target = lbu(m, record + 0x60);
            if target == 0xff || lbu(m, record + 0x1c).wrapping_sub(9) < 2 {
                continue;
            }
            let table = m.gw(0x374);
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

/// Movement-state routines of 800461B8 (jump table 80023A78) other than
/// the translated states 0 (8004DECC), 1 (8004D794), 4 (8004DA2C) and 10
/// (8004E504).
fn movement_routine(state: u32) -> Option<u32> {
    Some(match state {
        2 => 0x8004_d860,
        3 => 0x8004_d978,
        5 => 0x8004_dac0,
        7 => 0x8004_df64,
        11 => 0x8004_e580,
        12 => 0x8004_e5e0,
        14 => 0x8004_e6d8,
        15 => 0x8004_e720,
        16 => 0x8004_e784,
        17 => 0x8004_e970,
        18 => 0x8004_ead4,
        19 => 0x8004_eb1c,
        20 => 0x8004_eb6c,
        21 => 0x8004_ecbc,
        22 => 0x8004_ef14,
        _ => return None,
    })
}

/// 800461B8: switch each present slot to its requested movement state and
/// run the state routine; a request of 0xFF ends it (record flag 0x200000).
pub fn run_movement_461b8(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        for slot in 2..32u32 {
            let entity = entity(m, slot);
            let record = slot_record(m, slot);
            if !alive(m, entity) || lbu(m, record + 0x1c) == 6 {
                continue;
            }
            let next = lbu(m, record + 0x3d);
            if lbu(m, record + 0x3c) != next {
                m.sb(record + 0x3c, next as u8);
                m.sb(record + 0x3e, 0);
            }
            match lbu(m, record + 0x3c) {
                0 => patrol_4decc(m, slot),
                1 => pursue_4d794(m, slot),
                4 => escort_4da2c(m, slot),
                10 => hold_4e504(m, slot),
                state => {
                    if let Some(routine) = movement_routine(state) {
                        original(m, routine, [slot, 0, 0, 0]);
                    }
                }
            }
            let next = lbu(m, record + 0x3d);
            if next == 0xff {
                let word = m.lw(record);
                m.sb(record + 0x3c, 0xff);
                m.sw(record, word | 0x20_0000);
            }
        }
    });
}

/// Action routines of 800463D8 (jump table 80023AD8).
fn action_routine(m: &mut Machine, slot: u32, record: u32, action: u32) -> Option<u32> {
    let kind = |m: &mut Machine| {
        let info = m.gw(0x374).wrapping_add(slot << 6);
        let pointer = m.lw(info + 0x14);
        (info, m.lw(pointer))
    };
    Some(match action {
        0 => 0x8004_86f0,
        4 => {
            let (info, kind) = kind(m);
            if kind == 1 {
                0x8004_83c0
            } else {
                let special = lbu(m, record + 0x3c) == 3 || {
                    let pointer = m.lw(info + 0x18);
                    lbu(m, pointer) != 0
                };
                if special && !matches!(lbu(m, MISSION), 0x20 | 0x2f | 0x35) {
                    0x8004_7ed4
                } else {
                    0x8004_7f34
                }
            }
        }
        5 => match kind(m).1 {
            1 => 0x8004_83c0,
            3 => 0x8004_8514,
            _ => 0x8004_82e4,
        },
        6 => 0x8004_83c0,
        7 => 0x8004_8134,
        8 => 0x8004_878c,
        9 => 0x8004_8800,
        10 => 0x8004_8ba0,
        11 => 0x8004_8c5c,
        12 => 0x8004_8f40,
        13 => 0x8004_8d3c,
        14 => 0x8004_8e34,
        15..=17 => 0x8004_90ac,
        25 => 0x8004_9300,
        26 => 0x8004_9554,
        27 => 0x8004_965c,
        30 => 0x8004_9870,
        31 => 0x8004_99cc,
        40 => 0x8004_9bc4,
        41 => 0x8004_9c84,
        42 => 0x8004_9d94,
        43 => 0x8004_9f9c,
        50 => 0x8004_88e0,
        51 => 0x8004_8988,
        52 => 0x8004_a144,
        _ => return None,
    })
}

/// Reset an entity's angular rates (+0x30/+0x32/+0x34).
fn stop_turning(m: &mut Machine, entity: u32) {
    m.sh(entity + 0x30, 0);
    m.sh(entity + 0x32, 0);
    m.sh(entity + 0x34, 0);
}

/// 800463D8: switch each present slot to its requested action (+0x63 into
/// +0x62), run the action routine and count its frames (+0x66); then the
/// per-slot passes 80043F24 and 800440E0.
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

/// 80047598: find the two nearest trackable slots (object flag 0x10000)
/// to the player, assign them to the tracked-sound channels, copy the
/// tracked entities' position and angles into channel records 0 and 1, and
/// update the channel sounds.
pub fn nearest_targets_47598(m: &mut Machine) {
    with_frame(m, 0x40, |m, frame| {
        let mut first = u32::MAX;
        let mut second = u32::MAX;
        let mut best = 0x8000u32;
        let mut next = 0x8000u32;
        for slot in 2..32u32 {
            let flags = m.lw(SLOT_FLAGS + (slot << 2));
            if flags & 1 == 0 || flags & 0x40_0000 != 0 {
                continue;
            }
            let object = m.lw(SLOT_OBJECTS + (slot << 2));
            let distance = if m.lw(object + 0x20) & 0x1_0000 != 0 {
                let gameplay = m.gw(common::GAMEPLAY);
                let player = m.lw(gameplay + 0x28).wrapping_add(4);
                let target = entity(m, slot) + 8;
                shared::f_800827b4(m, [player, target, 0, 0])
            } else {
                0x8000
            };
            if lt(distance, best) {
                next = best;
                best = distance;
                second = first;
                first = slot;
            } else if lt(distance, next) {
                next = distance;
                second = slot;
            }
            if best == 0x8000 {
                first = u32::MAX;
            }
            if next == 0x8000 {
                second = u32::MAX;
            }
        }
        m.sw(frame + 0x10, first);
        m.sw(frame + 0x14, second);
        m.sw(frame + 0x20, best);
        m.sw(frame + 0x24, next);
        assign_tracks_2f354(m, frame + 0x10, frame + 0x18);
        for (index_at, out) in [(8, 0xc), (0x64, 0x68)] {
            let index = m.lw(TRACK_RECORDS + index_at);
            if index == u32::MAX {
                continue;
            }
            let source = entity(m, index);
            let head: [u32; 4] = std::array::from_fn(|i| m.lw(source + 8 + i as u32 * 4));
            for (i, word) in head.into_iter().enumerate() {
                m.sw(TRACK_RECORDS + out + i as u32 * 4, word);
            }
            let tail: [u32; 2] = std::array::from_fn(|i| m.lw(source + 0x18 + i as u32 * 4));
            for (i, word) in tail.into_iter().enumerate() {
                m.sw(TRACK_RECORDS + out + 0x10 + i as u32 * 4, word);
            }
        }
        let gameplay = m.gw(common::GAMEPLAY);
        let listener = m.lw(gameplay + 0x28).wrapping_add(4);
        update_track_sounds_2f590(m, listener);
    });
}

/// 8002F354: match the tracked-sound channels (`[0x800C43D8]` records,
/// current slot at +8) to the wanted slots `indices[]`: channels already
/// following a wanted slot keep it, the others take the remaining slots
/// (8002F2F0 starts a sound, 8002E418 stops one for -1). `out[i]` receives
/// the channel serving `indices[i]`.
pub fn assign_tracks_2f354(m: &mut Machine, indices: u32, out: u32) {
    with_frame(m, 0x38, |m, _| {
        let mut used_channels = 0u32;
        let mut used_indices = 0u32;
        let free = |mask: u32, index: u32| ((mask & 0xff) >> (index & 31)) & 1 == 0;
        let mut channel = 0u32;
        while lt(channel, m.lw(TRACKS)) {
            if free(used_channels, channel) && lt(0, m.lw(TRACKS)) {
                let record = TRACK_RECORDS.wrapping_add(channel.wrapping_mul(0x5c));
                let mut index = 0u32;
                loop {
                    if free(used_indices, index)
                        && m.lw(record + 8) == m.lw(indices.wrapping_add(index << 2))
                    {
                        m.sw(out.wrapping_add(index << 2), channel);
                        used_channels |= bit(channel);
                        used_indices |= bit(index);
                        break;
                    }
                    index += 1;
                    if !lt(index, m.lw(TRACKS)) {
                        break;
                    }
                }
            }
            channel += 1;
        }
        let mut channel = 0u32;
        while lt(channel, m.lw(TRACKS)) {
            let record = TRACK_RECORDS.wrapping_add(channel.wrapping_mul(0x5c));
            if free(used_channels, channel) && lt(0, m.lw(TRACKS)) {
                let mut index = 0u32;
                loop {
                    if free(used_indices, index) {
                        let wanted_at = indices.wrapping_add(index << 2);
                        let wanted = m.lw(wanted_at);
                        m.sw(record + 8, wanted);
                        if m.lw(wanted_at) == u32::MAX {
                            set_word_bits(m, record, 2);
                            original(m, 0x8002_e418, [record + 0x24, wanted_at, record, 0]);
                        } else {
                            set_word_bits(m, record, 1);
                            let wanted = m.lw(wanted_at);
                            original(m, 0x8002_f2f0, [channel, wanted, record, 0]);
                        }
                        m.sw(out.wrapping_add(index << 2), channel);
                        used_channels |= bit(channel);
                        used_indices |= bit(index);
                        break;
                    }
                    index += 1;
                    if !lt(index, m.lw(TRACKS)) {
                        break;
                    }
                }
            }
            channel += 1;
        }
    });
}

/// `x / 100` as compiled (magic 0x51EB851F, shift 5).
fn div100(value: u32) -> u32 {
    sra(hi(value, 0x51eb_851f), 5).wrapping_sub(sra(value, 31))
}
/// `x / 80` as compiled (magic 0x66666667, shift 5).
fn div80(value: u32) -> u32 {
    sra(hi(value, 0x6666_6667), 5).wrapping_sub(sra(value, 31))
}

/// 8002F590: per tracked-sound channel with three voices: start (flag 1),
/// stop (flag 2 or no entity), else position the sound relative to
/// `listener` and set its voice volumes.
pub fn update_track_sounds_2f590(m: &mut Machine, listener: u32) {
    with_frame(m, 0x58, |m, frame| {
        let mut index = 0u32;
        while lt(index, m.lw(TRACKS)) {
            let record = TRACK_RECORDS.wrapping_add(index.wrapping_mul(0x5c));
            let kind = m.lw(record + 4);
            let params = kind.wrapping_mul(20).wrapping_add(0x800a_ed20);
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
            original(m, 0x8002_d854, [program, voice, 0, 0]);
        }
        for voice in voices {
            let at = VOICES.wrapping_add(voice * 20);
            let pitch = lh(m, at + 0xe);
            let pitch = original(m, 0x8002_e230, [pitch, voice << 4, 0, 0]);
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
        original(m, 0x8002_e8ec, args);
        original(m, 0x8002_ea60, [near, first, 0, 0]);
        let args = [near, lbu(m, params + 8), lbu(m, params + 0xc), 0];
        original(m, 0x8002_ea6c, args);
        let args = [near, lbu(m, params + 9), lbu(m, params + 0xe), 0];
        original(m, 0x8002_ea8c, args);
        original(m, 0x8002_ea60, [far, last, 0, 0]);
        let args = [far, lbu(m, params + 0xa), lbu(m, params + 0x10), 0];
        original(m, 0x8002_ea6c, args);
        let args = [far, lbu(m, params + 0xb), lbu(m, params + 0x12), 0];
        original(m, 0x8002_ea8c, args);
    } else if word & 2 != 0 {
        m.sw(record, word & !2);
        silence(m);
        return;
    }
    if m.lw(record + 8) == u32::MAX {
        silence(m);
        return;
    }
    original(m, 0x8002_e2c0, [work, listener, position, 0]);
    shared::f_8002daa0(m, [last, 0x1000, 0x1000, 0]);
    original(m, 0x8002_eaac, [far, 0, 0, 0]);
    original(m, 0x8002_e430, [last, work, 4, 0]);
    original(m, 0x8002_e4c0, [last, work, 0, 0]);
    original(m, 0x8002_e260, [first, 0, 0, 0]);
    original(m, 0x8002_eaac, [near, 0, 0, 0]);
    original(m, 0x8002_e430, [first, work, 2, 0]);
    original(m, 0x8002_e4c0, [first, work, 0, 0]);
    original(m, 0x8002_e82c, [first, work, 0, 0]);
    original(m, 0x8002_e658, [first, middle, work, 0]);
    let mut range = 0x200;
    let wide = |m: &mut Machine| {
        let gameplay = m.gw(common::GAMEPLAY);
        m.lw(gameplay + 4) & 0x20 != 0
    };
    let mut scaled = true;
    if lt(m.lw(position + 4), 0xff40_0000) {
        original(m, 0x8002_e910, [work, shape, 0, 0]);
        if lbu(m, shape) == 1 {
            let voice = lbu(m, shape + 3);
            original(m, 0x8002_e260, [voice, 0, 0, 0]);
            let voice = lbu(m, shape + 3);
            original(m, 0x8002_e4c0, [voice, work, 0, 0]);
            let voice = lbu(m, shape + 3);
            original(m, 0x8002_e82c, [voice, work, 0, 0]);
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
            let left = lh(m, at + 4).wrapping_mul(60);
            let right = lh(m, at + 6).wrapping_mul(60);
            m.sh(at + 4, div100(left) as u16);
            m.sh(at + 6, div100(right) as u16);
        }
        range = 0x800;
        let context = m.gw(common::CONTEXT);
        if m.lw(context + 0xf7c) == 4 && lhu(m, 0x800b_ade4) < 0x1e {
            let fade = lhu(m, 0x800b_ade4);
            for voice in voices {
                let at = VOICES.wrapping_add(voice * 20);
                let left = lo(lh(m, at + 4), fade);
                let right = lo(lh(m, at + 6), fade);
                m.sh(at + 4, div80(left) as u16);
                m.sh(at + 6, div80(right) as u16);
            }
        } else if m.lw(context + 0xf7c) == 10 {
            for voice in voices {
                let at = VOICES.wrapping_add(voice * 20);
                m.sh(at + 4, 0);
                m.sh(at + 6, 0);
            }
        }
    }
    for (i, voice) in voices.into_iter().enumerate() {
        let source = VOICES.wrapping_add(voice * 20) + 4;
        original(
            m,
            0x8002_fe9c,
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

/// 8003809C: per-entity motion requests (+0x54 flags: angle and velocity
/// updates through 80030D20/80030CB4, bit 31 integrates the velocity), then
/// mission-specific entity effects.
pub fn integrate_entities_3809c(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        for slot in 2..32u32 {
            let entity = entity(m, slot);
            if !alive(m, entity) {
                continue;
            }
            if m.lw(entity + 0x54) & 0x80_0000 != 0 {
                original(m, 0x8003_0d20, [entity + 0x18, entity + 0x30, 0, 0]);
            }
            if m.lw(entity + 0x54) & 0x40_0000 != 0 {
                original(
                    m,
                    0x8003_0cb4,
                    [entity + 0x20, entity + 0x30, entity + 0x38, 0],
                );
            }
            if m.lw(entity + 0x54) & 0x20_0000 != 0 {
                original(
                    m,
                    0x8003_0cb4,
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
            original(m, 0x8004_6e0c, [0; 4]);
        }
        let mission = lbu(m, MISSION);
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
                    let view = m.gw(0x32c);
                    let height = m.lw(view + 4).wrapping_neg();
                    proximity_effect_81838(m, entity + 8, entity + 0x38, height);
                }
            }
        }
        let player = m.gw(0x330);
        if matches!(mission, 0x11 | 0x1f | 0x26 | 0x2e) && lbu(m, player + 0x3c) >= 3 {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity)
                    && lh(m, entity + 0x48) == 0x3f
                    && m.lw(SLOT_FLAGS + (slot << 2)) & 0x20_0000 != 0
                {
                    let gameplay = m.gw(common::GAMEPLAY);
                    let seed = m.lw(gameplay + 0xc).wrapping_add(slot);
                    original(m, 0x8008_05e8, [entity + 8, entity + 0x38, seed, 3]);
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
                if lh(m, entity + 0x48) == 0x14 {
                    burning_effect(m, slot, entity, mission);
                }
                if lh(m, entity + 0x48) == 0x15 && matches!(mission, 0x21 | 0x2e | 0x35) {
                    let points = lh(m, entity + 0x4e);
                    if lt(points, 0xc9) && lt(0, points) {
                        let gameplay = m.gw(common::GAMEPLAY);
                        let seed = m.lw(gameplay + 0xc).wrapping_add(slot);
                        original(m, 0x8008_05e8, [entity + 8, entity + 0x38, seed, 2]);
                        let flags = lbu(m, entity + 5) & 0xf7;
                        m.sb(entity + 5, flags as u8);
                    }
                }
            }
        }
        if mission == 0x22 || mission == 0x36 {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity) && lh(m, entity + 0x48) == 0x13 && lbu(m, entity + 5) & 8 != 0 {
                    original(m, 0x8008_1374, [entity + 8, entity + 0x38, 3, 0]);
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
                if flags & 1 != 0 && lh(m, entity + 0x48) == 0x3c && flags & 0x20_0000 != 0 {
                    original(m, 0x8008_16f0, [entity + 8, entity + 0x38, 0, 0]);
                }
            }
        }
        if mission == 0x3a {
            for slot in 2..32u32 {
                let entity = entity(m, slot);
                if alive(m, entity) {
                    original(m, 0x8008_1374, [entity + 8, entity + 0x38, 4, 0]);
                }
            }
        }
    });
}

/// 8003809C, entity kind 0x14: damaged-structure smoke and sparks.
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
        original(m, 0x8008_05e8, [entity + 8, entity + 0x38, seed, 2]);
    } else {
        let player = m.gw(0x330);
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
        original(m, 0x8008_1374, [entity + 8, entity + 0x38, 2, 0]);
    }
    let flags = lbu(m, entity + 5) & 0xf7;
    m.sb(entity + 5, flags as u8);
}

// ==== 8006ADE4: projectiles and particles =========================================

/// 8006ADE4: spawn requested particles, particle collisions, projectiles,
/// hit timers.
pub fn update_shots_6ade4(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        spawn_particles_69940(m);
        update_particles_69d34(m);
        update_projectiles_69750(m);
        apply_hit_timers_66d04(m);
    });
}

/// Particle record `index` (0x3C bytes from block +0x15D0).
fn particle(m: &mut Machine, index: u32) -> u32 {
    let block = m.gw(GP_SHOTS);
    block.wrapping_add(index.wrapping_mul(60).wrapping_add(0x15d0))
}

/// 80069940: spawn the particles requested this frame (block +0x154C
/// requests of 0x10 bytes from +0x1550): type parameters, launch velocity
/// rotated by the source's angles, random spread.
pub fn spawn_particles_69940(m: &mut Machine) {
    with_frame(m, 0xa0, |m, frame| {
        let requests = m.gw(GP_SHOTS) + 0x1550;
        let mut request = 0u32;
        loop {
            let block = m.gw(GP_SHOTS);
            if !lt(request, m.lw(block + 0x154c)) {
                break;
            }
            let entry = requests + request * 0x10;
            let index = lh(m, entry);
            let source = m.lw(entry + 8);
            let record = block.wrapping_add(index.wrapping_mul(60).wrapping_add(0x15d0));
            let colour = m.lw(entry + 4);
            m.sw(record + 0x38, colour);
            let angles = [
                lh(m, source + 0x10),
                lh(m, source + 0x12),
                lh(m, source + 0x14),
            ];
            m.sh(frame + 0x50, angles[0] as u16);
            m.sh(frame + 0x52, angles[1] as u16);
            m.sh(frame + 0x54, angles[2] as u16);
            shared::f_80030534(m, [frame + 0x50, frame + 0x30, 0, 0]);
            sdk::set_rot_matrix(m, frame + 0x30);
            let kind = lh(m, entry + 2);
            let full = |m: &mut Machine, speed: u32, spread: u32| {
                m.sh(frame + 0x54, speed as u16);
                m.sh(frame + 0x50, 0);
                m.sh(frame + 0x52, 0);
                m.sh(frame + 0x58, 0);
                m.sh(frame + 0x5a, 0);
                m.sh(frame + 0x5c, spread as u16);
            };
            let flat = |m: &mut Machine, speed: u32| {
                m.sh(frame + 0x50, 0);
                m.sh(frame + 0x52, 0);
                m.sh(frame + 0x54, speed as u16);
                m.sh(frame + 0x58, 0);
                m.sh(frame + 0x5a, 0);
                m.sh(frame + 0x5c, 0);
            };
            match kind {
                0 => {
                    m.sw(record, 0x2_0410);
                    m.sw(record + 4, 0x25);
                    full(m, 0x1580, 0x800);
                }
                1 => {
                    m.sw(record, 0x2_0820);
                    m.sw(record + 4, 0x18);
                    full(m, 0x19c0, 0x990);
                }
                2 => {
                    m.sw(record, 0x4_1040);
                    m.sw(record + 4, 0x3e);
                    full(m, 0x1020, 0x4c0);
                }
                3 => {
                    m.sw(record, 0x8_2080);
                    m.sw(record + 4, 0x23);
                    flat(m, 0x1120);
                }
                4 => {
                    m.sw(record, 0x9_4103);
                    m.sw(record + 4, 0x10);
                    flat(m, 0x35c0);
                }
                5 => {
                    m.sw(record, 0x9_8202);
                    m.sw(record + 4, 0x10);
                    full(m, 0x2b00, 0x800);
                }
                _ => {}
            }
            load_v0(m, frame + 0x50);
            rtv0(m);
            store_mac(m, frame + 0x10);
            load_v0(m, frame + 0x58);
            rtv0(m);
            store_mac(m, frame + 0x20);
            let (mut dx, mut dy, mut dz) = (0u32, 0u32, 0u32);
            if m.lw(record) & 0x1_0000 == 0 {
                dx = sra(0x4000u32.wrapping_sub(sdk::rand(m)), 1);
                dy = sra(0x4000u32.wrapping_sub(sdk::rand(m)), 1);
                dz = sra(0x4000u32.wrapping_sub(sdk::rand(m)), 1);
            }
            if m.lw(record) & 2 == 0 {
                dy = dy.wrapping_sub(0x3332);
            }
            let base = m.lw(entry + 0xc);
            let vx = (m.lw(frame + 0x10) << 7)
                .wrapping_add(m.lw(base))
                .wrapping_add(dx);
            m.sw(record + 0x28, vx);
            let base = m.lw(entry + 0xc);
            let vy = (m.lw(frame + 0x14) << 7)
                .wrapping_add(m.lw(base + 4))
                .wrapping_add(dy);
            m.sw(record + 0x2c, vy);
            let base = m.lw(entry + 0xc);
            let vz = (m.lw(frame + 0x18) << 7)
                .wrapping_add(m.lw(base + 8))
                .wrapping_add(dz);
            m.sw(record + 0x30, vz);
            let x = m.lw(source).wrapping_sub(m.lw(record + 0x28));
            let y = m.lw(source + 4).wrapping_sub(m.lw(record + 0x2c));
            m.sw(record + 8, x);
            let z = m.lw(source + 8).wrapping_sub(vz);
            m.sw(record + 0xc, y);
            m.sw(record + 0x10, z);
            let tail = x.wrapping_add(m.lw(frame + 0x20) << 8);
            m.sw(record + 0x18, tail);
            let flags = m.lw(record + 0x38);
            let tail = y.wrapping_add(m.lw(frame + 0x24) << 8);
            m.sw(record + 0x1c, tail);
            let tail = z.wrapping_add(m.lw(frame + 0x28) << 8);
            m.sw(record + 0x20, tail);
            if flags & 0x2000 != 0 {
                m.sw(frame + 0x60, x);
                let value = m.lw(record + 0xc);
                m.sw(frame + 0x64, value);
                let value = m.lw(record + 0x10);
                m.sw(frame + 0x68, value);
                for offset in [0, 2, 4] {
                    let value = m.lhu(source + 0x10 + offset);
                    m.sh(frame + 0x70 + offset, value);
                }
                let index = lh(m, entry);
                let kind = lh(m, entry + 2);
                write_particle_event_7d248(m, index, frame + 0x60, record + 0x28, kind);
            }
            request += 1;
        }
        let block = m.gw(GP_SHOTS);
        m.sw(block + 0x154c, 0);
    });
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
        original(m, 0x8009_8b80, [from, target, hit, SCRATCH])
    } else {
        original(m, 0x8009_8db0, [from, target, hit, SCRATCH])
    };
    (!lt(0x7fff_fffe, result)).then_some(result)
}

/// The sound selection of the particle hits: a0 of 8002F044 and a2.
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

/// The impact sound of a terrain or bounds hit (8002F044 a0/a2).
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

/// 80069D34: particle pass: entity hits (damage flags 0x54/0x50, sounds,
/// 8007EA98), detailed hits through 800999FC, world bounds and terrain,
/// then motion with gravity/drag (GTE GPF, IR0 = 0xFA0) and expiry.
pub fn update_particles_69d34(m: &mut Machine) {
    with_frame(m, 0x2d0, |m, frame| {
        let radii = frame + 0x18;
        let hits = frame + 0x220;
        let masks = frame + 0x280;
        let targets = collect_targets(m, radii);
        let mut hit_count = 0u32;
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
                    original(m, 0x8002_f044, [sound, record + 8, mode, dx]);
                    let variant = effect_variant(m.lw(record));
                    original(m, 0x8007_ea98, [record + 8, variant, 0, 0]);
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
                original(m, 0x8009_99fc, [record + 8, record + 0x28, struck, point])
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
            original(m, 0x8002_f044, [sound, point, mode, 0]);
            let variant = effect_variant(m.lw(record));
            let face = m.lw(LAST_FACE);
            original(m, 0x8007_ee70, [point, face, variant, 0]);
        }
        bounds_and_terrain_particles(m, frame);
        integrate_particles(m, frame);
    });
}

/// 80069D34, third pass: world bounds (gameplay +0x2A0) and terrain.
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
                original(m, 0x8002_f044, [sound, point, mode, new[2]]);
            } else {
                let target = frame + 0x260;
                if terrain_hit(m, terrain, heights, record + 8, old, new, target, point).is_none() {
                    continue;
                }
                let word = m.lw(record);
                m.sw(record + 4, 0);
                let (sound, mode) = impact_sound(m, word);
                original(m, 0x8002_f044, [sound, point, mode, 0]);
            }
            let variant = effect_variant(m.lw(record));
            let face = m.lw(LAST_FACE);
            original(m, 0x8007_ee70, [point, face, variant, 0]);
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

/// 80069D34, fourth and fifth passes: motion, drag/gravity and expiry.
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

/// 80069750: projectile pass: collisions, motion, behaviours, lifetimes.
pub fn update_projectiles_69750(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        projectile_collisions_6894c(m);
        move_projectiles_693e8(m);
        run_projectiles_687f8(m);
        age_projectiles_69464(m);
    });
}

/// 8006894C: projectile collisions (records of 0xA4 bytes from block +0xC8,
/// active mask +0x1548): entity hits, detailed hits (800999FC), world
/// bounds and terrain bounces.
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
                original(
                    m,
                    0x8009_99fc,
                    [record + 0x1c, record + 0x38, struck, point],
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

/// 8006894C, third pass: world bounds and terrain for projectiles; a hit
/// stores the surface normal (80024088 table or the terrain normal at
/// `[0x800BAF48]`) and bounces (state 2).
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
            let normal = 0x8002_4088 + axis * 8;
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
        let normal = m.lw(0x800b_af48);
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

/// Bounds hit tail of 8006894C (velocity x/y kept at +0x8C/+0x90).
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

/// 800693E8: projectile motion: position (+0x1C) += velocity (+0x38).
pub fn move_projectiles_693e8(m: &mut Machine) {
    let block = m.gw(GP_SHOTS);
    let active = m.lw(block + 0x1548);
    if active == 0 {
        return;
    }
    for index in 0..32u32 {
        if active & bit(index) == 0 {
            continue;
        }
        let record = block + 0xc8 + index * 0xa4;
        let x = m.lw(record + 0x1c);
        let vx = m.lw(record + 0x38);
        let vy = m.lw(record + 0x3c);
        let vz = m.lw(record + 0x40);
        m.sw(record + 0x1c, x.wrapping_add(vx));
        let y = m.lw(record + 0x20);
        let z = m.lw(record + 0x24);
        m.sw(record + 0x20, y.wrapping_add(vy));
        m.sw(record + 0x24, z.wrapping_add(vz));
    }
}

/// Projectile behaviour tables of 800687F8 by class (+4): class 0 by +5
/// (800B09F0), class 1 by +6 (800B0A04), class 2 by +7 (800B0A18).
const BEHAVIOUR_TABLES: [(u32, u32); 3] = [(5, 0x800b_09f0), (6, 0x800b_0a04), (7, 0x800b_0a18)];

/// 800687F8: run each active projectile's behaviour routine (through its
/// class table) and count its age (+0xA).
pub fn run_projectiles_687f8(m: &mut Machine) {
    with_frame(m, 0x38, |m, _| {
        let block = m.gw(GP_SHOTS);
        let active = m.lw(block + 0x1548);
        if active == 0 {
            return;
        }
        for index in 0..32u32 {
            if active & bit(index) == 0 {
                continue;
            }
            let record = block + 0xc8 + index * 0xa4;
            let class = lbu(m, record + 4);
            let routine = match BEHAVIOUR_TABLES.get(class as usize) {
                Some(&(selector, table)) => {
                    let kind = lbu(m, record + selector);
                    m.lw(table.wrapping_add(kind << 2))
                }
                None => 0x8006_87f0,
            };
            dispatch::call(m, routine, [record, 0, 0, 0]);
            let age = m.lhu(record + 0xa).wrapping_add(1);
            m.sh(record + 0xa, age);
        }
    });
}

/// 800687F0: the empty projectile behaviour.
pub fn idle_projectile_687f0(_m: &mut Machine) {}

/// 80069464: projectile flags and lifetimes: spin (+0x30 += +0x5C), angle
/// and velocity requests (0x1000/0x4000), smoke trails (0x40/0x80), then
/// age +0x18; on expiry the end effects, owner counters and target marks.
pub fn age_projectiles_69464(m: &mut Machine) {
    with_frame(m, 0x38, |m, _| {
        let block = m.gw(GP_SHOTS);
        let mut active = m.lw(block + 0x1548);
        if active == 0 {
            return;
        }
        for index in 0..32u32 {
            let own = bit(index);
            if !active & own != 0 {
                continue;
            }
            let record = block + 0xc8 + index * 0xa4;
            let mut word = m.lw(record);
            if word & 0x2000 != 0 {
                let spin = lhu(m, record + 0x30);
                let rate = lhu(m, record + 0x5c);
                word &= !0x2000;
                m.sh(record + 0x30, spin.wrapping_add(rate) as u16);
            }
            if word & 0x1000 != 0 {
                original(m, 0x8003_0d20, [record + 0x2c, record + 0x58, 0, 0]);
                word &= !0x1000;
            }
            if word & 0x4000 != 0 {
                original(
                    m,
                    0x8003_0cb4,
                    [record + 0x48, record + 0x2c, record + 0x38, 0],
                );
                word &= !0x4000;
            }
            if word & 0x40 != 0 {
                original(m, 0x8008_1374, [record + 0x1c, record + 0x38, 0, 0]);
            }
            if word & 0x80 != 0 {
                original(m, 0x8008_1374, [record + 0x1c, record + 0x38, 1, 0]);
            }
            m.sw(record, word);
            let life = lhu(m, record + 0x18).wrapping_sub(1);
            m.sh(record + 0x18, life as u16);
            if !lt(sext16(life), 0) {
                if m.lw(record + 0x6c) & 0x2000 != 0 {
                    let angle = lh(m, record + 0x34);
                    write_projectile_event_7d2b4(m, index, angle, record + 0x1c);
                }
                continue;
            }
            if m.lw(record) & 0x10 != 0 {
                let argument = m.lw(record + 0x9c);
                original(m, 0x8006_91a8, [record, argument, 0, 0]);
            }
            if m.lw(record) & 0x20 != 0 {
                original(m, 0x8006_932c, [record, 0, 0, 0]);
            }
            if m.lw(record + 0x6c) & 0x2000 != 0 {
                write_projectile_end_7d2ec(m, index);
            }
            let owner = m.lw(record + 0x6c) & 0x3f;
            let counters = m.gw(GP_SHOTS).wrapping_add(owner);
            let count = lbu(m, counters + 0x48).wrapping_sub(1);
            m.sb(counters + 0x48, count as u8);
            let target = m.lw(record + 0x70);
            active &= !own;
            if target == u32::MAX {
                continue;
            }
            let counters = m.gw(GP_SHOTS).wrapping_add(target & 0x3f);
            let count = lbu(m, counters + 0x88).wrapping_sub(1);
            m.sb(counters + 0x88, count as u8);
            if count & 0xff != 0 {
                continue;
            }
            let block = m.gw(GP_SHOTS);
            let mask_at = if target & 0x20 != 0 { block + 4 } else { block };
            clear_word_bits(m, mask_at, bit(target & 0x1f));
            if m.lw(record) & 2 != 0 {
                continue;
            }
            let victim = entity(m, target & 0x3f);
            let flags = m.lw(victim + 0x54);
            if flags & 0x7fc0 != 0 {
                continue;
            }
            m.sw(victim + 0x54, flags | 0x8000);
            let colour = m.lw(record + 0x6c);
            m.sw(victim + 0x50, colour);
        }
        let block = m.gw(GP_SHOTS);
        m.sw(block + 0x1548, active);
    });
}

/// 80066D04: delayed hit timers (block +0x33E0 mask, 0xC-byte records from
/// +0x33E4): on expiry apply the damage flags to the target entity unless
/// it is excluded by the hit's side mask.
pub fn apply_hit_timers_66d04(m: &mut Machine) {
    let block = m.gw(GP_SHOTS);
    let mut active = m.lw(block + 0x33e0);
    if active == 0 {
        return;
    }
    let table = m.gw(common::ENTITIES);
    for index in 0..32u32 {
        if !active & bit(index) != 0 {
            continue;
        }
        let record = block + 0x33e4 + index * 0xc;
        let timer = lhu(m, record).wrapping_sub(1);
        m.sh(record, timer as u16);
        if !lt(sext16(timer), 0) {
            continue;
        }
        let index_word = m.lw(record + 4);
        let target = m.lw(table.wrapping_add((index_word & 0x3f) << 2));
        let word = m.lw(target);
        active &= !bit(index);
        if word & 0x4200 != 0x4200 || word & m.lw(record + 8) & 0x1c0 != 0 {
            continue;
        }
        let damage = lhu(m, record + 2);
        set_word_bits(m, target + 0x54, damage);
        let colour = m.lw(record + 8);
        m.sw(target + 0x50, colour);
    }
    let block = m.gw(GP_SHOTS);
    m.sw(block + 0x33e0, active);
}

// ==== 8007C44C: trails ===============================================================

/// 8007C44C: missile and entity trails.
pub fn update_trails_7c44c(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        missile_trails_7b36c(m);
        advance_trail_queues_7a8b4(m);
        entity_trails_7c164(m);
        advance_entity_trail_queues_7b834(m);
    });
}

/// The trail handle byte of `slot` at `[gp+0x544] + offset + slot`.
fn trail_handle(m: &mut Machine, offset: u32, slot: u32) -> u32 {
    m.gw(GP_TRAILS).wrapping_add(offset + slot)
}

/// 8007B36C: smoke trail points of the active missiles (flag +0x35 bit 3):
/// four model points (800B1E94, XY only) rotated by the missile angles
/// into scratch +0x20..+0x5F, then a new trail (8007A60C) or the next
/// segment (8007A7C0) on the missile's frame phase.
pub fn missile_trails_7b36c(m: &mut Machine) {
    with_frame(m, 0x30, |m, _| {
        let block = m.gw(GP_SHOTS);
        let active = m.lw(block + 0x1548);
        for slot in 0..32u32 {
            let record = block + 0xe4 + slot * 0xa4;
            if active & bit(slot) == 0 || lbu(m, record + 0x19) & 8 == 0 {
                let handle = trail_handle(m, 0x1_2104, slot);
                m.sb(handle, 0xff);
                continue;
            }
            shared::f_80030534(m, [record + 0x10, SCRATCH, 0, 0]);
            sdk::set_rot_matrix(m, SCRATCH);
            let origin = [m.lw(record), m.lw(record + 4), m.lw(record + 8)];
            let mut last = [0u32; 3];
            for point in 0..4u32 {
                let xy = m.lw(0x800b_1e94 + point * 8);
                m.gte.mtc2(reg::VXY0, xy);
                rtv0(m);
                let mac = read_mac(m);
                let out = SCRATCH + 0x20 + point * 0x10;
                for (offset, (value, base)) in (0..3u32).zip(mac.into_iter().zip(origin)) {
                    let world = (value << 3).wrapping_add(base);
                    m.sw(out + offset * 4, world);
                    last[offset as usize] = world;
                }
            }
            let gameplay = m.gw(common::GAMEPLAY);
            let phase = m.lw(gameplay + 0xc) & 3;
            if phase != slot & 3 {
                let handle = trail_handle(m, 0x1_2104, slot);
                let current = lb(m, handle);
                if !lt(current, 0) {
                    original(m, 0x8007_a7c0, [current, SCRATCH + 0x20, last[1], last[2]]);
                    continue;
                }
            }
            let handle = trail_handle(m, 0x1_2104, slot);
            let current = lb(m, handle);
            let next = original(m, 0x8007_a60c, [current, SCRATCH + 0x20, last[1], last[2]]);
            let handle = trail_handle(m, 0x1_2104, slot);
            m.sb(handle, next as u8);
        }
    });
}

/// 8007A8B4/8007B834: advance the trail point queues of `[gp+0x544]`:
/// 32 queues of `stride` bytes from `count_offset` (count word, first-index
/// word, ages at +8, lives at +0x88), active mask at `mask_offset`.
fn advance_queues(m: &mut Machine, count_offset: u32, mask_offset: u32, stride: u32) {
    let block = m.gw(GP_TRAILS);
    let mut mask = m.lw(block + mask_offset);
    for queue in 0..32u32 {
        if mask & bit(queue) == 0 {
            continue;
        }
        let count = block + count_offset + queue * stride;
        let first = count + 4;
        let mut cursor = m.lw(first);
        while lt(cursor, m.lw(count)) {
            let entry = count + ((cursor & 0x1f) << 2);
            let age = m.lw(entry + 8);
            let life = m.lw(entry + 0x88);
            m.sw(entry + 8, age.wrapping_add(1));
            m.sw(entry + 0x88, life.wrapping_sub(1));
            cursor = cursor.wrapping_add(1);
        }
        let start = m.lw(first);
        let entry = count + ((start & 0x1f) << 2);
        if lt(m.lw(entry + 0x88), 0) {
            m.sw(first, start.wrapping_add(1));
        }
        if !lt(m.lw(first), m.lw(count)) {
            mask &= !bit(queue);
        }
    }
    let block = m.gw(GP_TRAILS);
    m.sw(block + mask_offset, mask);
}

/// 8007A8B4: advance the missile trail queues (0x908 bytes from +4).
pub fn advance_trail_queues_7a8b4(m: &mut Machine) {
    advance_queues(m, 4, 0, 0x908);
}

/// 8007B834: advance the entity trail queues (0x508 bytes from +0x12128).
pub fn advance_entity_trail_queues_7b834(m: &mut Machine) {
    advance_queues(m, 0x1_2128, 0x1_2124, 0x508);
}

/// 8007C164: contrail points of present entities with trail flag (+5 bit 3)
/// and kind < 0x30: two model points (800B1EB4 + kind * 0x10) rotated by
/// the entity angles into scratch +0x20/+0x30, then a new trail (8007B5F0)
/// or the next segment (8007B77C) on the frame parity.
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
            if !lt(kind << 1, 0x60) || lt(kind, 0) {
                continue;
            }
            let points = (kind << 4).wrapping_add(0x800b_1eb4);
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
                let next = original(m, 0x8007_b5f0, [current, SCRATCH + 0x20, z, far[1]]);
                let handle = trail_handle(m, 0x1_c228, slot);
                m.sb(handle, next as u8);
            } else {
                let handle = trail_handle(m, 0x1_c228, slot);
                let current = lb(m, handle);
                if !lt(current, 0) {
                    original(m, 0x8007_b77c, [current, SCRATCH + 0x20, z, far[1]]);
                }
            }
        }
    });
}

// ==== 80083A64: timers and effects =====================================================

/// 80083A64: player proximity effects, then the effect timer tables of
/// `[gp+0x40C]`.
pub fn update_effects_83a64(m: &mut Machine) {
    with_frame(m, 0x18, |m, _| {
        let gameplay = m.gw(common::GAMEPLAY);
        let flags = m.lw(gameplay);
        let effects = m.gw(0x40c);
        if flags & 2 != 0 {
            let player = entity(m, 0);
            let view = m.gw(0x32c);
            let height = m.lw(view + 4).wrapping_neg();
            proximity_effect_81838(m, player + 8, player + 0x38, height);
            let player = entity(m, 0);
            if lt(0, lh(m, player + 0x4e)) {
                let context = m.gw(common::CONTEXT);
                let kind = lh(m, context + 0x390);
                let variant = lh(m, context + 0x392);
                approach_effect_83914(m, kind, variant);
            }
        }
        let context = m.gw(common::CONTEXT);
        if lh(m, context + 0x390) == 0x11 {
            let player = entity(m, 0);
            if lt(0, lh(m, player + 0x4e)) {
                original(m, 0x8008_3814, [player + 8, player + 0x38, 0, 0]);
            }
        }
        run_timers_88600(m, effects + 0x36c4);
        move_sprites_836ac(m, effects + 0x600, 3);
        move_sprites_836ac(m, effects + 0x1098, 10);
        age_timers_88768(m, effects + 0x33c0);
        update_shake_8e8d8(m, effects + 0x3948);
        toggle_bits_8fde4(m, effects + 0x3964);
        update_explosion_91084(m, effects + 0x3b28);
    });
}

/// 80081838: low-altitude spray behind a fast mover: `position` (x, y, z),
/// `velocity`, `height` (negated view height); intensity from the height
/// and the speed (800828F8), effect 3 through 8007D66C/80081738.
pub fn proximity_effect_81838(m: &mut Machine, position: u32, velocity: u32, height: u32) {
    with_frame(m, 0x38, |m, frame| {
        let gameplay = m.gw(common::GAMEPLAY);
        if m.lw(gameplay) & 0x100 != 0 {
            return;
        }
        let mut level = sra(m.lw(position + 4).wrapping_sub(height), 12).wrapping_add(0x400);
        if lt(level, 0) {
            return;
        }
        let view = m.lw(0x800b_af20);
        if m.lw(view + 0x38) & 0xf != 1 {
            return;
        }
        let args = [
            sra(m.lw(velocity), 4),
            sra(m.lw(velocity + 4), 4),
            sra(m.lw(velocity + 8), 4),
            0,
        ];
        let speed = original(m, 0x8008_28f8, args) << 4;
        if !lt(0x88bf, speed) {
            return;
        }
        level = level
            .wrapping_sub(sra(level, 2))
            .wrapping_add(sra(speed.wrapping_add(0xfffd_543e), 9));
        if lt(level, 0) {
            return;
        }
        if lt(level, 0x400) {
            // 800A4698 is the SDK `rcos` (not yet in sdk.rs).
            let cosine = original(m, 0x800a_4698, [level, 0, 0, 0]);
            level = sra(0x1000u32.wrapping_sub(cosine), 4);
        }
        if lt(level, 0x50) {
            return;
        }
        if !lt(level, 0x100) {
            level = 0xff;
        }
        let vx = m.lw(velocity);
        let x = m.lw(position);
        m.sw(frame + 0x14, height);
        m.sw(frame + 0x10, x.wrapping_sub(vx << 1));
        let vz = m.lw(velocity + 8);
        let z = m.lw(position + 8);
        m.sw(frame + 0x18, z.wrapping_sub(vz << 1));
        let effect = original(m, 0x8007_d66c, [3, frame + 0x10, 0, 0]);
        let word = (m.lw(effect + 8) & 0xffff_f00f) | ((level & 0xff) << 4);
        m.sw(effect + 8, word);
        original(m, 0x8008_1738, [frame + 0x10, level, word, 0]);
    });
}

/// 80083914: approach effect (kind 0x25, or variants 0x19/0x1A): when the
/// view is high enough and the player fast enough, effect 80081374 at the
/// player (slightly behind it unless variant 0x19..0x1B).
pub fn approach_effect_83914(m: &mut Machine, kind: u32, variant: u32) {
    with_frame(m, 0x30, |m, _| {
        let (limit, speed_limit, effect) = if kind == 0x25 {
            (0x780, 0x1e, 5)
        } else if variant.wrapping_sub(0x19) < 2 {
            (0xc00, 0x3c, variant.wrapping_sub(0x13))
        } else {
            return;
        };
        let view = m.lw(0x800b_af18);
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
        let speed = original(m, 0x8008_28f8, args);
        if lt(speed, speed_limit) {
            return;
        }
        let player = entity(m, 0);
        let mut point = [m.lw(player + 8), m.lw(player + 0xc), m.lw(player + 0x10)];
        if variant.wrapping_sub(0x19) >= 3 {
            for (coordinate, speed) in point.iter_mut().zip(velocity) {
                let lead = speed.wrapping_sub(sra(speed, 2));
                *coordinate = coordinate.wrapping_sub(lead);
            }
        }
        m.sw(SCRATCH, point[0]);
        m.sw(SCRATCH + 4, point[1]);
        m.sw(SCRATCH + 8, point[2]);
        original(m, 0x8008_1374, [SCRATCH, velocity_at, effect, point[2]]);
    });
}

/// 80088600: callback timers: mask word, then 0x14-byte records (count,
/// limit, ..., +0x10 callback). A running timer calls its callback with
/// (count, record); reaching the limit clears its mask bit.
pub fn run_timers_88600(m: &mut Machine, table: u32) {
    with_frame(m, 0x30, |m, _| {
        let mut pending = m.lw(table);
        let mut index = 0u32;
        let mut record = table + 4;
        while pending != 0 {
            if pending & 1 != 0 {
                let count = lhu(m, record).wrapping_add(1);
                m.sh(record, count as u16);
                let count = sext16(count);
                if !lt(count, lh(m, record + 2)) {
                    clear_word_bits(m, table, bit(index));
                } else {
                    let callback = m.lw(record + 0x10);
                    if callback != 0 {
                        dispatch::call(m, callback, [count, record, 0, 0]);
                    }
                }
            }
            pending >>= 1;
            index += 1;
            record = record.wrapping_add(0x14);
        }
    });
}

/// 800836AC: moving sprites: `groups` mask words at `masks`, then per group
/// 32 records of 0x1C bytes (count, limit, position words +4/+8/+0xC,
/// velocity halves +0x10..+0x14 in 1/256, mode +0x16: drag shift and rise).
pub fn move_sprites_836ac(m: &mut Machine, masks: u32, groups: u32) {
    if !lt(0, groups) {
        return;
    }
    let records = masks.wrapping_add(groups << 2);
    for group in 0..groups {
        let mask_at = masks.wrapping_add(group << 2);
        let mut pending = m.lw(mask_at);
        let mut record = records.wrapping_add(group.wrapping_mul(0x380));
        let mut index = 0u32;
        while pending != 0 {
            if pending & 1 != 0 {
                let count = lhu(m, record).wrapping_add(1);
                m.sh(record, count as u16);
                if !lt(sext16(count), lh(m, record + 2)) {
                    clear_word_bits(m, mask_at, bit(index));
                } else {
                    move_sprite(m, record);
                }
            }
            index += 1;
            pending >>= 1;
            record = record.wrapping_add(0x1c);
        }
    }
}

fn move_sprite(m: &mut Machine, record: u32) {
    let mode = lhu(m, record + 0x16);
    if mode & 0xff == 0 {
        return;
    }
    let mut dx = lh(m, record + 0x10) << 8;
    let mut dy = lh(m, record + 0x12) << 8;
    let mut dz = lh(m, record + 0x14) << 8;
    let x = m.lw(record + 4);
    let z = m.lw(record + 0xc);
    m.sw(record + 4, x.wrapping_add(dx));
    let y = m.lw(record + 8);
    m.sw(record + 0xc, z.wrapping_add(dz));
    m.sw(record + 8, y.wrapping_add(dy));
    if mode & 0x1f == 0 {
        return;
    }
    let drag = mode & 7;
    if drag != 0 {
        dx = dx.wrapping_sub(sra(dx, drag));
        dy = dy.wrapping_sub(sra(dy, drag));
        dz = dz.wrapping_sub(sra(dz, drag));
    }
    match mode & 0x18 {
        0 => {}
        8 => dy = dy.wrapping_add(0x8aa),
        0x10 => dy = dy.wrapping_add(0x1a00),
        _ => dy = dy.wrapping_add(0x3400),
    }
    m.sh(record + 0x10, sra(dx, 8) as u16);
    m.sh(record + 0x12, sra(dy, 8) as u16);
    m.sh(record + 0x14, sra(dz, 8) as u16);
}

/// 80088768: plain timers: mask word, then 0x18-byte records (count,
/// limit); reaching the limit clears the mask bit.
pub fn age_timers_88768(m: &mut Machine, table: u32) {
    let mut pending = m.lw(table);
    let mut record = table + 4;
    let mut index = 0u32;
    while pending != 0 {
        if pending & 1 != 0 {
            let count = lhu(m, record).wrapping_add(1);
            let limit = lh(m, record + 2);
            m.sh(record, count as u16);
            if !lt(sext16(count), limit) {
                clear_word_bits(m, table, bit(index));
            }
        }
        index += 1;
        pending >>= 1;
        record = record.wrapping_add(0x18);
    }
}

/// 8008E8D8: screen shake: count up to the limit (+4 high half), and for
/// kind 5 (+4 low half) during flight shake the player (80081B88).
pub fn update_shake_8e8d8(m: &mut Machine, state: u32) {
    with_frame(m, 0x18, |m, _| {
        let count = m.lw(state);
        if !lt(0, count) {
            return;
        }
        let info = m.lw(state + 4);
        if lt(count, sra(info, 16)) {
            m.sw(state, count.wrapping_add(1));
        } else {
            m.sw(state, 0);
        }
        let gameplay = m.gw(common::GAMEPLAY);
        if m.lw(gameplay) & 2 == 0 || info & 0xffff != 5 {
            return;
        }
        let mut strength = count.wrapping_sub(1) << 6;
        let player = m.lw(gameplay + 0x28);
        let context = m.gw(common::CONTEXT);
        let side = m.lw(player + 0x50) ^ 1;
        let mode = m.lw(context + 0x128) & 0x800;
        let own = u32::from(side == 0);
        if mode == 0 {
            strength = strength.wrapping_add(0x100);
            if !lt(strength, 0x401) {
                strength = 0x400;
            }
        }
        let player = entity(m, 0);
        original(m, 0x8008_1b88, [player + 8, player + 0x18, strength, own]);
    });
}

/// 8008FDE4: for every bit set in the first halfword, clear it in the second
/// halfword if set there, else clear it in the first.
pub fn toggle_bits_8fde4(m: &mut Machine, pair: u32) {
    let mut first = lhu(m, pair);
    if first == 0 {
        return;
    }
    let mut second = lhu(m, pair + 2);
    let mut pending = first;
    let mut mask = 1u32;
    while pending != 0 {
        if pending & 1 != 0 {
            if second & mask != 0 {
                second &= !mask;
            } else {
                first &= !mask;
            }
        }
        pending >>= 1;
        mask <<= 1;
    }
    m.sh(pair, first as u16);
    m.sh(pair + 2, second as u16);
}

/// 80091084: the large explosion sequence (+6 age up to 0x30): light ramp
/// (80081A0C), flash (80092350), debris at age 9 (8007E76C), fading glow
/// (80082E2C).
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
            original(m, 0x8008_1a0c, [state + 0x20, state + 0x14, level, 2]);
        }
        let phase = age.wrapping_sub(4);
        if phase < 5 {
            let size = sra(0x400, phase).wrapping_add(0x100);
            m.sh(SCRATCH, 0xff60);
            m.sh(SCRATCH + 2, 0xff80);
            m.sh(SCRATCH + 4, 0x140);
            m.sh(SCRATCH + 6, 0x100);
            let gameplay = m.gw(common::GAMEPLAY);
            let list = m.lw(gameplay + 0x10).wrapping_add(8);
            let next = m.lw(gameplay + 0x14);
            let next = original(m, 0x8009_2350, [list, next, SCRATCH, size]);
            let gameplay = m.gw(common::GAMEPLAY);
            m.sw(gameplay + 0x14, next);
        }
        if age == 9 {
            let pool = m.gw(0x40c).wrapping_add(0x33c0);
            let debris = original(m, 0x8007_e76c, [pool, 1, 6, 0]);
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
            let next = original(m, 0x8008_2e2c, [list, next, glow, 0]);
            let gameplay = m.gw(common::GAMEPLAY);
            m.sw(gameplay + 0x14, next);
        }
    });
}

// ==== 80039D08: lock-on ============================================================

/// 80039D08: lock-on update. `frame` is the caller's 6-word stack buffer
/// (80032CA4 sp+0x10): words +4/+8/+0xC in, +0x10 (reported target) and
/// +0x14 (secondary target) out. Returns the word stored to status +0x24.
pub fn update_lock_on_39d08(m: &mut Machine, frame: u32) -> u32 {
    with_frame(m, 0x20, |m, _| {
        update_player_lock_3a2c4(m);
        update_warning_3a614(m);
        let reported = lock_report_39f10(m);
        m.sw(frame + 0x10, reported);
        let secondary = secondary_target_3a064(m);
        m.sw(frame + 0x14, secondary);
        let word = m.lw(frame + 4);
        m.sw(STATUS + 0x20, word);
        let mode = shared::f_8003a08c(m, [0; 4]);
        let value = if mode != 0 {
            m.lw(frame + 0xc)
        } else {
            m.lw(frame + 8)
        };
        m.sw(STATUS + 0x24, value);
        value
    })
}

/// 8003A2C4: keep the player's lock target valid and apply the forced
/// targets (gameplay +4 bit 2: mission 3 target 2 or 8003A450/8003A4F0;
/// bit 0x800: the scripted target table 800AFA5C; else none).
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
            if lbu(m, MISSION) == 3 && m.lw(gameplay + 8) == 0 {
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
            let table = m.gw(0x324).wrapping_add(0x800a_fa5c);
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

/// 8003A614: missile warning: copy the player timer/level, and during
/// combat (gameplay +4 bit 2) position the warning sound at the missile
/// nearest the player and blink status bit 2 when it is close and ahead.
pub fn update_warning_3a614(m: &mut Machine) {
    with_frame(m, 0x40, |m, frame| {
        let player = m.gw(0x330);
        let timer = m.lw(player + 0x14);
        m.sw(TARGETING, timer);
        let status = m.lw(STATUS + 0x2c) & !2;
        let level = lbu(m, player + 0x1c);
        let gameplay = m.gw(common::GAMEPLAY);
        m.sw(STATUS + 0x2c, status);
        m.sh(TARGETING + 4, level as u16);
        if m.lw(gameplay + 4) & 4 == 0 {
            return;
        }
        if m.lw(gameplay + 0xc) == 0 {
            m.sw(frame + 0x10, 0x10);
            original(m, 0x8002_e8ec, [GP + 0x41c, 3, 3, 0x400]);
        }
        let shots = m.gw(GP_SHOTS);
        if m.lw(shots) & 1 == 0 {
            return;
        }
        let sound = 0x800c_4f28;
        let index = lbu(m, shots + 8);
        let missile = shots
            .wrapping_add(index.wrapping_mul(0xa4))
            .wrapping_add(0xe4);
        let gameplay = m.gw(common::GAMEPLAY);
        let listener = m.lw(gameplay + 0x28).wrapping_add(4);
        let player = entity(m, 0);
        original(m, 0x8002_e2c0, [sound, listener, missile, 0]);
        original(m, 0x8002_e910, [sound, GP + 0x41c, 0, 0]);
        let voice = lbu(m, GP + 0x41f);
        if voice != 0xff {
            original(m, 0x8002_e260, [voice, 0, 0, 0]);
            let voice = lbu(m, GP + 0x41f);
            original(m, 0x8002_e4c0, [voice, sound, 0, 0]);
        }
        let position = player + 8;
        let distance = shared::f_800827b4(m, [missile, position, 0, 0]);
        if !lt(distance, 0x2000) {
            return;
        }
        original(m, 0x8004_444c, [missile, position, frame + 0x18, 0]);
        if lt(m.lw(frame + 0x20), 0) {
            return;
        }
        let angle = original(m, 0x8004_4548, [frame + 0x18, 0, 0, 0]);
        if !lt(angle, 0x600) && !lt(distance, 0x800) {
            return;
        }
        set_word_bits(m, STATUS + 0x2c, 2);
        let period = (sra(distance, 9) as i32).max(2);
        let timer = m.lw(TARGETING) as i32;
        if timer % period == 0 {
            original(m, 0x8002_d7b8, [0x2c, 0, 0, 0]);
        }
    });
}

/// 80039F10: the target to report: +0x13 if set, else the player target
/// (+0xE), else 80039FB4 when context +0x12C bit 0x2000; context +0x128
/// bit 0x2000 clear forces the player target. Stored to +0x13; returns it
/// or -1 for 0xFF.
pub fn lock_report_39f10(m: &mut Machine) -> u32 {
    with_frame(m, 0x18, |m, _| {
        let mut target = lbu(m, TARGETING + 0x13);
        if target == 0xff {
            let lock = lbu(m, TARGETING + 0xe);
            if lock != target {
                target = lock;
            } else {
                let context = m.gw(common::CONTEXT);
                if m.lw(context + 0x12c) & 0x2000 != 0 {
                    target = nearest_marked_39fb4(m);
                }
            }
        }
        let context = m.gw(common::CONTEXT);
        if m.lw(context + 0x128) & 0x2000 == 0 {
            target = lbu(m, TARGETING + 0xe);
        }
        m.sb(TARGETING + 0x13, target as u8);
        if target == 0xff { u32::MAX } else { target }
    })
}

/// 8003A064: the secondary target (+0x12), or -1 for 0xFF.
pub fn secondary_target_3a064(m: &mut Machine) -> u32 {
    let target = lbu(m, TARGETING + 0x12);
    if target == 0xff { u32::MAX } else { target }
}

// ==== 8007DA7C: event recording =========================================================

/// 8007DA7C: record this frame's entity changes into the event ring
/// (`[gp+0x54C]`); the first frame snapshots all 64 entities twice.
pub fn record_frame_7da7c(m: &mut Machine) {
    with_frame(m, 0x28, |m, _| {
        let events = m.gw(GP_EVENTS);
        if m.lw(events) == 0 {
            let start = m.lw(events + 4);
            m.sw(events + 0x10, start);
            m.sw(events + 0xc, start);
            for slot in 0..64u32 {
                let source = entity(m, slot);
                let events = m.gw(GP_EVENTS);
                shared::copy_words(m, events + 0x18 + slot * 0x20, source, 8);
                let events = m.gw(GP_EVENTS);
                shared::copy_words(m, events + 0x818 + slot * 0x20, source, 8);
            }
        }
        record_changes_7d0a4(m);
        emit_event_7c948(m, 0, 0, 0, 0);
        let events = m.gw(GP_EVENTS);
        let frames = m.lw(events).wrapping_add(1);
        m.sw(events, frames);
    });
}

/// 8007D0A4: compare each entity with its recorded copy and emit change
/// events (presence, flags, kind, position/angles); the focused entity
/// (+0x184C) is always compared.
pub fn record_changes_7d0a4(m: &mut Machine) {
    with_frame(m, 0x30, |m, _| {
        for slot in 0..64u32 {
            let source = entity(m, slot);
            let events = m.gw(GP_EVENTS);
            let focus = m.lw(events + 0x184c);
            let copy = events + 0x818 + slot * 0x20;
            let forced = slot == focus & 0x3f && focus >> 31 == 0;
            if m.lw(copy) & 0x1_4000 != 0x1_4000 && m.lw(source) & 0x1_4000 != 0x1_4000 && !forced {
                store_entity_7d05c(m, source, copy);
                continue;
            }
            let changes = compare_entity_7cf40(m, copy, source);
            let flags = m.lw(source);
            if flags & 0x2000 == 0 && changes & 1 == 0 {
                continue;
            }
            if changes & 1 != 0 {
                let present = u32::from(flags & 0x1_4000 == 0x1_4000);
                emit_event_7c948(m, 1, slot, present, 0);
            }
            if changes & 4 != 0 {
                let a = lbu(m, source + 4);
                let b = lbu(m, source + 5);
                emit_event_7c948(m, 2, slot, a, b);
            }
            if changes & 8 != 0 {
                let kind = lh(m, source + 6);
                emit_event_7c948(m, 3, slot, kind, 0);
            }
            if changes & 0x10 != 0 {
                emit_position_7cb50(m, 0, slot, source + 8);
            }
            store_entity_7d05c(m, source, copy);
        }
    });
}

/// 8007CF40: change mask between a recorded copy and an entity: 1 presence
/// (0x14000), 4 flags half +4, 8 kind +6 (when +5 bit 2), 0x10 position
/// (>> 12) or angles (>> 2).
pub fn compare_entity_7cf40(m: &mut Machine, copy: u32, source: u32) -> u32 {
    let copy_half = lh(m, copy + 4);
    let copy_word = m.lw(copy);
    let word = m.lw(source);
    let half = lh(m, source + 4);
    let mut mask = u32::from((word ^ copy_word) & 0x1_4000 != 0);
    if copy_half != half {
        mask |= 4;
    }
    if lbu(m, source + 5) & 4 != 0 && lh(m, copy + 6) != lh(m, source + 6) {
        mask |= 8;
    }
    for offset in [8, 0xc, 0x10] {
        if sra(m.lw(copy + offset), 12) != sra(m.lw(source + offset), 12) {
            mask |= 0x10;
        }
    }
    for offset in [0x18, 0x1a, 0x1c] {
        if sra(lh(m, copy + offset), 2) != sra(lh(m, source + offset), 2) {
            mask |= 0x10;
        }
    }
    mask
}

/// 8007D05C: record an entity: its word, and when present (0x4000) also
/// +4..+0x13.
pub fn store_entity_7d05c(m: &mut Machine, source: u32, copy: u32) {
    let word = m.lw(source);
    if word & 0x4000 == 0 {
        m.sw(copy, word);
        return;
    }
    let half = m.lw(source + 4);
    m.sw(copy, word);
    m.sw(copy + 4, half);
    let x = m.lw(source + 8);
    let y = m.lw(source + 0xc);
    let z = m.lw(source + 0x10);
    m.sw(copy + 8, x);
    m.sw(copy + 0xc, y);
    m.sw(copy + 0x10, z);
}

/// 8007CB50: position event (type 4) of `slot`: coordinates >> 12 and
/// angles >> 2 packed into three words; `skip` reserves only.
pub fn emit_position_7cb50(m: &mut Machine, skip: u32, slot: u32, position: u32) {
    with_frame(m, 0x40, |m, _| {
        let record = shared::reserve_events(m, 4);
        if skip != 0 {
            return;
        }
        let mut word = m.lw(record);
        word = (word & 0xffff_fff0) | 4;
        word = (word & 0xffff_ff8f) | 0x10;
        word &= 0xffff_fe7f;
        word &= 0xffff_81ff;
        word |= (slot & 0x3f) << 9;
        m.sw(record, word);
        let ax = lhu(m, position + 0x10);
        let x = m.lw(position);
        let ay = lhu(m, position + 0x12);
        let az = lhu(m, position + 0x14);
        let first = ((ax >> 2) & 0xfff) | (x & 0xffff_f000);
        let y = m.lw(position + 4);
        let second = ((ay >> 2) & 0xfff) | (y & 0xffff_f000);
        let z = m.lw(position + 8);
        m.sw(record + 4, first);
        m.sw(record + 8, second);
        m.sw(record + 0xc, ((az >> 2) & 0xfff) | (z & 0xffff_f000));
    });
}

/// 8007C948: one-word event of `kind` 0..4 (0 frame marker, 1 presence,
/// 2 flags, 3 kind, 4 focus) for `slot` with operands `a`, `b`.
pub fn emit_event_7c948(m: &mut Machine, kind: u32, slot: u32, a: u32, b: u32) {
    with_frame(m, 0x30, |m, _| {
        let record = shared::reserve_events(m, 1);
        if kind >= 5 {
            return;
        }
        let header = |word: u32| {
            ((((word & 0xffff_fff0) | 1) & 0xffff_ff8f) & 0xffff_fc7f) | ((kind & 7) << 7)
        };
        let with_slot = |word: u32, slot: u32| (word & 0xffff_03ff) | ((slot & 0x3f) << 10);
        match kind {
            0 => m.sw(record, 1),
            1 => {
                let word = with_slot(header(m.lw(record)), slot);
                let word = (word & 0xfffe_ffff) | ((a & 1) << 16);
                m.sw(record, word & 0x1_ffff);
            }
            2 => {
                let word = with_slot(header(m.lw(record)), slot);
                m.sw(record, word);
                m.sb(record + 2, a as u8);
                m.sb(record + 3, b as u8);
            }
            3 => {
                let word = with_slot(header(m.lw(record)), slot);
                m.sw(record, (word & 0xff81) | ((a & 0x7fff) << 16));
            }
            _ => {
                let word = header(m.lw(record));
                m.sw(record, word);
                let slot = if lt(slot, 0) { 0 } else { slot };
                m.sw(record, with_slot(word, slot));
                m.sh(record + 2, 0);
            }
        }
    });
}

/// 8007D214: set the recorder focus entity (+0x184C) and emit a focus event.
pub fn set_event_focus_7d214(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let events = m.gw(GP_EVENTS);
        m.sw(events + 0x184c, slot);
        emit_event_7c948(m, 4, slot, 0, 0);
    });
}

/// The header of 8007CC34 events: length, class 2, `bits`, id in 9..15.
fn object_header(word: u32, length: u32, bits: u32, id: u32) -> u32 {
    let word = (((word & 0xffff_fff0) | length) & 0xffff_ff8f) | 0x20;
    (((word & 0xffff_fe7f) | bits) & 0xffff_01ff) | ((id & 0x7f) << 9)
}

/// Packed position word: coordinate high 20 bits, angle >> 2 low 12 bits.
fn packed(coordinate: u32, angle: u32) -> u32 {
    ((angle >> 2) & 0xfff) | (coordinate & 0xffff_f000)
}

/// 8007CC34: object event `kind` for object `id`: 0 spawn (6 words:
/// velocity >> 6 halves, packed position/angles of `position`, `extra`),
/// 1 expiry, 2 projectile state (`position` low half, packed `vector`),
/// 3 projectile end. `extra` is the fifth (caller stack) argument.
pub fn write_event_7cc34(
    m: &mut Machine,
    kind: u32,
    id: u32,
    position: u32,
    vector: u32,
    extra: u32,
) {
    with_frame(m, 0x58, |m, _| match kind {
        0 => {
            let record = shared::reserve_events(m, 6);
            m.sw(record + 0x14, extra);
            let word = object_header(m.lw(record), 6, 0, id);
            m.sw(record, word);
            let x = m.lw(vector);
            let z = m.lw(vector + 8);
            m.sh(record + 2, sra(x, 6) as u16);
            m.sh(record + 6, sra(z, 6) as u16);
            let y = m.lw(vector + 4);
            m.sh(record + 4, sra(y, 6) as u16);
            let words: [u32; 3] = std::array::from_fn(|i| {
                let coordinate = m.lw(position + i as u32 * 4);
                let angle = lhu(m, position + 0x10 + i as u32 * 2);
                packed(coordinate, angle)
            });
            m.sw(record + 8, words[0]);
            m.sw(record + 0xc, words[1]);
            m.sw(record + 0x10, words[2]);
        }
        1 => {
            let record = shared::reserve_events(m, 1);
            let word = object_header(m.lw(record), 1, 0x80, id);
            m.sw(record, word);
        }
        2 => {
            let record = shared::reserve_events(m, 4);
            let word = object_header(m.lw(record), 4, 0x100, id);
            m.sw(record, word);
            m.sh(record + 2, position as u16);
            let words: [u32; 3] = std::array::from_fn(|i| {
                let angle = lhu(m, vector + 0x10 + i as u32 * 2);
                let coordinate = m.lw(vector + i as u32 * 4);
                packed(coordinate, angle)
            });
            m.sw(record + 4, words[0]);
            m.sw(record + 8, words[1]);
            m.sw(record + 0xc, words[2]);
        }
        3 => {
            let record = shared::reserve_events(m, 1);
            let word = (((((m.lw(record) & 0xffff_fff0) | 1) & 0xffff_ff8f) | 0x1a0) & 0xffff_01ff)
                | ((id & 0x7f) << 9);
            m.sw(record, word);
        }
        _ => {}
    });
}

/// 8007D248: spawn event of particle `index` (8007CC34 kind 0).
pub fn write_particle_event_7d248(
    m: &mut Machine,
    index: u32,
    position: u32,
    velocity: u32,
    kind: u32,
) {
    with_frame(m, 0x20, |m, frame| {
        m.sw(frame + 0x10, kind);
        write_event_7cc34(m, 0, index, position, velocity, kind);
    });
}

/// 8007D284: expiry event of particle `index` (8007CC34 kind 1).
pub fn write_expiry_event_7d284(m: &mut Machine, index: u32) {
    with_frame(m, 0x20, |m, frame| {
        m.sw(frame + 0x10, 0);
        write_event_7cc34(m, 1, index, 0, 0, 0);
    });
}

/// 8007D2B4: state event of projectile `index` (8007CC34 kind 2).
pub fn write_projectile_event_7d2b4(m: &mut Machine, index: u32, value: u32, position: u32) {
    with_frame(m, 0x20, |m, frame| {
        m.sw(frame + 0x10, 0);
        write_event_7cc34(m, 2, index, value, position, 0);
    });
}

/// 8007D2EC: end event of projectile `index` (8007CC34 kind 3).
pub fn write_projectile_end_7d2ec(m: &mut Machine, index: u32) {
    with_frame(m, 0x20, |m, frame| {
        m.sw(frame + 0x10, 0);
        write_event_7cc34(m, 3, index, 0, 0, 0);
    });
}

/// 80039FB4: the present slot with flag 0x800000 whose object has the
/// lowest +4 value below 0x1000000, or 0xFF.
pub fn nearest_marked_39fb4(m: &mut Machine) -> u32 {
    let table = m.gw(common::ENTITIES);
    let mut best = 0x100_0000u32;
    let mut found = 0xffu32;
    for slot in 2..32u32 {
        let entity = m.lw(table.wrapping_add(slot << 2));
        if !alive(m, entity) || m.lw(SLOT_FLAGS + (slot << 2)) & 0x80_0000 == 0 {
            continue;
        }
        let object = m.lw(SLOT_OBJECTS + (slot << 2));
        if object == 0 {
            continue;
        }
        let value = m.lw(object + 4);
        if lt(value, best) {
            best = value;
            found = slot;
        }
    }
    found
}

/// 8003AB44: lock priority of `slot`: the slot number, plus 0x400000 for
/// priority objects (flag 2), plus (0x8000 - object +4) << 6 when the
/// object is active (flag 1).
pub fn lock_priority_3ab44(m: &mut Machine, slot: u32) -> u32 {
    let object = m.lw(SLOT_OBJECTS.wrapping_add(slot << 2));
    if object == 0 {
        return slot;
    }
    let flags = m.lw(object);
    if flags & 1 == 0 {
        return slot;
    }
    let mut score = slot;
    if flags & 2 != 0 {
        score = score.wrapping_add(0x40_0000);
    }
    let distance = m.lw(object + 4);
    score.wrapping_add(0x8000u32.wrapping_sub(distance) << 6)
}

/// 8003A820: choose a lock target relative to `current` (0xFF none):
/// scores every lockable slot 2..63 (8003AB44; slots above 31 read past the
/// 32-entry tables as the original does), prefers priority targets when
/// any is present, and returns the next-higher (`direction` != 0) or
/// next-lower scored slot, wrapping to the extremes. `mode` 1 skips
/// entities flagged 0x100.
pub fn choose_lock_3a820(m: &mut Machine, current: u32, direction: u32, mode: u32) -> u32 {
    with_frame(m, 0x150, |m, _| {
        let mut current = current;
        let mut current_score = 0u32;
        let mut priority = 0u32;
        let mut lower = 0xffu32;
        let mut higher = 0xffu32;
        let mut highest = 0u32;
        let mut highest_slot = 0xffu32;
        let mut lowest = 0x80_0000u32;
        let mut lowest_slot = 0xffu32;
        let mut below = 0u32;
        let mut above = 0x80_0000u32;
        if current & 0xff != 0xff {
            current_score = lock_priority_3ab44(m, current & 0xff);
            priority = u32::from(lt(0x3f_ffff, current_score));
        }
        let mut scores = [u32::MAX; 64];
        for slot in 2..64u32 {
            if slot == current & 0xff {
                continue;
            }
            let entity = entity(m, slot);
            let word = m.lw(entity);
            if word & 0x4000 == 0 {
                continue;
            }
            let flags = m.lw(SLOT_FLAGS.wrapping_add(slot << 2));
            if flags & 1 == 0 || flags & 0x8000 == 0 || word & 0x40 != 0 {
                continue;
            }
            if flags & 0x4_0000 != 0 && flags & 0x100 == 0 {
                continue;
            }
            if mode == 1 && word & 0x100 != 0 {
                continue;
            }
            let object = m.lw(SLOT_OBJECTS.wrapping_add(slot << 2));
            if m.lw(object) & 1 == 0 {
                continue;
            }
            let score = lock_priority_3ab44(m, slot);
            scores[slot as usize] = score;
            if priority & 0xff == 0 && lt(0x3f_ffff, score) {
                current = 0xff;
                priority = 1;
            }
        }
        for slot in 2..64u32 {
            let score = scores[slot as usize];
            if score == u32::MAX {
                continue;
            }
            if priority & 0xff == 1 && !lt(0x3f_ffff, score) {
                continue;
            }
            if lt(highest, score) {
                highest = score;
                highest_slot = slot;
            }
            if current & 0xff == 0xff {
                continue;
            }
            if lt(score, lowest) {
                lowest = score;
                lowest_slot = slot;
            }
            if lt(current_score, score) && lt(score, above) {
                above = score;
                higher = slot;
                continue;
            }
            if lt(below, score) && lt(score, current_score) {
                below = score;
                lower = slot;
            }
        }
        if current & 0xff == 0xff {
            lower = highest_slot;
        }
        if lower & 0xff == 0xff {
            lower = highest_slot;
        }
        if higher & 0xff == 0xff {
            higher = lowest_slot;
        }
        if lower & 0xff == 0xff {
            lower = current;
        }
        if higher & 0xff == 0xff {
            higher = current;
        }
        if direction != 0 {
            higher & 0xff
        } else {
            lower & 0xff
        }
    })
}

/// 8003A450: manual target switching: context +0x12C bit 0x200000 picks
/// the next-lower, bit 0x400000 the next-higher target. Returns 0.
pub fn switch_lock_3a450(m: &mut Machine) -> u32 {
    with_frame(m, 0x20, |m, _| {
        let context = m.gw(common::CONTEXT);
        let buttons = m.lw(context + 0x12c);
        let direction = if buttons & 0x20_0000 != 0 {
            0
        } else if buttons & 0x40_0000 != 0 {
            1
        } else {
            return 0;
        };
        let current = lbu(m, TARGETING + 0xe);
        let target = choose_lock_3a820(m, current, direction, 0);
        if target == 0xff || target == lbu(m, TARGETING + 0xe) {
            return 0;
        }
        m.sb(TARGETING + 0xe, target as u8);
        shared::release_lock(m, 0);
        shared::set_lock(m, 0, target);
        0
    })
}

/// 8003A4F0: automatic targeting: count frames on an ordinary target
/// (`gp+0x414`), and switch to the best target (8003A820 mode 1) when none
/// is locked, or after 31 frames when the new target's object +0x1E is at
/// least 31. Returns 0.
pub fn auto_lock_3a4f0(m: &mut Machine) -> u32 {
    with_frame(m, 0x18, |m, _| {
        if lbu(m, TARGETING + 0xe) != 0xff {
            let current = lbu(m, TARGETING + 0xe);
            let object = m.lw(SLOT_OBJECTS + (current << 2));
            if m.lw(object) & 2 != 0 || m.lw(object + 0x20) & 0x2_0000 != 0 {
                m.set_gw(0x414, 0);
                return 0;
            }
            let frames = m.gw(0x414).wrapping_add(1);
            m.set_gw(0x414, frames);
        }
        let target = choose_lock_3a820(m, 0xff, 0, 1);
        if target == 0xff {
            return 0;
        }
        let current = lbu(m, TARGETING + 0xe);
        if target == current {
            return 0;
        }
        if current != 0xff {
            if lt(m.gw(0x414), 0x1f) {
                return 0;
            }
            let object = m.lw(SLOT_OBJECTS.wrapping_add(target << 2));
            if lt(lh(m, object + 0x1e), 0x1f) {
                return 0;
            }
        }
        m.sb(TARGETING + 0xe, target as u8);
        shared::release_lock(m, 0);
        shared::set_lock(m, 0, target);
        m.set_gw(0x414, 0);
        0
    })
}

// ---- movement-state routines (800461B8 table) -----------------------------------------

/// 8004F070: clamp the height of `point` (+4) between the slot info's
/// limits (`[gp+0x374] + slot * 0x40 + 0x18` → +8 low, +4 high).
pub fn clamp_height_4f070(m: &mut Machine, slot: u32, point: u32) {
    let info_at = |m: &mut Machine| {
        let table = m.gw(0x374);
        m.lw((slot << 6).wrapping_add(table) + 0x18)
    };
    let info = info_at(m);
    let low = m.lw(info + 8);
    if lt(low, m.lw(point + 4)) {
        m.sw(point + 4, low);
    }
    let info = info_at(m);
    let high = m.lw(info + 4);
    if lt(m.lw(point + 4), high) {
        m.sw(point + 4, high);
    }
}

/// 8004EFBC: clamp a destination point to the slot's height limits and to
/// the mission area (`[gp+0x32C]` +0x10..+0x1C) less a 0x4000000 margin.
pub fn clamp_destination_4efbc(m: &mut Machine, slot: u32, point: u32) {
    with_frame(m, 0x18, |m, _| {
        clamp_height_4f070(m, slot, point);
        for (offset, coordinate, minimum) in [
            (0x10, 0, true),
            (0x14, 0, false),
            (0x18, 8, true),
            (0x1c, 8, false),
        ] {
            let area = m.gw(0x32c);
            let edge = m.lw(area + offset);
            let value = m.lw(point + coordinate);
            if minimum {
                let limit = edge.wrapping_add(0x400_0000);
                if lt(value, limit) {
                    m.sw(point + coordinate, limit);
                }
            } else {
                let limit = edge.wrapping_sub(0x400_0000);
                if lt(limit, value) {
                    m.sw(point + coordinate, limit);
                }
            }
        }
    });
}

/// 8004D794: movement state 1, pursue: destination = target (+0x3F)
/// position plus velocity, target angles copied; action and mode 4.
pub fn pursue_4d794(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        let target_slot = lbu(m, record + 0x3f);
        let target = entity(m, target_slot);
        if lbu(m, record + 0x3e) == 0 {
            m.sb(record + 0x3e, 1);
        }
        for offset in [0, 4, 8] {
            let position = m.lw(target + 8 + offset);
            let velocity = m.lw(target + 0x38 + offset);
            m.sw(record + 0x68 + offset, position.wrapping_add(velocity));
        }
        // lwl/lwr pairs: an unaligned-safe copy of the angles.
        let angles = m.read_bytes(target + 0x18, 8);
        m.write_bytes(record + 0x78, &angles);
        clamp_destination_4efbc(m, slot, record + 0x68);
        m.sb(record + 0x63, 4);
        m.sb(record + 0x61, 4);
    });
}

/// 8004DECC: movement state 0, patrol: on entry the destination becomes
/// the stored waypoint (+0x44..+0x53); action 5 for flag 0x2000000 else 0.
pub fn patrol_4decc(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        if lbu(m, record + 0x3e) == 0 {
            m.sb(record + 0x3e, 1);
            let words: [u32; 4] = std::array::from_fn(|i| m.lw(record + 0x44 + i as u32 * 4));
            for (i, word) in words.into_iter().enumerate() {
                m.sw(record + 0x68 + i as u32 * 4, word);
            }
            clamp_destination_4efbc(m, slot, record + 0x68);
            m.sb(record + 0x61, 0);
        }
        let action = if m.lw(record) & 0x200_0000 != 0 { 5 } else { 0 };
        m.sb(record + 0x63, action);
    });
}

// ---- behaviour checks of 80045DA8 ------------------------------------------------------

/// `div` quotient (FLIGHT_MACHINE.md rule 2). The original guards these
/// divisions with `break` traps for a zero divisor and for
/// `-0x80000000 / -1`; the hardware results are used here.
fn div(dividend: u32, divisor: u32) -> u32 {
    let (a, b) = (dividend as i32, divisor as i32);
    if b == 0 {
        return if a < 0 { 1 } else { u32::MAX };
    }
    a.wrapping_div(b) as u32
}

/// The frames-to-limit test shared by 80046828/80046968: `room` frames
/// remain at vertical speed `speed`; the turn needs |pitch / pitch rate| +
/// 2 + max(-roll / roll rate, 0) frames. Returns 1 when the turn does not
/// fit, else 0 (2 when already past the limit).
fn turn_fits(m: &mut Machine, entity: u32, params: u32, room: u32, speed: u32, rate: u32) -> u32 {
    let frames = div(room, speed);
    if lt(frames, 0) {
        return 2;
    }
    let pitch = lh(m, entity + 0x1c);
    let pitch_rate = sra(sext16(lhu(m, params + 0x18)), 1);
    let mut turn = div(pitch, pitch_rate);
    if lt(turn, 0) {
        turn = turn.wrapping_neg();
    }
    let roll = lh(m, entity + 0x18).wrapping_neg();
    let roll_rate = sra(sext16(lhu(m, params + rate)), 1);
    let mut level = div(roll, roll_rate);
    if lt(level, 0) {
        level = 0;
    }
    let needed = turn.wrapping_add(2).wrapping_add(level);
    u32::from(!lt(needed, frames))
}

/// 80046828: ceiling check of `slot`: 2 above the slot's ceiling (info
/// +0x18 → +8), 1 when climbing would reach it before the pull-out ends,
/// else 0.
pub fn ceiling_check_46828(m: &mut Machine, slot: u32) -> u32 {
    let entity = entity(m, slot);
    let info = (slot << 6).wrapping_add(m.gw(0x374));
    let limits = m.lw(info + 0x18);
    let y = m.lw(entity + 0xc);
    let ceiling = m.lw(limits + 8);
    let params = m.lw(info + 0x14);
    if lt(ceiling, y) {
        return 2;
    }
    let speed = m.lw(entity + 0x3c);
    if !lt(0, speed) {
        return 0;
    }
    turn_fits(m, entity, params, ceiling.wrapping_sub(y), speed, 0x1a)
}

/// 80046968: floor check of `slot` (info +0x18 → +4), the mirror of
/// 80046828 for descending.
pub fn floor_check_46968(m: &mut Machine, slot: u32) -> u32 {
    let entity = entity(m, slot);
    let info = (slot << 6).wrapping_add(m.gw(0x374));
    let limits = m.lw(info + 0x18);
    let y = m.lw(entity + 0xc);
    let floor = m.lw(limits + 4);
    let params = m.lw(info + 0x14);
    if lt(y, floor) {
        return 2;
    }
    let speed = m.lw(entity + 0x3c);
    if !lt(speed, 0) {
        return 0;
    }
    turn_fits(m, entity, params, floor.wrapping_sub(y), speed, 0x1c)
}

/// 80046BD4: proximity flags of `slot`'s AI record: set record flags
/// 0x3000000, then clear 0x1000000 when the entity is within +0x30 (x/z)
/// of its goal and 0x2000000 when within +0x34 (x/z, and 0x800000 in
/// height for the pursuit states 2, 4, 9, 10). The goal is the target
/// entity (+0x1F) in those states, else the waypoint (+0x20). Returns 0.
pub fn goal_flags_46bd4(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x10, |m, _| {
        let mover = entity(m, slot);
        let record = slot_record(m, slot);
        set_word_bits(m, record, 0x300_0000);
        let pursuing = matches!(lbu(m, record + 0x1c), 2 | 4 | 9 | 10);
        let source = if pursuing {
            let target = lbu(m, record + 0x1f);
            entity(m, target) + 8
        } else {
            record + 0x20
        };
        let goal: [u32; 4] = std::array::from_fn(|i| m.lw(source + i as u32 * 4));
        let within = |m: &mut Machine, radius: u32| {
            let x = m.lw(mover + 8);
            if lt(goal[0].wrapping_add(radius), x) || lt(x, goal[0].wrapping_sub(radius)) {
                return false;
            }
            let z = m.lw(mover + 0x10);
            !(lt(goal[2].wrapping_add(radius), z) || lt(z, goal[2].wrapping_sub(radius)))
        };
        let near = m.lw(record + 0x30);
        if within(m, near) {
            clear_word_bits(m, record, 0x100_0000);
        }
        let close = m.lw(record + 0x34);
        let pursuing = matches!(lbu(m, record + 0x1c), 2 | 4 | 9 | 10);
        if !within(m, close) {
            return 0;
        }
        if pursuing {
            let y = m.lw(mover + 0xc);
            if lt(goal[1].wrapping_add(0x80_0000), y) || lt(y, goal[1].wrapping_sub(0x80_0000)) {
                return 0;
            }
        }
        clear_word_bits(m, record, 0x200_0000);
        0
    })
}

/// 8004DA2C: movement state 4, escort: destination = target (+0x3F)
/// position (+8..+0x14 copied as four words); action 4, speed mode 6.
pub fn escort_4da2c(m: &mut Machine, slot: u32) {
    with_frame(m, 0x18, |m, _| {
        let record = slot_record(m, slot);
        let target_slot = lbu(m, record + 0x3f);
        let target = entity(m, target_slot);
        if lbu(m, record + 0x3e) == 0 {
            m.sb(record + 0x3e, 1);
        }
        let words: [u32; 4] = std::array::from_fn(|i| m.lw(target + 8 + i as u32 * 4));
        for (i, word) in words.into_iter().enumerate() {
            m.sw(record + 0x68 + i as u32 * 4, word);
        }
        clamp_destination_4efbc(m, slot, record + 0x68);
        m.sb(record + 0x63, 4);
        m.sb(record + 0x61, 6);
    });
}

/// 8004E504: movement state 10, hold: on entry action 0x1E; afterwards
/// record flag 0x400000 requests state 0xFF. Speed mode 2 until +0x64 is
/// set, then 0.
pub fn hold_4e504(m: &mut Machine, slot: u32) {
    let record = slot_record(m, slot);
    if lbu(m, record + 0x3e) == 0 {
        m.sb(record + 0x3e, 1);
        m.sb(record + 0x63, 0x1e);
    } else if m.lw(record) & 0x40_0000 != 0 {
        m.sb(record + 0x3d, 0xff);
    }
    let mode = if lbu(m, record + 0x64) == 0 { 2 } else { 0 };
    m.sb(record + 0x61, mode);
}

// ---- per-slot steering passes of 800463D8 -------------------------------------------------

/// 80043AF4: normalize Euler angles (three halfwords, 4096 = 360°): a
/// pitch beyond ±90° flips pitch and turns yaw and roll by 180°, then yaw
/// and roll wrap into -180°..+180°.
pub fn normalize_angles_43af4(m: &mut Machine, angles: u32) {
    let flip = |m: &mut Machine, pitch: u32| {
        m.sh(angles, pitch as u16);
        let yaw = lhu(m, angles + 2).wrapping_add(0x2000);
        let roll = lhu(m, angles + 4).wrapping_add(0x2000);
        m.sh(angles + 2, yaw as u16);
        m.sh(angles + 4, roll as u16);
    };
    if !lt(lh(m, angles), 0x1001) {
        let pitch = 0x2000u32.wrapping_sub(lhu(m, angles));
        flip(m, pitch);
    }
    if lt(lh(m, angles), 0xffff_f000) {
        let pitch = 0xffff_e000u32.wrapping_sub(lhu(m, angles));
        flip(m, pitch);
    }
    for offset in [2, 4] {
        if !lt(lh(m, angles + offset), 0x2000) {
            let value = lhu(m, angles + offset).wrapping_sub(0x4000);
            m.sh(angles + offset, value as u16);
        }
        if lt(lh(m, angles + offset), 0xffff_e000) {
            let value = lhu(m, angles + offset).wrapping_add(0x4000);
            m.sh(angles + offset, value as u16);
        }
    }
}

/// 80043BC4: roll rate (+8) of `slot` toward the requested change
/// `request` (signed), limited by the type's rate (params +0x18) and the
/// pilot skill (info +0x18 → +1). Returns the new rate.
pub fn roll_rate_43bc4(m: &mut Machine, slot: u32, request: u32) -> u32 {
    let table = m.gw(0x374);
    let info = (slot << 6).wrapping_add(table);
    let skill_at = m.lw(info + 0x18);
    let params = m.lw(info + 0x14);
    let skill = lbu(m, skill_at + 1).wrapping_add(1);
    let rate = lh(m, params + 0x18);
    let record = slot_record(m, slot);
    let limit = sra(
        rate.wrapping_mul(3).wrapping_add(sra(lo(rate, skill), 3)),
        2,
    );
    let current = lhu(m, record + 8);
    let signed = sext16(current);
    if !lt(request, 0) {
        let value = if lt(signed, 0xffff_ffff) {
            sra(signed, 1)
        } else if !lt(signed << 1, request) {
            sra(request, 1)
        } else {
            let step = if lt(request, sext16(limit)) {
                request
            } else {
                sext16(limit)
            };
            current
                .wrapping_add(1)
                .wrapping_add(sra(lo(step, skill), 8))
        };
        m.sh(record + 8, value as u16);
        if lt(sext16(limit), lh(m, record + 8)) {
            m.sh(record + 8, limit as u16);
        }
    } else {
        let value = if lt(0, signed) {
            sra(signed, 1)
        } else if !lt(request, signed << 1) {
            sra(request, 1)
        } else {
            let floor = sext16(limit).wrapping_neg();
            let step = if lt(floor, request) { request } else { floor };
            current.wrapping_add(sra(lo(step, skill), 8))
        };
        m.sh(record + 8, value as u16);
        if lt(lh(m, record + 8), sext16(limit).wrapping_neg()) {
            m.sh(record + 8, limit.wrapping_neg() as u16);
        }
    }
    lh(m, record + 8)
}

/// 80043D3C: pitch rate (+4) of `slot` toward `request`, capped by the
/// type's climb (+0x1A) and dive (+0x1C) rates; on frames whose phase
/// (gameplay +0xC & 3) exceeds the pilot skill the rate holds. Returns the
/// new rate.
pub fn pitch_rate_43d3c(m: &mut Machine, slot: u32, request: u32) -> u32 {
    let record = slot_record(m, slot);
    let table = m.gw(0x374);
    let info = (slot << 6).wrapping_add(table);
    let params = m.lw(info + 0x14);
    let skilled = |m: &mut Machine| {
        let gameplay = m.gw(common::GAMEPLAY);
        let skill_at = m.lw(info + 0x18);
        let phase = m.lw(gameplay + 0xc) & 3;
        let skill = lbu(m, skill_at + 1);
        !lt(skill, phase)
    };
    let mut request = request;
    if !lt(request, 0) {
        let current = lhu(m, record + 4);
        let signed = sext16(current);
        if lt(signed, 0xffff_ffff) {
            m.sh(record + 4, sra(signed, 1) as u16);
        } else if !lt(signed << 1, request) {
            m.sh(record + 4, sra(request, 1) as u16);
        } else {
            let cap = lh(m, params + 0x1a);
            if !lt(request, cap) {
                request = cap;
            }
            if skilled(m) {
                let value = current.wrapping_add(1).wrapping_add(sra(request, 4));
                m.sh(record + 4, value as u16);
            }
        }
        if lt(lh(m, params + 0x1a), lh(m, record + 4)) {
            let cap = m.lhu(params + 0x1a);
            m.sh(record + 4, cap);
        }
    } else {
        let current = lhu(m, record + 4);
        let signed = sext16(current);
        if lt(0, signed) {
            m.sh(record + 4, sra(signed, 1) as u16);
        } else if !lt(request, signed << 1) {
            m.sh(record + 4, sra(request, 1) as u16);
        } else {
            let cap = lh(m, params + 0x1c);
            if !lt(cap, request) {
                request = cap;
            }
            if skilled(m) {
                let value = current.wrapping_add(sra(request, 4));
                m.sh(record + 4, value as u16);
            }
        }
        if lt(lh(m, record + 4), lh(m, params + 0x1c)) {
            let cap = m.lhu(params + 0x1c);
            m.sh(record + 4, cap);
        }
    }
    lh(m, record + 4)
}

/// 80043E94: yaw rate (+6) of `slot` = `request` limited to the type's
/// ±(params +0x1E). Returns the new rate.
pub fn yaw_rate_43e94(m: &mut Machine, slot: u32, request: u32) -> u32 {
    let record = slot_record(m, slot);
    let table = m.gw(0x374);
    let params = m.lw((slot << 6).wrapping_add(table) + 0x14);
    if request == 0 {
        m.sh(record + 6, 0);
    } else if lt(0, request) {
        let cap = lh(m, params + 0x1e);
        let value = if lt(request, cap) {
            request
        } else {
            lhu(m, params + 0x1e)
        };
        m.sh(record + 6, value as u16);
    } else {
        let cap = lh(m, params + 0x1e).wrapping_neg();
        let value = if lt(cap, request) {
            request
        } else {
            lhu(m, params + 0x1e).wrapping_neg()
        };
        m.sh(record + 6, value as u16);
    }
    lh(m, record + 6)
}

/// 80043F24: steering of `slot`: contrail flag (+5 bit 3) while turning
/// hard, then apply the requested pitch (+0x80), roll (+0x84) and yaw
/// (+0x82) changes through the rate limiters and normalize the angles.
pub fn steer_43f24(m: &mut Machine, slot: u32) {
    with_frame(m, 0x20, |m, _| {
        let record = slot_record(m, slot);
        let object = m.lw(SLOT_OBJECTS.wrapping_add(slot << 2));
        let mover = entity(m, slot);
        let set_trail = |m: &mut Machine| {
            let flags = lbu(m, mover + 5) | 8;
            m.sb(mover + 5, flags as u8);
        };
        let keep = object != 0 && lt(m.lw(object + 4), 0x4000) && {
            if !lt(lh(m, record + 0x80), 0x800) {
                set_trail(m);
            }
            if !lt(abs(lh(m, record + 0x84)), 0x1000) {
                set_trail(m);
            }
            lh(m, record + 0x80) != 0 || lh(m, record + 0x84) != 0
        };
        if !keep {
            let flags = lbu(m, mover + 5) & 0xf7;
            m.sb(mover + 5, flags as u8);
        }
        let request = lh(m, record + 0x80);
        let pitch = pitch_rate_43d3c(m, slot, request);
        if pitch != 0 {
            let flags = m.lw(mover + 0x54);
            m.sh(mover + 0x30, pitch as u16);
            m.sw(mover + 0x54, flags | 0x80_0000);
            let left = lhu(m, record + 0x80).wrapping_sub(pitch);
            m.sh(record + 0x80, left as u16);
        }
        let request = lh(m, record + 0x84);
        let roll = roll_rate_43bc4(m, slot, request);
        if roll != 0 {
            let angle = lhu(m, mover + 0x1c).wrapping_add(roll);
            m.sh(mover + 0x1c, angle as u16);
            let left = lhu(m, record + 0x84).wrapping_sub(roll);
            m.sh(record + 0x84, left as u16);
        }
        let request = lh(m, record + 0x82);
        let yaw = yaw_rate_43e94(m, slot, request);
        if yaw != 0 {
            let flags = m.lw(mover + 0x54);
            m.sh(mover + 0x32, yaw as u16);
            m.sw(mover + 0x54, flags | 0x80_0000);
            let left = lhu(m, record + 0x82).wrapping_sub(yaw);
            m.sh(record + 0x82, left as u16);
        }
        normalize_angles_43af4(m, mover + 0x18);
    });
}

/// 8006D614: altitude-dependent speed limits: from the height `altitude`
/// (clamped to -50000..0 in 4096 units) and the sine table at 80010498,
/// store the stall speed (`low`), cruise speed (`cruise`) and top speed
/// (`high`); returns the thrust factor (<= 0x1000).
pub fn speed_limits_6d614(m: &mut Machine, altitude: u32, low: u32, cruise: u32, high: u32) -> u32 {
    let bottom = 0xffff_3cb0u32;
    let mut height = sra(altitude, 12);
    if lt(0, height) {
        height = 0;
    }
    if lt(height, bottom) {
        height = bottom;
    }
    let fraction = div((height.wrapping_sub(bottom) << 14).wrapping_neg(), bottom);
    let top = lo(fraction, 0x7000);
    let first = (sra(fraction.wrapping_mul(5), 2) & 0x7ffe).wrapping_add(0x8001_0498);
    let ratio = div(height << 14, bottom);
    let sine = lh(m, first);
    let scaled = lo(sine, 0x1c00);
    let second = (ratio & 0x7ffe).wrapping_add(0x8001_0498);
    let sine = lh(m, second);
    let bend = lo(sine, 0xb800);
    let mut fastest = sra(top, 14)
        .wrapping_add(0x5_2000)
        .wrapping_add(sra(scaled, 13));
    let slowest = sra(lo(ratio, 0xb800), 12)
        .wrapping_add(0xd000)
        .wrapping_sub(sra(bend, 14));
    let limit = slowest.wrapping_add(0x4_a000);
    if lt(limit, fastest) {
        fastest = limit;
    }
    let thrust = div(height.wrapping_sub(0xffff_6d84) << 12, 0xffff_cf2c);
    m.sw(low, slowest);
    m.sw(high, fastest);
    let cruising = sra(lo(ratio, 0xa200), 12).wrapping_add(0x1_e000);
    m.sw(cruise, cruising);
    let factor = 0x1000u32.wrapping_sub(thrust);
    if lt(factor, 0x1001) { factor } else { 0x1000 }
}

/// 800440E0: throttle of `slot`: pick the target speed for the speed mode
/// (+0x61), clamp it to the type's range (params +0xC..+8), change the
/// acceleration (+0x14) toward it by the pilot skill (mode-dependent caps)
/// and integrate the entity speed (+0x28). Returns 1 without an object,
/// else 0.
pub fn throttle_440e0(m: &mut Machine, slot: u32) -> u32 {
    with_frame(m, 0x38, |m, frame| {
        let record = slot_record(m, slot);
        let mover = entity(m, slot);
        let info = (slot << 6).wrapping_add(m.gw(0x374));
        let object = m.lw(SLOT_OBJECTS.wrapping_add(slot << 2));
        let params = m.lw(info + 0x14);
        if object == 0 {
            return 1;
        }
        let skill_at = m.lw(info + 0x18);
        let mut accel_cap = m.lw(params + 0x10);
        let mut step = lbu(m, skill_at + 1).wrapping_add(1);
        let altitude = m.lw(mover + 0xc);
        speed_limits_6d614(m, altitude, frame + 0x10, frame + 0x14, frame + 0x18);
        let mut minimum = m.lw(params + 0xc);
        let mode = lbu(m, record + 0x61);
        let cruise = m.lw(params + 4);
        let maximum = m.lw(params + 8);
        let target_of = |m: &mut Machine| {
            let target_slot = lbu(m, record + 0x3f);
            (target_slot, entity(m, target_slot))
        };
        // The approach term: +0x88 (scaled by 8 above 0x4000), negated when
        // +0x94 is negative.
        let approach = |m: &mut Machine| {
            let closing = m.lw(record + 0x88);
            let term = if lt(m.lw(record + 0x94), 0) {
                closing.wrapping_neg()
            } else {
                m.lw(record + 0x88)
            };
            if lt(closing, 0x4001) { term } else { term << 3 }
        };
        let target = match mode {
            0 => cruise,
            1 => m.lw(params + 8),
            2 => m.lw(params + 0xc),
            3 => m.lw(frame + 0x14),
            4 => {
                let (target_slot, target) = target_of(m);
                if target_slot == 0 || lbu(m, record + 0x60) == 0 {
                    if lt(m.lw(record + 0x94), 0) {
                        m.lw(frame + 0x14)
                    } else {
                        let closing = m.lw(record + 0x88);
                        m.lw(frame + 0x14).wrapping_add(closing << 3)
                    }
                } else if m.lw(mover) & 0x40 == 0 || lt(m.lw(record + 0x94), 0) {
                    m.lw(params + 4)
                } else {
                    let closing = m.lw(record + 0x88);
                    m.lw(target + 0x28).wrapping_add(closing << 3)
                }
            }
            5 | 7 => {
                if mode == 5 {
                    accel_cap = 0x100;
                    step = 8;
                }
                let (target_slot, target) = target_of(m);
                let base = if target_slot == 0 {
                    m.lw(frame + 0x14)
                } else {
                    m.lw(target + 0x28)
                };
                base.wrapping_add(approach(m))
            }
            6 => {
                let (_, target) = target_of(m);
                if lt(m.lw(record + 0x94), 0) {
                    m.lw(params + 4)
                } else {
                    let closing = m.lw(record + 0x88);
                    m.lw(target + 0x28).wrapping_add(closing)
                }
            }
            8 => {
                accel_cap = 0x100;
                step = 8;
                let speed = m.lw(frame + 0x14);
                speed.wrapping_sub(sra(speed, 3))
            }
            9 => return 0,
            10 => {
                let target = minimum;
                minimum = 0;
                accel_cap = 0x100;
                step = 8;
                target
            }
            _ => cruise,
        };
        let target = if lt(maximum, target) {
            maximum
        } else if lt(target, minimum) {
            minimum
        } else {
            target
        };
        let speed = m.lw(mover + 0x28);
        let mut accel = m.lw(record + 0x14);
        let delta = target.wrapping_sub(speed);
        if lt(accel, delta) {
            m.sw(record + 0x14, accel.wrapping_add(step));
            accel = m.lw(record + 0x14);
        } else if lt(delta, accel) {
            m.sw(record + 0x14, accel.wrapping_sub(step));
            accel = m.lw(record + 0x14);
        }
        if lt(accel_cap, accel) {
            m.sw(record + 0x14, accel_cap);
        } else if lt(accel, accel_cap.wrapping_neg()) {
            m.sw(record + 0x14, accel_cap.wrapping_neg());
        }
        let flags = m.lw(mover + 0x54) | 0x20_0000;
        let speed = m.lw(mover + 0x28);
        m.sw(mover + 0x54, flags);
        let speed = speed.wrapping_add(m.lw(record + 0x14));
        m.sw(mover + 0x28, speed);
        if lt(maximum, speed) {
            m.sw(mover + 0x28, maximum);
        } else if lt(speed, minimum) {
            m.sw(mover + 0x28, minimum);
        }
        0
    })
}

// ==== indirect-call claims ============================================================

/// Native targets owned by this module (every translated routine, by its
/// original address and a0..a3; returns v0, 0 for routines without one).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let v0 = match target {
        0x8003_8728 => {
            update_entities_38728(m);
            0
        }
        0x8003_7dfc => {
            mark_entities_37dfc(m);
            0
        }
        0x8003_7da0 => {
            run_entity_logic_37da0(m);
            0
        }
        0x8004_59c0 => {
            retire_entities_459c0(m);
            0
        }
        0x8004_5c10 => {
            reset_idle_45c10(m);
            0
        }
        0x8004_a2c4 => {
            update_groups_4a2c4(m);
            0
        }
        0x8004_a6e4 => {
            mark_formations_4a6e4(m);
            0
        }
        0x8004_5da8 => {
            select_behaviour_45da8(m);
            0
        }
        0x8004_61b8 => {
            run_movement_461b8(m);
            0
        }
        0x8004_63d8 => {
            run_actions_463d8(m);
            0
        }
        0x8004_7598 => {
            nearest_targets_47598(m);
            0
        }
        0x8002_f354 => {
            assign_tracks_2f354(m, a0, a1);
            0
        }
        0x8002_f590 => {
            update_track_sounds_2f590(m, a0);
            0
        }
        0x8003_809c => {
            integrate_entities_3809c(m);
            0
        }
        0x8006_ade4 => {
            update_shots_6ade4(m);
            0
        }
        0x8006_9940 => {
            spawn_particles_69940(m);
            0
        }
        0x8006_9d34 => {
            update_particles_69d34(m);
            0
        }
        0x8006_9750 => {
            update_projectiles_69750(m);
            0
        }
        0x8006_894c => {
            projectile_collisions_6894c(m);
            0
        }
        0x8006_93e8 => {
            move_projectiles_693e8(m);
            0
        }
        0x8006_87f8 => {
            run_projectiles_687f8(m);
            0
        }
        0x8006_87f0 => {
            idle_projectile_687f0(m);
            0
        }
        0x8006_9464 => {
            age_projectiles_69464(m);
            0
        }
        0x8006_6d04 => {
            apply_hit_timers_66d04(m);
            0
        }
        0x8007_c44c => {
            update_trails_7c44c(m);
            0
        }
        0x8007_b36c => {
            missile_trails_7b36c(m);
            0
        }
        0x8007_a8b4 => {
            advance_trail_queues_7a8b4(m);
            0
        }
        0x8007_c164 => {
            entity_trails_7c164(m);
            0
        }
        0x8007_b834 => {
            advance_entity_trail_queues_7b834(m);
            0
        }
        0x8008_3a64 => {
            update_effects_83a64(m);
            0
        }
        0x8008_1838 => {
            proximity_effect_81838(m, a0, a1, a2);
            0
        }
        0x8008_3914 => {
            approach_effect_83914(m, a0, a1);
            0
        }
        0x8008_8600 => {
            run_timers_88600(m, a0);
            0
        }
        0x8008_36ac => {
            move_sprites_836ac(m, a0, a1);
            0
        }
        0x8008_8768 => {
            age_timers_88768(m, a0);
            0
        }
        0x8008_e8d8 => {
            update_shake_8e8d8(m, a0);
            0
        }
        0x8008_fde4 => {
            toggle_bits_8fde4(m, a0);
            0
        }
        0x8009_1084 => {
            update_explosion_91084(m, a0);
            0
        }
        0x8003_9d08 => update_lock_on_39d08(m, a0),
        0x8003_a2c4 => {
            update_player_lock_3a2c4(m);
            0
        }
        0x8003_a614 => {
            update_warning_3a614(m);
            0
        }
        0x8003_9f10 => lock_report_39f10(m),
        0x8003_a064 => secondary_target_3a064(m),
        0x8007_da7c => {
            record_frame_7da7c(m);
            0
        }
        0x8007_d0a4 => {
            record_changes_7d0a4(m);
            0
        }
        0x8007_cf40 => compare_entity_7cf40(m, a0, a1),
        0x8007_d05c => {
            store_entity_7d05c(m, a0, a1);
            0
        }
        0x8007_cb50 => {
            emit_position_7cb50(m, a0, a1, a2);
            0
        }
        0x8007_c948 => {
            emit_event_7c948(m, a0, a1, a2, a3);
            0
        }
        0x8007_d214 => {
            set_event_focus_7d214(m, a0);
            0
        }
        0x8003_9fb4 => nearest_marked_39fb4(m),
        0x8003_ab44 => lock_priority_3ab44(m, a0),
        0x8003_a820 => choose_lock_3a820(m, a0, a1, a2),
        0x8003_a450 => switch_lock_3a450(m),
        0x8003_a4f0 => auto_lock_3a4f0(m),
        0x8004_f070 => {
            clamp_height_4f070(m, a0, a1);
            0
        }
        0x8004_efbc => {
            clamp_destination_4efbc(m, a0, a1);
            0
        }
        0x8004_d794 => {
            pursue_4d794(m, a0);
            0
        }
        0x8004_decc => {
            patrol_4decc(m, a0);
            0
        }
        0x8004_6828 => ceiling_check_46828(m, a0),
        0x8004_6968 => floor_check_46968(m, a0),
        0x8004_6bd4 => goal_flags_46bd4(m, a0),
        0x8004_da2c => {
            escort_4da2c(m, a0);
            0
        }
        0x8004_e504 => {
            hold_4e504(m, a0);
            0
        }
        0x8004_3af4 => {
            normalize_angles_43af4(m, a0);
            0
        }
        0x8004_3bc4 => roll_rate_43bc4(m, a0, a1),
        0x8004_3d3c => pitch_rate_43d3c(m, a0, a1),
        0x8004_3e94 => yaw_rate_43e94(m, a0, a1),
        0x8004_3f24 => {
            steer_43f24(m, a0);
            0
        }
        0x8006_d614 => speed_limits_6d614(m, a0, a1, a2, a3),
        0x8004_40e0 => throttle_440e0(m, a0),
        0x8007_cc34 => {
            // The fifth argument is the caller's stack word +0x10.
            let extra = m.lw(m.sp.wrapping_add(0x10));
            write_event_7cc34(m, a0, a1, a2, a3, extra);
            0
        }
        0x8007_d248 => {
            write_particle_event_7d248(m, a0, a1, a2, a3);
            0
        }
        0x8007_d284 => {
            write_expiry_event_7d284(m, a0);
            0
        }
        0x8007_d2b4 => {
            write_projectile_event_7d2b4(m, a0, a1, a2);
            0
        }
        0x8007_d2ec => {
            write_projectile_end_7d2ec(m, a0);
            0
        }
        _ => return None,
    };
    Some(v0)
}
