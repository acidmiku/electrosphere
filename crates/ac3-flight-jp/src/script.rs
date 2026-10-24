//! Mission script interpreter 80054EBC with its condition blocks
//! (80055C4C..PAL 80054AA4) and byte readers, the mission state step 80037E50,
//! and the resident routines the opcodes reach.
//!
//! A script is a byte string of opcodes `0x01..0x4F` with inline operands,
//! terminated by a zero byte; `interpret` returns the address of that zero.
//! Opcodes `0x50..0xFF` and the unused table entries are one-byte no-ops.
//! Condition opcodes run a nested block (`[skip][else] then.. else..`)
//! through a recursive `interpret` and continue after the skip.
//!
//! The mission state block is `[gp+0x340]` (0x40 bytes, allocated by
//! 80053BAC). Translated from `analysis/main/disassembly.txt`; validated by
//! `tests/script.rs`.
use crate::common::{self, CONTEXT, ENTITIES, GAMEPLAY};
use crate::dispatch;
use crate::machine::{GP, Machine, SmallData};
use crate::{sdk, shared};

// Former names of routines now in `shared.rs`, still used by `mainloop.rs`
// (re-exports, not copies).
pub use crate::shared::{
    current_cd_file as current_stream_8002c6a8, release_channel as release_channel_8002d980,
    reset_channel as reset_channel_8002e198,
};

/// `[gp+0x340]`: the 0x40-byte mission state block.
pub const MISSION: u32 = 0x340;
/// Byte: current mission number (set by 80038B2C from `PAL 0x800B0210[ctx+0x390]`).
pub const MISSION_ID: u32 = 0x800b_d02c;
/// 64 entity flag words (entity `i` at `+i*4`; objects at `+0x80`).
pub const ENTITY_FLAGS: u32 = 0x800c_6fd0;
/// 64 per-entity auxiliary record pointers.
pub const ENTITY_AUX: u32 = 0x800c_67a0;
/// 32 entity display records of 12 bytes.
pub const ENTITY_DISPLAY: u32 = 0x800c_6a60;
/// Lock-on block; byte `+0xE` is the locked entity (0xFF none).
pub const LOCK: u32 = 0x800c_6d60;
/// 24 sound channels of 20 bytes.
pub const SOUND_CHANNELS: u32 = 0x800c_5830;
/// Sound definitions of 20 bytes.
pub const SOUND_DEFINITIONS: u32 = 0x800c_5a10;
/// 32 queued sound events of 4 bytes (`[gp+0x325]` is the write index).
pub const SOUND_EVENTS: u32 = 0x800c_6570;

fn mission(m: &mut Machine) -> u32 {
    m.gw(MISSION)
}
fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw(table.wrapping_add(index.wrapping_mul(4)))
}
fn or_word(m: &mut Machine, address: u32, bits: u32) {
    let value = m.lw(address);
    m.sw(address, value | bits);
}
fn and_word(m: &mut Machine, address: u32, mask: u32) {
    let value = m.lw(address);
    m.sw(address, value & mask);
}
fn flag_word(index: u32) -> u32 {
    ENTITY_FLAGS.wrapping_add(index.wrapping_mul(4))
}

// ---- interpreter ----------------------------------------------------------------

// JP-REVIEW(R0433): near function: PAL 80053704 -> JP 80054EBC, call-:3 del:22 imm:12 ins:2 op:1 reg:8, 45 instructions differ (whole routine)
/// 80054EBC: run the script at `script` until its zero byte; returns the
/// address of that byte.
// JP-PATCH: replaced by `crate::jp::res_p2_script::interpret` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn interpret(m: &mut Machine, script: u32) -> u32 {
    crate::jp::res_p2_script::interpret(m, script)
}

/// 80056148: little-endian signed halfword at any alignment.
pub fn read_i16_80054b04(m: &mut Machine, at: u32) -> i16 {
    let low = m.lbu(at);
    let high = m.lbu(at.wrapping_add(1));
    i16::from_le_bytes([low, high])
}

