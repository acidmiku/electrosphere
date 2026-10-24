//! Patches of `init_38818` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES).
//!
//! The JP mission start has no language: the message tables are one list
//! (80056198), the mission number byte of PAL 80038818 (`0x800B0210[context
//! +0x390]`, the `MISSION_NUMBERS` site of REVIEW.md §1) is not set here,
//! the result block lost its special-mission score restore (PAL
//! `gp+0x464..0x468`), and the stream of the mission state comes from the
//! mission index (context half +0x38C). The gp offsets the generated
//! 80056198 flags for review are the JP message words it writes.
use crate::common::{CONTEXT, ENTITIES, GAMEPLAY};
use crate::init_38818::{
    RESOURCES, bind_player_object_800387e0, initialize_entities_80038758,
    initialize_objects_8004cdb0, load_mission_tables_8004bbb8, load_script_tables_800527c8,
    reset_hud_8002c7a4, reset_links_800453b8, stream_of_800550d8,
};
use crate::machine::{GP, Machine, SmallData};
use crate::{arena, common, dispatch, script, sdk};

fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw(table.wrapping_add(index.wrapping_mul(4)))
}

/// 80038B2C (JP; PAL 80038818, near, `call+:1 call-:1 del:10 ins:3 op:3
/// reg:1`): initialize the mission subsystems for the current mission, in
/// its 0x18-byte frame: `[gp+0x3DC]` = gameplay +0x260 (the variant table;
/// kind 3 picks one of three by `rand() % 3`), the mission tables
/// (8004D1AC), the script tables (80053E08), the mission state (80053BAC),
/// the HUD (8002CBCC), the message queue (80056198), 8004CE9C, 8005B648, the
/// result block (8004856C), the entities (80038A6C), the objects (80038AD4),
/// the links (80045FF0), the player object (80038AF4 on gameplay +0x28),
/// JP 80048BDC and 80051824. PAL also stored the mission number byte and
/// called PAL-only 8005AC20 before 8004CE9C's counterpart.
pub fn initialize_38818(m: &mut Machine) {
    const SIZE: u32 = 0x18;
    m.push_frame(SIZE);
    let gameplay = m.gw(GAMEPLAY);
    let variants = m.lw(gameplay.wrapping_add(0x260));
    m.set_gw(0x3dc, variants);
    if m.lw(variants) == 3 {
        let value = sdk::rand(m) as i32;
        let quotient = ((i64::from(value) * 0x5555_5556) >> 32) as i32 - (value >> 31);
        let choice = value.wrapping_sub(quotient.wrapping_mul(3)) as u32;
        let base = m.gw(0x3dc);
        let offset = m.lw(choice.wrapping_mul(4).wrapping_add(base).wrapping_add(4));
        m.set_gw(0x3dc, base.wrapping_add(offset));
    }
    load_mission_tables_8004bbb8(m);
    load_script_tables_800527c8(m);
    start_mission_state_80052574(m);
    reset_hud_8002c7a4(m);
    reset_message_queue_80054b54(m);
    dispatch::call(m, 0x8004_ce9c, [0; 4]); // hud.rs: no message shown
    m.sb(GP + 0x4e8, 0); // 8005B648
    reset_result_800477c8(m);
    initialize_entities_80038758(m);
    initialize_objects_8004cdb0(m); // via 80038AD4
    reset_links_800453b8(m);
    let gameplay = m.gw(GAMEPLAY);
    let object = m.lw(gameplay.wrapping_add(0x28));
    bind_player_object_800387e0(m, object);
    dispatch::call(m, 0x8004_8bdc, [0; 4]);
    common::reset_800501f8(m);
    m.pop_frame(SIZE);
}

/// 80053BAC (JP; PAL 80052574, near, `ins:2 op:1`): allocate and clear the
/// mission state block (`[gp+0x340]`, 0x40 bytes; +0x2B = 0xFF, +0x2C =
/// stream of the mission index: 80056964 on context half +0x38C, PAL the
/// gp byte +0x4C0) and run the mission's initialization script
/// `[[gp+0x338]]`.
pub fn start_mission_state_80052574(m: &mut Machine) {
    const SIZE: u32 = 0x18;
    m.push_frame(SIZE);
    let resources = m.lw(RESOURCES);
    let offset = m.lw(resources.wrapping_add(0x10));
    m.set_gw(0x33c, resources.wrapping_add(offset));
    let state = arena::alloc(m, 0x40);
    m.set_gw(0x340, state);
    m.sb(state.wrapping_add(0xa), 0);
    m.sw(state, 0);
    m.sw(state.wrapping_add(4), 0);
    m.sh(state.wrapping_add(8), 0);
    m.sb(state.wrapping_add(0xb), 0);
    m.sb(state.wrapping_add(0x1c), 0);
    m.sw(state.wrapping_add(0xc), 0);
    m.sw(state.wrapping_add(0x10), 0);
    m.sw(state.wrapping_add(0x14), 0);
    m.sb(state.wrapping_add(0x1d), 0);
    m.sh(state.wrapping_add(0x18), 0);
    m.sh(state.wrapping_add(0x1a), 0);
    for offset in [
        0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x3d, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a,
    ] {
        let state = m.gw(0x340);
        m.sb(state.wrapping_add(offset), 0);
    }
    let state = m.gw(0x340);
    m.sb(state.wrapping_add(0x2b), 0xff);
    let context = m.gw(CONTEXT);
    let index = m.lh(context.wrapping_add(0x38c)) as i32 as u32;
    let stream = stream_of_800550d8(m, index);
    let state = m.gw(0x340);
    m.sb(state.wrapping_add(0x2c), stream as u8);
    for offset in [0x2d, 0x2e, 0x2f, 0x30, 0x31, 0x34] {
        let state = m.gw(0x340);
        m.sb(state.wrapping_add(offset), 0);
    }
    let state = m.gw(0x340);
    m.sh(state.wrapping_add(0x32), 0);
    for offset in [0x35, 0x36, 0x37, 0x3c] {
        let state = m.gw(0x340);
        m.sb(state.wrapping_add(offset), 0);
    }
    let state = m.gw(0x340);
    m.sh(state.wrapping_add(0x38), 0);
    m.sh(state.wrapping_add(0x3a), 0);
    m.sb(state.wrapping_add(0x3e), 0);
    let state = m.gw(0x340);
    m.sb(state.wrapping_add(0x3f), 0);
    let scripts = m.gw(0x338);
    let script = m.lw(scripts);
    script::interpret(m, script);
    m.pop_frame(SIZE);
}

