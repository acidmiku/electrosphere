//! Patches of `shared` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, resident part 1). Each is listed in
//! [`REPLACES`](super::REPLACES).
use crate::common;
use crate::machine::{Machine, SmallData};
use crate::shared::{ENTITY_FLAGS, LOCK_LINKS, TARGET_INDEX, release_lock};

/// 800C6630: sound channel records of 0x5C bytes (`shared.rs`).
const SOUND_CHANNELS: u32 = 0x800c_6630;
/// 800B0964: the sound byte of each entity kind (`shared.rs`).
const KIND_SOUNDS: u32 = 0x800b_0964;
/// 800C6D72: the byte 8003A62C reads as its target index (PAL reads the
/// current target 800C4B36 there, whose JP counterpart is 800C6D6E).
const MODE_TARGET_INDEX: u32 = 0x800c_6d72;
/// Context +0x38C (JP): the mission halfword, which replaces PAL's byte
/// 800BAEB4.
const CONTEXT_MISSION: u32 = 0x38c;

/// 8002F70C (JP; PAL 8002F2F0, near, `imm:1`): (channel, entity index) ->
/// v0. When entity `index`'s kind (halfword +0x48) is below 0x30 (PAL
/// 0x40), channel record `channel` +4 receives the kind's sound byte, which
/// is returned; otherwise nothing is written and 0 is returned.
pub fn request_kind_sound(m: &mut Machine, channel: u32, index: u32) -> u32 {
    let table = m.gw(common::ENTITIES);
    let entity = m.lw(table.wrapping_add(index << 2));
    let kind = u32::from(m.lhu(entity.wrapping_add(0x48)));
    if kind >= 0x30 {
        return 0;
    }
    let sound = u32::from(m.lbu(KIND_SOUNDS.wrapping_add(kind & 0x3f)));
    m.sw(
        SOUND_CHANNELS
            .wrapping_add(channel.wrapping_mul(0x5c))
            .wrapping_add(4),
        sound,
    );
    sound
}

/// 8003A5C0 (JP; PAL 8003A08C, near, `imm:1`): () -> 1 when the target
/// entity (index byte 800C6D6E, not 0xFF) is not in state 0x51 (PAL 100;
/// `entity +0x48`) and its flag word (800C6FD0) lacks 0x400; otherwise 0.
pub fn target_available(m: &mut Machine) -> u32 {
    let index = u32::from(m.lbu(TARGET_INDEX));
    if index == 0xff {
        return 0;
    }
    let table = m.gw(common::ENTITIES);
    let entity = m.lw(table.wrapping_add(index << 2));
    if m.lh(entity.wrapping_add(0x48)) == 0x51 {
        return 0;
    }
    let flags = m.lw(ENTITY_FLAGS.wrapping_add(index << 2));
    u32::from(flags & 0x400 == 0)
}

/// 8003A62C (JP; PAL 8003A0F8, changed, `op:3 reg:5`): () -> 11 when the
/// mission (context +0x38C, a signed halfword; PAL: byte 800BAEB4) is 0x16
/// and the entity whose index is the byte 800C6D72 (not 0xFF) has flag
/// 0x100 set and 0x400 clear; otherwise -1.
pub fn target_mode_code(m: &mut Machine) -> u32 {
    let context = m.gw(common::CONTEXT);
    let mission = i32::from(m.lh(context.wrapping_add(CONTEXT_MISSION)));
    let index = u32::from(m.lbu(MODE_TARGET_INDEX));
    if mission != 0x16 || index == 0xff {
        return -1i32 as u32;
    }
    let flags = m.lw(ENTITY_FLAGS.wrapping_add(index << 2));
    if flags & 0x100 != 0 && !flags & 0x400 != 0 {
        0xb
    } else {
        -1i32 as u32
    }
}

/// 800463B8 (JP; PAL 8004577C, near, `imm:1`): lock `slot` onto `target`
/// (0x20-byte frame). A target keeps two trackers (+2, +3); the player
/// (target 0) takes a second one only while context +0x394 (PAL +0x398) is
/// set, and a fully tracked target yields its +3 tracker to the player.
/// Returns `target`, or 0xFF when refused.
pub fn set_lock(m: &mut Machine, slot: u32, target: u32) -> u32 {
    m.push_frame(0x20);
    let result = set_lock_framed(m, slot, target);
    m.pop_frame(0x20);
    result
}

fn set_lock_framed(m: &mut Machine, slot: u32, target: u32) -> u32 {
    if target == 0xff {
        return target;
    }
    if target == 0 && u32::from(m.lbu(LOCK_LINKS + 1)) == slot {
        return 0xff;
    }
    let record = LOCK_LINKS.wrapping_add(target << 2);
    if m.lbu(record.wrapping_add(2)) == 0xff {
        m.sb(record.wrapping_add(2), slot as u8);
    } else if m.lbu(record.wrapping_add(3)) == 0xff {
        let context = m.gw(common::CONTEXT);
        if m.lw(context.wrapping_add(0x394)) == 0 && target == 0 {
            return 0xff;
        }
        m.sb(record.wrapping_add(3), slot as u8);
    } else {
        if slot != 0 {
            return 0xff;
        }
        let previous = u32::from(m.lbu(record.wrapping_add(3)));
        release_lock(m, previous);
        m.sb(record.wrapping_add(3), 0);
    }
    m.sb(
        LOCK_LINKS.wrapping_add(slot << 2).wrapping_add(1),
        target as u8,
    );
    target
}