/// 80056164: little-endian word at any alignment.
pub fn read_u32_80054b20(m: &mut Machine, at: u32) -> u32 {
    let bytes: [u8; 4] = std::array::from_fn(|i| m.lbu(at.wrapping_add(i as u32)));
    u32::from_le_bytes(bytes)
}

// ---- condition blocks ----------------------------------------------------------------

/// Shared tail of the condition blocks. `at` addresses the else-offset byte;
/// the then-block starts right after it, the else-block at `at + offset`.
/// `frame` is the handler's stack frame size, so the nested interpreter
/// frame sits at its original address.
fn run_block(m: &mut Machine, at: u32, otherwise: bool, frame: u32) {
    let start = if otherwise {
        at.wrapping_add(u32::from(m.lbu(at)))
    } else {
        at.wrapping_add(1)
    };
    m.push_frame(frame);
    interpret(m, start);
    m.pop_frame(frame);
}

/// Continuation and else-offset address for `[skip][else]` at `at`.
fn block_header(m: &mut Machine, at: u32) -> (u32, u32) {
    let skip = u32::from(m.lbu(at));
    (at.wrapping_add(skip), at.wrapping_add(1))
}

/// 80055C4C `[u32 limit][skip][else]`: then-block when `limit >= value`
/// (signed). Opcode 0x0B passes mission word +0x10, opcode 0x3C word +0xC.
pub fn block_word_below_800544e4(m: &mut Machine, at: u32, value: u32) -> u32 {
    let limit = read_u32_80054b20(m, at);
    let (next, at) = block_header(m, at.wrapping_add(4));
    run_block(m, at, (limit as i32) < (value as i32), 0x20);
    next
}

/// 80055CBC `[u32 limit][skip][else]`: then-block when mission word +4 >=
/// `limit` (signed).
pub fn block_mission_word4_80054554(m: &mut Machine, at: u32) -> u32 {
    let limit = read_u32_80054b20(m, at);
    let (next, at) = block_header(m, at.wrapping_add(4));
    let state = mission(m);
    let value = m.lw(state.wrapping_add(4));
    run_block(m, at, (value as i32) < (limit as i32), 0x20);
    next
}

/// 80055D2C `[value][skip][else]`: then-block when mission byte +0x22 >= value.
pub fn block_byte22_800545c4(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let state = mission(m);
    let current = m.lbu(state.wrapping_add(0x22));
    run_block(m, at, current < value, 0x18);
    next
}

/// 80055D94 `[value][skip][else]`: then-block when value >= mission byte +0x21.
pub fn block_byte21_8005462c(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let state = mission(m);
    let current = m.lbu(state.wrapping_add(0x21));
    run_block(m, at, value < current, 0x18);
    next
}

/// 80055DFC `[value][skip][else]`: then-block when mission byte +0x20 >= value.
pub fn block_byte20_80054694(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let state = mission(m);
    let current = m.lbu(state.wrapping_add(0x20));
    run_block(m, at, current < value, 0x18);
    next
}

/// 80055E64 `[u32 ignored][skip][else]`: then-block when mission byte +0x2F
/// is 1. The word is read and discarded.
pub fn block_byte2f_800546fc(m: &mut Machine, at: u32) -> u32 {
    let _ = read_u32_80054b20(m, at);
    let (next, at) = block_header(m, at.wrapping_add(4));
    let state = mission(m);
    let value = m.lbu(state.wrapping_add(0x2f));
    run_block(m, at, value != 1, 0x20);
    next
}

/// 80055ED8 `[skip][else]`: then-block when mission byte +0x2D < +0x2E.
pub fn block_bytes2d2e_80054770(m: &mut Machine, at: u32) -> u32 {
    let (next, at) = block_header(m, at);
    let state = mission(m);
    let high = m.lbu(state.wrapping_add(0x2e));
    let low = m.lbu(state.wrapping_add(0x2d));
    run_block(m, at, low >= high, 0x18);
    next
}

// JP-REVIEW(R0435): near function: PAL 800547D4 -> JP 80055F3C, imm:1, 1 instructions differ (whole routine)
/// 80055F3C `[index][skip][else]`: then-block when context byte
/// `+0xD50+index` is nonzero.
// JP-PATCH: replaced by `crate::jp::res_p2_script::block_context_flag_800547d4` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_context_flag_800547d4(m: &mut Machine, at: u32) -> u32 {
    crate::jp::res_p2_script::block_context_flag_800547d4(m, at)
}