/// 80056198 (JP; PAL 80054B54, changed), in its 0x18-byte frame: the
/// message list of the mission resource (`[0x800BCF50]` + word +0x14: a
/// count `n` and `n` words): `gp+0x4C0` = the words, `gp+0x4B0` = `n`,
/// `gp+0x4B8` = the end of the words (PAL resolved per-language tables into
/// `gp+0x4A0/0x4B0/0x4B4`); the four-entry message queue `[gp+0x4AC]` (0x18
/// bytes: +0 = 0xFF, bytes +0x14.. = 0xFF, halves +4.. and +0xC.. = 0), then
/// JP 800566F0 (the message buffer `[gp+0x4BC]`) and `gp+0x4B4` = 0.
pub fn reset_message_queue_80054b54(m: &mut Machine) {
    const SIZE: u32 = 0x18;
    m.push_frame(SIZE);
    let resources = m.lw(RESOURCES);
    let offset = m.lw(resources.wrapping_add(0x14));
    let tables = resources.wrapping_add(offset);
    let count = m.lw(tables);
    let words = tables.wrapping_add(4);
    m.set_gw(0x4c0, words);
    m.set_gw(0x4b0, count);
    m.set_gw(0x4b8, words.wrapping_add(count << 2));
    let queue = arena::alloc(m, 0x18);
    m.set_gw(0x4ac, queue);
    m.sw(queue, 0xff);
    for i in 0..4 {
        let queue = m.gw(0x4ac);
        m.sb(queue.wrapping_add(i).wrapping_add(0x14), 0xff);
        let queue = m.gw(0x4ac);
        let at = queue.wrapping_add(i * 2);
        m.sh(at.wrapping_add(0xc), 0);
        m.sh(at.wrapping_add(4), 0);
    }
    dispatch::call(m, 0x8005_66f0, [4, 0xff, 0, 0]);
    m.set_gw(0x4b4, 0);
    m.pop_frame(SIZE);
}

/// 8004856C (JP; PAL 800477C8, changed): reset the context result block
/// (+0x3B8; PAL +0x3BC) for the mission: bytes +0/+1/+0xA = 0, halves +2/+8
/// = 0, +4/+5 = the mission index byte (context +0x38C), +0xB = 4, context
/// byte +0x956 = 0, then the player's map cell from entity 0's position
/// (`((x >> 12) + 0x40000) >> 11` clamped to 0..0xFF, for x and z) at
/// `+0xC + 2 * [+8]`. PAL restored the saved score of missions 0x36, 0x3A
/// and 0x3B instead of recording the cell.
pub fn reset_result_800477c8(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let block = context.wrapping_add(0x3b8);
    m.sb(block, 0);
    m.sb(block.wrapping_add(1), 0);
    let context = m.gw(CONTEXT);
    m.sh(block.wrapping_add(2), 0);
    let value = m.lbu(context.wrapping_add(0x38c));
    m.sb(block.wrapping_add(4), value);
    let context = m.gw(CONTEXT);
    let value = m.lbu(context.wrapping_add(0x38c));
    m.sb(block.wrapping_add(0xb), 4);
    m.sb(block.wrapping_add(0xa), 0);
    m.sb(block.wrapping_add(5), value);
    let context = m.gw(CONTEXT);
    m.sh(block.wrapping_add(8), 0);
    m.sb(context.wrapping_add(0x956), 0);
    let player = entity(m, 0);
    for (i, field) in [8u32, 0x10].into_iter().enumerate() {
        let position = m.lw(player.wrapping_add(field)) as i32;
        let cell = ((position >> 12).wrapping_add(0x4_0000) >> 11).clamp(0, 0xff);
        let index = m.lh(block.wrapping_add(8)) as i32 as u32;
        m.sb(
            block
                .wrapping_add(index.wrapping_mul(2))
                .wrapping_add(0xc + i as u32),
            cell as u8,
        );
    }
}