/// 80055FA4 `[skip][else]`: then-block when `rand()` is odd.
pub fn block_random_8005483c(m: &mut Machine, at: u32) -> u32 {
    let (next, at) = block_header(m, at);
    let value = sdk::rand(m);
    run_block(m, at, value & 1 == 0, 0x20);
    next
}

/// 80056004 `[entity][value][skip][else]`: then-block when the entity's
/// signed halfword +0x4E >= value.
pub fn block_entity_value_8005489c(m: &mut Machine, at: u32) -> u32 {
    let index = u32::from(m.lbu(at));
    let value = m.lbu(at.wrapping_add(1));
    let (next, at) = block_header(m, at.wrapping_add(2));
    let object = entity(m, index);
    let current = m.lh(object.wrapping_add(0x4e));
    run_block(m, at, i32::from(current) < i32::from(value), 0x18);
    next
}

/// 80056080 `[value][skip][else]`: then-block when gameplay byte +8 == value.
pub fn block_gameplay_byte8_80054918(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let gameplay = m.gw(GAMEPLAY);
    let current = m.lbu(gameplay.wrapping_add(8));
    run_block(m, at, value != current, 0x18);
    next
}

// JP-REVIEW(R0436): near function: PAL 8005497C -> JP 800560E4, imm:1, 1 instructions differ (whole routine)
/// 800560E4 `[value][skip][else]`: then-block when context byte +0x398 == value.
// JP-PATCH: replaced by `crate::jp::res_p2_script::block_context_byte398_8005497c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn block_context_byte398_8005497c(m: &mut Machine, at: u32) -> u32 {
    crate::jp::res_p2_script::block_context_byte398_8005497c(m, at)
}

// ---- mission state ------------------------------------------------------------------

/// 80054DBC: mission rank 1 (best) .. 5 from the evaluation mode at mission
/// byte +0x34 (0xFF → 5, 0 → 4, 1 → kill/time counts, 2 → word +0xC
/// against halfwords +0x38/+0x3A, 4 → word +4 against them, else 4).
pub fn rank_80053604(m: &mut Machine) -> u32 {
    let state = mission(m);
    let field = |m: &mut Machine, offset: u32| i32::from(m.lbu(state.wrapping_add(offset)));
    let mode = m.lbu(state.wrapping_add(0x34));
    match mode {
        0xff => 5,
        0 => 4,
        1 => {
            let counted = field(m, 0xb) + field(m, 0x21);
            if field(m, 0x37) < counted {
                return 3;
            }
            let total = field(m, 0x2e) + field(m, 0x2d);
            if total < field(m, 0x36) {
                return 3;
            }
            if total < field(m, 0x35) { 2 } else { 1 }
        }
        2 => {
            let value = m.lw(state.wrapping_add(0xc)) as i32;
            let first = i32::from(m.lhu(state.wrapping_add(0x38)));
            if value < first {
                return 1;
            }
            let second = i32::from(m.lhu(state.wrapping_add(0x3a)));
            if value < second { 2 } else { 3 }
        }
        4 => {
            let value = m.lw(state.wrapping_add(4)) as i32;
            let first = i32::from(m.lhu(state.wrapping_add(0x38)));
            if value >= first {
                return 1;
            }
            let second = i32::from(m.lhu(state.wrapping_add(0x3a)));
            if value < second { 3 } else { 2 }
        }
        _ => 4,
    }
}

// JP-REVIEW(R0439): near function: PAL 80037B24 -> JP 80037E50, del:7 imm:11 ins:4 op:3, 25 instructions differ (whole routine)
/// 80037E50: advance the mission state machine and request the resident
/// transition for the current phase (mission byte +0x29) or pending flag
/// (byte +0x2A bits 0..2).
// JP-PATCH: replaced by `crate::jp::res_p2_script::step_80037b24` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn step_80037b24(m: &mut Machine) {
    crate::jp::res_p2_script::step_80037b24(m)
}

// JP-REVIEW(R0441): near function: PAL 80037D34 -> JP 80038054, imm:1, 1 instructions differ (whole routine)
/// 80038054: context word +0xF7C = byte `0x800B0DA0[id * 4 + index]`.
pub fn set_result_80037d34(m: &mut Machine, id: u8, index: u8) {
    let at = 0x800b_0da0u32
        .wrapping_add(u32::from(id) * 4)
        .wrapping_add(u32::from(index));
    let context = m.gw(CONTEXT);
    let value = m.lbu(at);
    m.sw(context.wrapping_add(0xf7c), u32::from(value));
}

/// 80079924: reattach the player model to the gameplay object (+0x28) with
/// flags 0x2000 and clear the gameplay pause bit through 80035740(0).
pub fn restore_player_model_800782b4(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(ENTITIES);
    let object = m.lw(gameplay.wrapping_add(0x28));
    let player = m.lw(table);
    shared::set_camera_target(m, object, 0x2000, player);
    // 80035740 belongs to the 80032754 tail tree.
    dispatch::call(m, 0x8003_5740, [0, 0, 0, 0]);
}

// ---- sound -----------------------------------------------------------------------

/// 8002F294: append event `{kind, -, sound}` at the write index `gp+0x325`
/// (modulo 32).
pub fn queue_sound_event_8002ee78(m: &mut Machine, sound: u32, kind: u32) {
    let index = u32::from(m.lbu(GP + 0x325));
    let at = SOUND_EVENTS + index * 4;
    m.sh(at + 2, sound as u16);
    m.sb(at, kind as u8);
    let index = m.lbu(GP + 0x325);
    m.sb(GP + 0x325, index.wrapping_add(1) & 0x1f);
}

/// 8002DB4C: start sound effect `sound` on a free channel (definitions
/// 0x2E..0x31 first try 8002DBE0). Returns the channel or 0xFF.
pub fn play_sound_8002d724(m: &mut Machine, sound: u32) -> u32 {
    if sound.wrapping_sub(0x2e) < 4 {
        play_sound_reuse_8002d7b8(m, sound);
    }
    let fixed = SOUND_DEFINITIONS.wrapping_add(sound.wrapping_mul(20));
    let mut channel = u32::from(m.lbu(fixed));
    if channel == 0xff {
        channel = claim_channel_8002d910(m);
        if channel == 0xff {
            return 0xff;
        }
    }
    start_channel_8002e0f8(m, sound, channel);
    channel
}

/// 8002DBE0: start `sound` on its fixed channel, the channel already
/// playing it, or a free one. Returns the channel or 0xFF.
pub fn play_sound_reuse_8002d7b8(m: &mut Machine, sound: u32) -> u32 {
    let fixed = SOUND_DEFINITIONS.wrapping_add(sound.wrapping_mul(20));
    let mut channel = u32::from(m.lbu(fixed));
    if channel == 0xff {
        channel = find_channel_8002d8d0(m, sound);
        if channel == 0xff {
            channel = claim_channel_8002d910(m);
            if channel == 0xff {
                return 0xff;
            }
        }
    }
    start_channel_8002e0f8(m, sound, channel);
    channel
}

/// 8002DCF8: channel 12..23 currently assigned `sound`, else 0xFF.
pub fn find_channel_8002d8d0(m: &mut Machine, sound: u32) -> u32 {
    for channel in 0xc..0x18 {
        if u32::from(m.lbu(SOUND_CHANNELS + channel * 20)) == sound {
            return channel;
        }
    }
    0xff
}

/// 8002DD38: claim the first idle channel 12..23 (state byte +3 := 2).
pub fn claim_channel_8002d910(m: &mut Machine) -> u32 {
    for channel in 0xc..0x18 {
        let at = SOUND_CHANNELS + channel * 20 + 3;
        if m.lbu(at) == 0 {
            m.sb(at, 2);
            return channel;
        }
    }
    0xff
}

/// 8002E514: load definition `sound` into `channel`; returns the channel
/// (0xFF is ignored).
pub fn start_channel_8002e0f8(m: &mut Machine, sound: u32, channel: u32) -> u32 {
    if channel == 0xff {
        return 0xff;
    }
    let at = SOUND_CHANNELS.wrapping_add(channel.wrapping_mul(20));
    m.sb(at.wrapping_add(1), 1);
    m.sb(at.wrapping_add(3), 1);
    let definition = SOUND_DEFINITIONS.wrapping_add(sound.wrapping_mul(20));
    m.sb(at.wrapping_add(2), 0x10);
    m.sb(at, sound as u8);
    let value = m.lhu(definition.wrapping_add(4));
    m.sh(at.wrapping_add(4), value);
    m.sh(at.wrapping_add(8), value);
    let value = m.lhu(definition.wrapping_add(6));
    m.sh(at.wrapping_add(6), value);
    m.sh(at.wrapping_add(0xa), value);
    let value = m.lhu(definition.wrapping_add(8));
    m.sb(at.wrapping_add(0x10), 0x80);
    m.sb(at.wrapping_add(0x11), 0x80);
    m.sh(at.wrapping_add(0x12), 0);
    m.sh(at.wrapping_add(0xc), value);
    m.sh(at.wrapping_add(0xe), value);
    channel
}

// ---- streamed speech (CD stream block at [gp+0x48]) -----------------------------------------

/// 8002C8D8: queue stream request `{3, 0, a0, a1, -, a2}` at the write index.
pub fn queue_stream_request_8002c4b0(m: &mut Machine, data: u32, second: u32, third: u32) {
    let stream = m.gw(0x48);
    let index = m.lw(stream.wrapping_add(4));
    m.sw(stream.wrapping_add(4), index.wrapping_add(1));
    let entry = stream.wrapping_add((index & 0xf) * 20 + 8);
    m.sb(entry, 3);
    m.sh(entry.wrapping_add(2), 0);
    m.sw(entry.wrapping_add(4), data);
    m.sw(entry.wrapping_add(8), second);
    m.sw(entry.wrapping_add(0x10), third);
    m.sb(entry.wrapping_add(1), 0);
}

/// 8005697C: play speech stream `index` (table 0x800B2334) unless it is
/// already playing; 0xFF only cancels. Returns `index`.
pub fn play_stream_800550f0(m: &mut Machine, index: u32) -> u32 {
    if index == 0xff {
        shared::cancel_stream_requests(m);
        return 0xff;
    }
    let slot = 0x800b_2334u32.wrapping_add(index.wrapping_mul(4));
    let entry = m.lw(slot);
    let wanted = m.lhu(entry) & 0x7ff;
    let playing = shared::current_cd_file(m) & 0x7ff;
    if u32::from(wanted) != playing {
        shared::cancel_stream_requests(m);
        let entry = m.lw(slot);
        queue_stream_request_8002c4b0(m, entry, 0, 0);
    }
    index
}

// ---- entities, links and groups -------------------------------------------------------------

/// 8004C2C8: enable group `index` (words `[gp+0x3A4]`, 20-byte records
/// `[gp+0x398]` naming two entities that gain flags 0x200000/0x400000).
pub fn enable_group_8004ab78(m: &mut Machine, index: u32) {
    let groups = m.gw(0x3a4);
    or_word(m, groups.wrapping_add(index.wrapping_mul(4)), 1);
    let records = m.gw(0x398);
    let record = records.wrapping_add(index.wrapping_mul(20));
    let first = u32::from(m.lbu(record.wrapping_add(2)));
    or_word(m, flag_word(first), 0x20_0000);
    let second = u32::from(m.lbu(record.wrapping_add(3)));
    or_word(m, flag_word(second), 0x40_0000);
}

/// 8004C338: disable group `index` if enabled (inverse of 8004C2C8).
pub fn disable_group_8004abe8(m: &mut Machine, index: u32) {
    let groups = m.gw(0x3a4);
    let at = groups.wrapping_add(index.wrapping_mul(4));
    let word = m.lw(at);
    if word & 1 == 0 {
        return;
    }
    m.sw(at, word & !1);
    let records = m.gw(0x398);
    let record = records.wrapping_add(index.wrapping_mul(20));
    let first = u32::from(m.lbu(record.wrapping_add(2)));
    and_word(m, flag_word(first), 0xffdf_ffff);
    let second = u32::from(m.lbu(record.wrapping_add(3)));
    and_word(m, flag_word(second), 0xffbf_ffff);
}

/// 8005F0E4: gameplay word bit 2 set (`on != 0`) or cleared.
pub fn set_gameplay_bit2_8005d918(m: &mut Machine, on: u32) {
    let gameplay = m.gw(GAMEPLAY);
    if on != 0 {
        or_word(m, gameplay, 4);
    } else {
        and_word(m, gameplay, !4);
    }
}

/// 80056234: put message `{slot byte, value, timer 0}` into the first free
/// of four queue entries (`[gp+0x4AC]`: bytes +0x14, halfwords +0xC/+4).
pub fn queue_message_80054c20(m: &mut Machine, slot: u32, value: u32) {
    for i in 0..4 {
        let queue = m.gw(0x4ac);
        let at = queue.wrapping_add(i);
        if m.lbu(at.wrapping_add(0x14)) == 0xff {
            m.sb(at.wrapping_add(0x14), slot as u8);
            let queue = m.gw(0x4ac);
            let at = queue.wrapping_add(i * 2);
            m.sh(at.wrapping_add(0xc), value as u16);
            m.sh(at.wrapping_add(4), 0);
            return;
        }
    }
}

// ---- messages --------------------------------------------------------------------------

// JP-REVIEW(R0444): changed function: PAL 8004B760 -> JP 8004CEB0, call+:3 call-:2 del:49 ins:35 op:13 reg:14, 111 instructions differ (whole routine)
/// 8004CEB0: show text message `message` (0xFF none): set up the glyph
/// list in the primitive control block `[gp+0x478]` (+0x110 count, +0x114
/// /+0x118 layout, +0x11C current message, 8-byte glyphs from +8).
/// Messages 6 and 7 also play sound 0x66/0x67.
pub fn show_message_8004b760(m: &mut Machine, message: u32) {
    if message == 0xff {
        return;
    }
    let control = m.gw(0x478);
    if m.lw(control.wrapping_add(0x11c)) == message {
        return;
    }
    let resources = m.lw(0x800b_cf50);
    m.sw(control.wrapping_add(0x11c), message);
    let offset = m.lw(resources.wrapping_add(0x18));
    let texts = resources.wrapping_add(offset);
    let header = m.lw(texts);
    let header = texts.wrapping_add(header);
    let context = m.gw(CONTEXT);
    let language = m.lw(context.wrapping_add(0x38c));
    let entry = message.wrapping_mul(4).wrapping_add(header);
    let layout = u32::from(m.lbu(entry.wrapping_add(1)));
    let table = m.lw(language.wrapping_mul(4).wrapping_add(texts).wrapping_add(4));
    m.sw(control.wrapping_add(0x114), layout);
    let layout = u32::from(m.lbu(entry.wrapping_add(2)));
    m.sw(control.wrapping_add(0x110), 0);
    let strings = texts.wrapping_add(table);
    m.sw(control.wrapping_add(0x118), layout);
    let text_base = strings.wrapping_add(0x294);
    if message.wrapping_sub(6) < 2 {
        let sound = if message == 7 { 0x67 } else { 0x66 };
        play_sound_8002d724(m, sound);
    }
    let line = message.wrapping_mul(12).wrapping_add(strings);
    // JP-REVIEW(R0445): unmapped data in const `LINES`: data 800C5598: no data map covers it
    const LINES: u32 = crate::jp_review::poison(445);
    let value = m.lbu(line.wrapping_add(4));
    m.sh(LINES, u16::from(value));
    let value = m.lbu(line.wrapping_add(5));
    m.sh(LINES + 2, u16::from(value));
    let offset = m.lh(line.wrapping_add(6)) as i32 as u32;
    m.sw(LINES + 4, text_base.wrapping_add(offset));
    layout_text_8004b920(m, LINES);
    if m.lw(line) == 2 {
        let value = m.lbu(line.wrapping_add(8));
        m.sh(LINES + 8, u16::from(value));
        let value = m.lbu(line.wrapping_add(9));
        m.sh(LINES + 0xa, u16::from(value));
        let offset = m.lh(line.wrapping_add(0xa)) as i32 as u32;
        m.sw(LINES + 0xc, text_base.wrapping_add(offset));
        layout_text_8004b920(m, LINES + 8);
    }
    let control = m.gw(0x478);
    let mut index = m.lw(control.wrapping_add(0x110));
    while (index as i32) < 0x20 {
        let control = m.gw(0x478);
        m.sb(control.wrapping_add(index << 3).wrapping_add(8), 0);
        index = index.wrapping_add(1);
    }
    let mut control = m.gw(0x478);
    if (m.lw(control.wrapping_add(0x110)) as i32) <= 0 {
        return;
    }
    let mut index = 0u32;
    loop {
        m.sb(control.wrapping_add(index << 3).wrapping_add(8), 2);
        control = m.gw(0x478);
        index = index.wrapping_add(1);
        if (index as i32) >= (m.lw(control.wrapping_add(0x110)) as i32) {
            break;
        }
    }
}

/// PAL 8004B920: append glyphs for the text line `{u16 x, u16 y, u32 text}` at
/// `line` (widths from 0x800B1914; accented letters add an accent glyph).
#[allow(unused_variables)]
pub fn layout_text_8004b920(m: &mut Machine, line: u32) {
    crate::jp_review::pal_only("layout_text_8004b920: PAL 8004B920")
}

/// Every routine translated here, by original address (`a0..a3` in, `v0`
/// out; routines without a result return 0).
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let _ = a3;
    Some(match target {
        0x8005_4ebc => interpret(m, a0),
        0x8003_7e50 => {
            step_80037b24(m);
            0
        }
        0x8005_6148 => read_i16_80054b04(m, a0) as u32,
        0x8005_6164 => read_u32_80054b20(m, a0),
        0x8005_5c4c => block_word_below_800544e4(m, a0, a1),
        0x8005_5cbc => block_mission_word4_80054554(m, a0),
        0x8005_5d2c => block_byte22_800545c4(m, a0),
        0x8005_5d94 => block_byte21_8005462c(m, a0),
        0x8005_5dfc => block_byte20_80054694(m, a0),
        0x8005_5e64 => block_byte2f_800546fc(m, a0),
        0x8005_5ed8 => block_bytes2d2e_80054770(m, a0),
        0x8005_5f3c => block_context_flag_800547d4(m, a0),
        0x8005_5fa4 => block_random_8005483c(m, a0),
        0x8005_6004 => block_entity_value_8005489c(m, a0),
        0x8005_6080 => block_gameplay_byte8_80054918(m, a0),
        0x8005_60e4 => block_context_byte398_8005497c(m, a0),
        0x8005_4dbc => rank_80053604(m),
        0x8007_9924 => {
            restore_player_model_800782b4(m);
            0
        }
        0x8002_f294 => {
            queue_sound_event_8002ee78(m, a0, a1);
            0
        }
        0x8002_db4c => play_sound_8002d724(m, a0),
        0x8002_dbe0 => play_sound_reuse_8002d7b8(m, a0),
        0x8002_dcf8 => find_channel_8002d8d0(m, a0),
        0x8002_dd38 => claim_channel_8002d910(m),
        0x8002_e514 => start_channel_8002e0f8(m, a0, a1),
        0x8002_c8d8 => {
            queue_stream_request_8002c4b0(m, a0, a1, a2);
            0
        }
        0x8005_697c => play_stream_800550f0(m, a0),
        0x8004_c2c8 => {
            enable_group_8004ab78(m, a0);
            0
        }
        0x8004_c338 => {
            disable_group_8004abe8(m, a0);
            0
        }
        0x8005_f0e4 => {
            set_gameplay_bit2_8005d918(m, a0);
            0
        }
        0x8005_6234 => {
            queue_message_80054c20(m, a0, a1);
            0
        }
        _ => return None,
    })
}
