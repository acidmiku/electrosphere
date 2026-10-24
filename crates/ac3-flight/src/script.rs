//! Mission script interpreter 80053704 with its condition blocks
//! (800544E4..80054AA4) and byte readers, the mission state step 80037B24,
//! and the resident routines the opcodes reach.
//!
//! A script is a byte string of opcodes `0x01..0x4F` with inline operands,
//! terminated by a zero byte; `interpret` returns the address of that zero.
//! Opcodes `0x50..0xFF` and the unused table entries are one-byte no-ops.
//! Condition opcodes run a nested block (`[skip][else] then.. else..`)
//! through a recursive `interpret` and continue after the skip.
//!
//! The mission state block is `[gp+0x330]` (0x40 bytes, allocated by
//! 80052574). Translated from `analysis/main/disassembly.txt`; validated by
//! `tests/script.rs`.
use crate::common::{self, CONTEXT, ENTITIES, GAMEPLAY};
use crate::dispatch;
use crate::machine::{Fault, GP, Machine, SmallData};
use crate::{sdk, shared};

// Former names of routines now in `shared.rs`, still used by `mainloop.rs`
// (re-exports, not copies).
pub use crate::shared::{
    current_cd_file as current_stream_8002c6a8, release_channel as release_channel_8002d980,
    reset_channel as reset_channel_8002e198,
};

/// `[gp+0x330]`: the 0x40-byte mission state block.
pub const MISSION: u32 = 0x330;
/// Byte: current mission number (set by 80038818 from `0x800B0210[ctx+0x390]`).
pub const MISSION_ID: u32 = 0x800b_aeb4;
/// 64 entity flag words (entity `i` at `+i*4`; objects at `+0x80`).
pub const ENTITY_FLAGS: u32 = 0x800c_4d98;
/// 64 per-entity auxiliary record pointers.
pub const ENTITY_AUX: u32 = 0x800c_4568;
/// 32 entity display records of 12 bytes.
pub const ENTITY_DISPLAY: u32 = 0x800c_4828;
/// Lock-on block; byte `+0xE` is the locked entity (0xFF none).
pub const LOCK: u32 = 0x800c_4b28;
/// 24 sound channels of 20 bytes.
pub const SOUND_CHANNELS: u32 = 0x800c_35f8;
/// Sound definitions of 20 bytes.
pub const SOUND_DEFINITIONS: u32 = 0x800c_37d8;
/// 32 queued sound events of 4 bytes (`[gp+0x315]` is the write index).
pub const SOUND_EVENTS: u32 = 0x800c_4338;

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
fn set_mission_byte(m: &mut Machine, offset: u32, value: u8) {
    let state = mission(m);
    m.sb(state.wrapping_add(offset), value);
}
fn or_mission_byte(m: &mut Machine, offset: u32, bits: u8) {
    let state = mission(m);
    let at = state.wrapping_add(offset);
    let value = m.lbu(at);
    m.sb(at, value | bits);
}
fn flag_word(index: u32) -> u32 {
    ENTITY_FLAGS.wrapping_add(index.wrapping_mul(4))
}

// ---- interpreter ----------------------------------------------------------------

/// 80053704: run the script at `script` until its zero byte; returns the
/// address of that byte.
pub fn interpret(m: &mut Machine, script: u32) -> u32 {
    // The 0x50-byte frame is guest memory: opcode 0x25 builds its camera
    // frame at +0x10 and passes that address on (the original also copies
    // the never-written pads +0x1C/+0x26 from there).
    let frame = m.push_frame(INTERPRETER_FRAME);
    // `s2` survives between opcodes; only 0x18 and 0x2B set it. Its entry
    // value is the caller's register, which 80037B24's undefined default
    // path forwards (see `step`).
    let mut s2 = None;
    let mut at = script;
    let mut op = m.lbu(at);
    while op != 0 {
        at = at.wrapping_add(1);
        if op < 0x50 {
            at = opcode(m, op, at, &mut s2, frame);
        }
        if m.fault().is_some() {
            break;
        }
        op = m.lbu(at);
    }
    m.pop_frame(INTERPRETER_FRAME);
    at
}

/// Stack frame size of 80053704.
const INTERPRETER_FRAME: u32 = 0x50;

/// One opcode (jump table 0x80023CB0); `at` is just past the opcode byte.
/// Returns the address of the next opcode.
fn opcode(m: &mut Machine, op: u8, mut at: u32, s2: &mut Option<u32>, frame: u32) -> u32 {
    match op {
        0x01 => {
            let state = mission(m);
            let phase = m.lbu(at);
            m.sb(state.wrapping_add(0x29), phase);
            let gameplay = m.gw(GAMEPLAY);
            let mode = m.lw(gameplay.wrapping_add(4));
            at = at.wrapping_add(1);
            if mode == 4 {
                let state = mission(m);
                let second = u32::from(m.lbu(state.wrapping_add(0x27)));
                common::request_transition(m, 8, second);
                let context = m.gw(CONTEXT);
                let flags = m.lw(context);
                let state = mission(m);
                m.sw(context, flags | 0x1000_0000);
                if m.lbu(state.wrapping_add(0x29)) != 0 {
                    shared::cancel_stream_requests(m);
                }
            } else if mode == 0 {
                step(m, s2.map(|s2| (s2, at)));
                let gameplay = m.gw(GAMEPLAY);
                and_word(m, gameplay, 0xdfff_ffff);
            }
        }
        0x02..=0x05 => {
            let offset = [0x24, 0x23, 0x25, 0x26][usize::from(op - 2)];
            let state = mission(m);
            let value = m.lbu(at);
            at = at.wrapping_add(1);
            m.sb(state.wrapping_add(offset), value);
        }
        0x06 => {
            let value = m.lbu(at);
            let mapped = match value {
                0 => Some(0),
                1 => Some(1),
                2 => Some(2),
                3 => Some(8),
                4 => Some(4),
                5 => Some(0x10),
                6 => Some(0x20),
                7 => Some(0x40),
                _ => None,
            };
            if let Some(mapped) = mapped {
                set_mission_byte(m, 0x27, mapped);
            }
            at = at.wrapping_add(1);
        }
        0x07 => {
            or_mission_byte(m, 0x2a, 4);
            set_mission_byte(m, 0x29, 9);
        }
        0x08 => {
            set_mission_byte(m, 0x29, 9);
            let context = m.gw(CONTEXT);
            let value = m.lbu(at);
            at = at.wrapping_add(1);
            m.sb(context.wrapping_add(0x3c1), value);
        }
        0x09 => or_mission_byte(m, 0x2a, 1),
        0x0a => {
            let context = m.gw(CONTEXT);
            let value = m.lbu(at);
            at = at.wrapping_add(1);
            m.sb(context.wrapping_add(0x95a), value);
        }
        0x0b => {
            let state = mission(m);
            let value = m.lw(state.wrapping_add(0x10));
            at = block_word_below_800544e4(m, at, value);
        }
        0x0c => at = block_mission_word4_80054554(m, at),
        0x0d => at = block_byte22_800545c4(m, at),
        0x0e => at = block_byte21_8005462c(m, at),
        0x0f => at = block_byte20_80054694(m, at),
        0x10 => at = block_byte2f_800546fc(m, at),
        0x11 => at = block_bytes2d2e_80054770(m, at),
        0x12 => at = block_context_flag_800547d4(m, at),
        0x13 => at = block_random_8005483c(m, at),
        0x14 => at = block_entity_value_8005489c(m, at),
        0x15 => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            if index < 0x20 {
                shared::mark_unit(m, index);
                shared::activate_unit(m, index);
            } else {
                let object = index - 0x20;
                shared::mark_object(m, object);
                shared::activate_object(m, object);
            }
        }
        0x16 => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let table = m.gw(ENTITIES);
            let slot = table.wrapping_add(index * 4);
            let object = m.lw(slot);
            and_word(m, object, !0x4000);
            if index < 0x20 {
                let object = m.lw(slot);
                and_word(m, object, 0xfffe_ffff);
            }
        }
        0x17 | 0x19 => at = at.wrapping_add(1),
        0x18 => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let object = entity(m, index);
            *s2 = Some(object);
            let value = read_i16_80054b04(m, at);
            match value {
                -1 => {
                    let source = if index < 0x20 {
                        let records = m.gw(0x374);
                        m.lw(records.wrapping_add(index << 6).wrapping_add(0x10))
                    } else {
                        let records = m.gw(0x378);
                        m.lw(records.wrapping_add(index * 40).wrapping_sub(0x4f4))
                    };
                    let value = m.lhu(source);
                    m.sh(object.wrapping_add(0x4e), value);
                }
                -2 => {
                    let value = m.lhu(GP + 0x498);
                    m.sh(object.wrapping_add(0x4e), value);
                }
                -3 => {
                    let value = m.lhu(object.wrapping_add(0x4e));
                    m.sh(GP + 0x498, value);
                }
                _ => m.sh(object.wrapping_add(0x4e), value as u16),
            }
            at = at.wrapping_add(2);
        }
        0x1a => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            or_word(m, flag_word(index), 0x2_0000);
        }
        0x1b => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            match m.lbu(at) {
                1 => {
                    let flags = flag_word(index);
                    or_word(m, flags, 0x100);
                    let table = m.gw(ENTITIES);
                    let slot = table.wrapping_add(index * 4);
                    let object = m.lw(slot);
                    and_word(m, object, !0x1c0);
                    let object = m.lw(slot);
                    or_word(m, object, 0x80);
                    let aux = m.lw(ENTITY_AUX.wrapping_add(index * 4));
                    m.sb(aux.wrapping_add(5), 1);
                    m.sb(ENTITY_DISPLAY.wrapping_add(index * 12 + 2), 1);
                    and_word(m, flags, !0x200);
                }
                0 => {
                    and_word(m, flag_word(index), !0x100);
                    let aux = m.lw(ENTITY_AUX.wrapping_add(index * 4));
                    m.sb(aux.wrapping_add(5), 0);
                    m.sb(ENTITY_DISPLAY.wrapping_add(index * 12 + 2), 0);
                }
                _ => {}
            }
            at = at.wrapping_add(1);
        }
        0x1c => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            match m.lbu(at) {
                1 => {
                    let flags = flag_word(index);
                    or_word(m, flags, 0x200);
                    let table = m.gw(ENTITIES);
                    let slot = table.wrapping_add(index * 4);
                    let object = m.lw(slot);
                    and_word(m, object, !0x1c0);
                    let object = m.lw(slot);
                    or_word(m, object, 0x40);
                    let aux = m.lw(ENTITY_AUX.wrapping_add(index * 4));
                    m.sb(aux.wrapping_add(5), 3);
                    m.sb(ENTITY_DISPLAY.wrapping_add(index * 12 + 2), 2);
                    and_word(m, flags, !0x100);
                    if u32::from(m.lbu(LOCK + 0xe)) == index {
                        let value = shared::release_lock(m, 0);
                        m.sb(LOCK + 0xe, value as u8);
                    }
                }
                0 => {
                    and_word(m, flag_word(index), !0x200);
                    let aux = m.lw(ENTITY_AUX.wrapping_add(index * 4));
                    m.sb(aux.wrapping_add(5), 0);
                    m.sb(ENTITY_DISPLAY.wrapping_add(index * 12 + 2), 0);
                }
                _ => {}
            }
            at = at.wrapping_add(1);
        }
        0x1d => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let flags = flag_word(index);
            if m.lbu(at) & 2 != 0 {
                or_word(m, flags, 0x4000);
            } else {
                and_word(m, flags, !0x4000);
            }
            if m.lbu(at) & 1 != 0 {
                or_word(m, flags, 0x8000);
            } else {
                and_word(m, flags, 0xffff_7fff);
            }
            at = at.wrapping_add(1);
            if u32::from(m.lbu(LOCK + 0xe)) == index {
                m.sb(LOCK + 0xe, 0xff);
                shared::release_lock(m, 0);
                shared::set_lock(m, 0, 0xff);
            }
        }
        0x1e => at = at.wrapping_add(2),
        0x1f => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let objects = m.gw(0x33c);
            m.sb(objects.wrapping_add(index * 0xb8).wrapping_add(0x1c), 0xff);
            let value = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let records = m.gw(0x374);
            m.sw(
                records.wrapping_add(index << 6).wrapping_add(0x24),
                value.wrapping_sub(1),
            );
        }
        0x20 => {
            let object = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let pattern = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let records = m.gw(0x378);
            let record = records.wrapping_add(object * 40);
            let patterns = m.gw(0x3ac);
            m.sw(
                record.wrapping_add(0x14),
                patterns.wrapping_add(pattern * 48),
            );
        }
        0x21 => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let mode = m.lbu(at);
            let value = match mode {
                1 => 0x800b_023c,
                2 => 0x800b_0264,
                _ => {
                    let sources = m.gw(0x37c);
                    let kind = m.lbu(sources.wrapping_add(index * 44).wrapping_add(3));
                    let patterns = m.gw(0x3a0);
                    patterns.wrapping_add(u32::from(kind) * 40)
                }
            };
            let records = m.gw(0x374);
            m.sw(records.wrapping_add(index << 6).wrapping_add(0x14), value);
            at = at.wrapping_add(1);
        }
        0x22 | 0x23 => {
            let bit = if op == 0x22 { 0x1000 } else { 0x2000 };
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            if m.lbu(at) == 1 {
                or_word(m, flag_word(index), bit);
            } else {
                and_word(m, flag_word(index), !bit);
            }
            at = at.wrapping_add(1);
        }
        0x24 => {
            let index = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let on = m.lbu(at) == 1;
            at = at.wrapping_add(1);
            if on {
                enable_group_8004ab78(m, index);
            } else {
                disable_group_8004abe8(m, index);
            }
        }
        0x25 => {
            let index = m.lbu(at);
            at = at.wrapping_add(1);
            // Frame +0x10: VECTOR {x, y, z << 12, pad +0x1C}, +0x20: SVECTOR
            // {rx, ry, rz, pad +0x26}. The pads are never written here.
            let locals = frame.wrapping_add(0x10);
            for i in 0..3 {
                let value = read_u32_80054b20(m, at);
                at = at.wrapping_add(4);
                m.sw(locals.wrapping_add(i * 4), value << 12);
            }
            for i in 0..3 {
                let value = read_i16_80054b04(m, at);
                at = at.wrapping_add(2);
                m.sh(locals.wrapping_add(0x10 + i * 2), value as u16);
            }
            if index == 0 {
                shared::place_player(m, locals);
            } else {
                let object = entity(m, u32::from(index));
                for i in 0..6 {
                    let word = m.lw(locals.wrapping_add(i * 4));
                    m.sw(object.wrapping_add(8 + i * 4), word);
                }
            }
        }
        0x29 => {
            let slot = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let value = read_i16_80054b04(m, at);
            at = at.wrapping_add(2);
            queue_message_80054c20(m, slot, value as u32);
        }
        0x2a => {
            let sound = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let kind = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            queue_sound_event_8002ee78(m, sound, kind);
        }
        0x2b => {
            let count = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            if count != 0 {
                *s2 = Some(count);
                for _ in 0..count {
                    let sound = u32::from(m.lbu(at));
                    at = at.wrapping_add(1);
                    let kind = u32::from(m.lbu(at));
                    at = at.wrapping_add(1);
                    queue_sound_event_8002ee78(m, sound, kind);
                }
            }
        }
        0x2d | 0x2f | 0x34 | 0x36 | 0x39 => {
            let offset = match op {
                0x2d => 0x1e,
                0x2f => 0x1f,
                0x34 => 0x2b,
                0x36 => 0x2c,
                _ => 0x1c,
            };
            let state = mission(m);
            let value = m.lbu(at);
            at = at.wrapping_add(1);
            m.sb(state.wrapping_add(offset), value);
        }
        0x2e => set_mission_byte(m, 0x20, 0),
        0x30 => set_mission_byte(m, 0x21, 0),
        0x31 => {
            let state = mission(m);
            let value = m.lbu(state.wrapping_add(0x22));
            m.sb(state.wrapping_add(0x22), value.wrapping_add(1));
        }
        0x32 => set_mission_byte(m, 0x22, 0),
        0x33 => {
            let state = mission(m);
            let message = u32::from(m.lbu(state.wrapping_add(0x2b)));
            show_message_8004b760(m, message);
        }
        0x35 => {
            let state = mission(m);
            let value = m.lbu(state.wrapping_add(0x20));
            m.sb(state.wrapping_add(0x20), value << 1);
        }
        0x37 => {
            let state = mission(m);
            let stream = u32::from(m.lbu(state.wrapping_add(0x2c)));
            play_stream_800550f0(m, stream);
        }
        0x38 => {
            let stream = u32::from(m.lbu(at));
            play_stream_800550f0(m, stream);
            at = at.wrapping_add(1);
        }
        0x3a => {
            let value = read_i16_80054b04(m, at);
            at = at.wrapping_add(2);
            let state = mission(m);
            m.sw(state.wrapping_add(0x14), value as u32);
        }
        0x3b => set_gameplay_bit2_8005d918(m, 1),
        0x3c => {
            let state = mission(m);
            let value = m.lw(state.wrapping_add(0xc));
            at = block_word_below_800544e4(m, at, value);
        }
        0x3d => at = block_gameplay_byte8_80054918(m, at),
        0x3e => at = block_context_byte398_8005497c(m, at),
        0x46 => {
            or_mission_byte(m, 0x2a, 2);
            let state = mission(m);
            let value = m.lbu(at);
            at = at.wrapping_add(1);
            m.sb(state.wrapping_add(0x3e), value);
        }
        0x47 => {
            at = at.wrapping_add(1);
            or_mission_byte(m, 0x2a, 4);
        }
        0x48 => {
            let value = m.lbu(at);
            at = at.wrapping_add(1);
            set_mission_byte(m, 0x31, value);
            let half = read_i16_80054b04(m, at);
            let state = mission(m);
            at = at.wrapping_add(2);
            m.sh(state.wrapping_add(0x32), half as u16);
        }
        0x49 => {
            set_mission_byte(m, 0x30, 1);
            let state = mission(m);
            m.sh(state.wrapping_add(0x1a), 0);
        }
        0x4a => {
            for offset in 0x34..0x38 {
                let state = mission(m);
                let value = m.lbu(at);
                at = at.wrapping_add(1);
                m.sb(state.wrapping_add(offset), value);
            }
        }
        0x4b => {
            let value = read_i16_80054b04(m, at);
            at = at.wrapping_add(2);
            let state = mission(m);
            let word = m.lw(state.wrapping_add(4));
            m.sw(state.wrapping_add(4), word.wrapping_add(value as u32));
        }
        0x4c => {
            let mode = m.lbu(at);
            at = at.wrapping_add(1);
            set_mission_byte(m, 0x34, mode);
            let low = read_i16_80054b04(m, at);
            at = at.wrapping_add(2);
            let state = mission(m);
            m.sh(state.wrapping_add(0x38), low as u16);
            let high = read_i16_80054b04(m, at);
            let state = mission(m);
            at = at.wrapping_add(2);
            m.sh(state.wrapping_add(0x3a), high as u16);
        }
        0x4d => at = block_all_targets_800549e0(m, at),
        0x4e => {
            let context = m.gw(CONTEXT);
            let flags = m.lw(context.wrapping_add(0x970));
            if !flags & 0x10 != 0 {
                m.sw(context.wrapping_add(0x970), flags | 8);
            }
        }
        0x4f => at = block_context_flag10_80054aa4(m, at),
        // 0x26-0x28, 0x2C, 0x3F-0x45: no operation.
        _ => {}
    }
    at
}

/// 80054B04: little-endian signed halfword at any alignment.
pub fn read_i16_80054b04(m: &mut Machine, at: u32) -> i16 {
    let low = m.lbu(at);
    let high = m.lbu(at.wrapping_add(1));
    i16::from_le_bytes([low, high])
}

/// 80054B20: little-endian word at any alignment.
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

/// 800544E4 `[u32 limit][skip][else]`: then-block when `limit >= value`
/// (signed). Opcode 0x0B passes mission word +0x10, opcode 0x3C word +0xC.
pub fn block_word_below_800544e4(m: &mut Machine, at: u32, value: u32) -> u32 {
    let limit = read_u32_80054b20(m, at);
    let (next, at) = block_header(m, at.wrapping_add(4));
    run_block(m, at, (limit as i32) < (value as i32), 0x20);
    next
}

/// 80054554 `[u32 limit][skip][else]`: then-block when mission word +4 >=
/// `limit` (signed).
pub fn block_mission_word4_80054554(m: &mut Machine, at: u32) -> u32 {
    let limit = read_u32_80054b20(m, at);
    let (next, at) = block_header(m, at.wrapping_add(4));
    let state = mission(m);
    let value = m.lw(state.wrapping_add(4));
    run_block(m, at, (value as i32) < (limit as i32), 0x20);
    next
}

/// 800545C4 `[value][skip][else]`: then-block when mission byte +0x22 >= value.
pub fn block_byte22_800545c4(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let state = mission(m);
    let current = m.lbu(state.wrapping_add(0x22));
    run_block(m, at, current < value, 0x18);
    next
}

/// 8005462C `[value][skip][else]`: then-block when value >= mission byte +0x21.
pub fn block_byte21_8005462c(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let state = mission(m);
    let current = m.lbu(state.wrapping_add(0x21));
    run_block(m, at, value < current, 0x18);
    next
}

/// 80054694 `[value][skip][else]`: then-block when mission byte +0x20 >= value.
pub fn block_byte20_80054694(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let state = mission(m);
    let current = m.lbu(state.wrapping_add(0x20));
    run_block(m, at, current < value, 0x18);
    next
}

/// 800546FC `[u32 ignored][skip][else]`: then-block when mission byte +0x2F
/// is 1. The word is read and discarded.
pub fn block_byte2f_800546fc(m: &mut Machine, at: u32) -> u32 {
    let _ = read_u32_80054b20(m, at);
    let (next, at) = block_header(m, at.wrapping_add(4));
    let state = mission(m);
    let value = m.lbu(state.wrapping_add(0x2f));
    run_block(m, at, value != 1, 0x20);
    next
}

/// 80054770 `[skip][else]`: then-block when mission byte +0x2D < +0x2E.
pub fn block_bytes2d2e_80054770(m: &mut Machine, at: u32) -> u32 {
    let (next, at) = block_header(m, at);
    let state = mission(m);
    let high = m.lbu(state.wrapping_add(0x2e));
    let low = m.lbu(state.wrapping_add(0x2d));
    run_block(m, at, low >= high, 0x18);
    next
}

/// 800547D4 `[index][skip][else]`: then-block when context byte
/// `+0xD50+index` is nonzero.
pub fn block_context_flag_800547d4(m: &mut Machine, at: u32) -> u32 {
    let index = u32::from(m.lbu(at));
    let (next, at) = block_header(m, at.wrapping_add(1));
    let context = m.gw(CONTEXT);
    let value = m.lbu(index.wrapping_add(context).wrapping_add(0xd50));
    run_block(m, at, value == 0, 0x18);
    next
}

/// 8005483C `[skip][else]`: then-block when `rand()` is odd.
pub fn block_random_8005483c(m: &mut Machine, at: u32) -> u32 {
    let (next, at) = block_header(m, at);
    let value = sdk::rand(m);
    run_block(m, at, value & 1 == 0, 0x20);
    next
}

/// 8005489C `[entity][value][skip][else]`: then-block when the entity's
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

/// 80054918 `[value][skip][else]`: then-block when gameplay byte +8 == value.
pub fn block_gameplay_byte8_80054918(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let gameplay = m.gw(GAMEPLAY);
    let current = m.lbu(gameplay.wrapping_add(8));
    run_block(m, at, value != current, 0x18);
    next
}

/// 8005497C `[value][skip][else]`: then-block when context byte +0x398 == value.
pub fn block_context_byte398_8005497c(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let context = m.gw(CONTEXT);
    let current = m.lbu(context.wrapping_add(0x398));
    run_block(m, at, value != current, 0x18);
    next
}

/// 800549E0 `[skip][else]`: then-block when context bytes `+0xD51..+0xD72`
/// are all 1 or 2 and the mission rank (80053604) is 1 or 2. The rank is
/// evaluated in every case.
pub fn block_all_targets_800549e0(m: &mut Machine, at: u32) -> u32 {
    let (next, at) = block_header(m, at);
    let context = m.gw(CONTEXT);
    let mut all = true;
    for index in 1..0x23 {
        let value = m.lbu(context.wrapping_add(index).wrapping_add(0xd50));
        if !matches!(value, 1 | 2) {
            all = false;
            break;
        }
    }
    let rank = rank_80053604(m);
    if !matches!(rank.wrapping_sub(1) & 0xff, 0 | 1) {
        all = false;
    }
    run_block(m, at, !all, 0x20);
    next
}

/// 80054AA4 `[skip][else]`: then-block when context word +0x970 has bit 0x10.
pub fn block_context_flag10_80054aa4(m: &mut Machine, at: u32) -> u32 {
    let (next, at) = block_header(m, at);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context.wrapping_add(0x970));
    run_block(m, at, flags & 0x10 == 0, 0x18);
    next
}

// ---- mission state ------------------------------------------------------------------

/// 80053604: mission rank 1 (best) .. 5 from the evaluation mode at mission
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

/// 80037B24: advance the mission state machine and request the resident
/// transition for the current phase (mission byte +0x29) or pending flag
/// (byte +0x2A bits 0..2).
pub fn step_80037b24(m: &mut Machine) {
    step(m, None);
}

/// 80037B24 with the caller's `(s2, s1)` registers, which the original
/// passes on unchanged to 80032C80 when the phase is none of 0-5 or 9.
/// Without them that undefined path raises `Fault::Untranslated` at 80037D10.
fn step(m: &mut Machine, caller: Option<(u32, u32)>) {
    let state = mission(m);
    let counter = m.lbu(state.wrapping_add(0x3f));
    m.sb(state.wrapping_add(0x3f), counter.wrapping_add(1));
    let state = mission(m);
    let flags = m.lbu(state.wrapping_add(0x2a));
    let (first, second) = if flags & 1 != 0 {
        m.sb(state.wrapping_add(0x2a), flags & 0xfe);
        (0x80, 0)
    } else if flags & 2 != 0 {
        m.sb(state.wrapping_add(0x2a), flags & 0xfd);
        (0x10_0000, 0)
    } else if flags & 4 != 0 {
        m.sb(state.wrapping_add(0x2a), flags & 0xfb);
        restore_player_model_800782b4(m);
        (0x20_0000, 0)
    } else {
        let phase = m.lbu(state.wrapping_add(0x29));
        match phase {
            0 => {
                let value = m.lbu(state.wrapping_add(0x23));
                m.sb(state.wrapping_add(0x3d), value);
                (2, u32::from(value))
            }
            1 => {
                let value = m.lbu(state.wrapping_add(0x24));
                let id = m.lbu(MISSION_ID);
                set_result_80037d34(m, id, value);
                (0x10, u32::from(value))
            }
            2 => {
                let value = m.lbu(state.wrapping_add(0x26));
                let id = m.lbu(MISSION_ID);
                set_ending_80037d58(m, id);
                (0x4000, u32::from(value))
            }
            3 => (0x400, u32::from(m.lbu(state.wrapping_add(0x25)))),
            4 => {
                let id = m.lbu(MISSION_ID);
                let context = m.gw(CONTEXT);
                m.sw(context.wrapping_add(0x3a4), 3);
                let _ = m.lbu(state.wrapping_add(0x26));
                set_ending_80037d58(m, id);
                record_result_80047cf4(m);
                shared::stop_all_sound(m);
                (0x100_0000, 0)
            }
            5 => {
                record_result_80047cf4(m);
                let context = m.gw(CONTEXT);
                let next = m.lbu(context.wrapping_add(0x3c1));
                m.sh(context.wrapping_add(0x390), u16::from(next));
                shared::stop_all_sound(m);
                let state = mission(m);
                if m.lbu(state.wrapping_add(0x29)) == 5 {
                    let context = m.gw(CONTEXT);
                    let mission_index = m.lh(context.wrapping_add(0x390)) as i32 as u32;
                    let id = m.lbu(0x800b_0210u32.wrapping_add(mission_index));
                    if id == 0x36 {
                        m.sb(context.wrapping_add(0x3ad), 5);
                        let current = m.gw(CONTEXT);
                        m.sh(context.wrapping_add(0x392), 0x13);
                        m.sb(current.wrapping_add(0x3ae), 1);
                        let current = m.gw(CONTEXT);
                        m.sb(current.wrapping_add(0x3af), 1);
                    }
                }
                (0x200_0000, 0)
            }
            9 => {
                record_result_80047cf4(m);
                shared::stop_all_sound(m);
                (0x80_0000, 0)
            }
            _ => match caller {
                Some(registers) => registers,
                None => {
                    m.raise(Fault::Untranslated {
                        address: 0x8003_7d10,
                    });
                    return;
                }
            },
        }
    };
    common::request_transition(m, first, second);
}

/// 80037D34: context word +0xF7C = byte `0x800AF8E8[id * 4 + index]`.
pub fn set_result_80037d34(m: &mut Machine, id: u8, index: u8) {
    let at = 0x800a_f8e8u32
        .wrapping_add(u32::from(id) * 4)
        .wrapping_add(u32::from(index));
    let context = m.gw(CONTEXT);
    let value = m.lbu(at);
    m.sw(context.wrapping_add(0xf7c), u32::from(value));
}

/// 80037D58: context byte +0x3AC = 1 for mission 0x2E, 2 for 0x36/0x3B, else 0.
pub fn set_ending_80037d58(m: &mut Machine, id: u8) {
    let value = match id {
        0x2e => 1,
        0x36 | 0x3b => 2,
        _ => 0,
    };
    let context = m.gw(CONTEXT);
    m.sb(context.wrapping_add(0x3ac), value);
}

/// 80047CF4: store the rank and score into the context result block
/// (+0x3BC); missions 0x22/0x27/0x3A also copy it to `gp+0x464..0x468`.
pub fn record_result_80047cf4(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let block = context.wrapping_add(0x3bc);
    let rank = rank_80053604(m);
    m.sb(block.wrapping_add(0xb), rank as u8);
    let state = mission(m);
    let id = m.lbu(MISSION_ID);
    let score = m.lhu(state.wrapping_add(0xc));
    m.sh(block.wrapping_add(2), score);
    if matches!(id, 0x27 | 0x3a | 0x22) {
        let value = m.lhu(block.wrapping_add(2));
        m.sh(GP + 0x464, value);
        let value = m.lhu(block.wrapping_add(8));
        m.sh(GP + 0x466, value);
        let value = m.lbu(block.wrapping_add(0xa));
        m.sb(GP + 0x468, value);
    }
}

/// 800782B4: reattach the player model to the gameplay object (+0x28) with
/// flags 0x2000 and clear the gameplay pause bit through 80035248(0).
pub fn restore_player_model_800782b4(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(ENTITIES);
    let object = m.lw(gameplay.wrapping_add(0x28));
    let player = m.lw(table);
    shared::set_camera_target(m, object, 0x2000, player);
    // 80035248 belongs to the 80032510 tail tree.
    dispatch::call(m, 0x8003_5248, [0, 0, 0, 0]);
}

// ---- sound -----------------------------------------------------------------------

/// 8002EE78: append event `{kind, -, sound}` at the write index `gp+0x315`
/// (modulo 32).
pub fn queue_sound_event_8002ee78(m: &mut Machine, sound: u32, kind: u32) {
    let index = u32::from(m.lbu(GP + 0x315));
    let at = SOUND_EVENTS + index * 4;
    m.sh(at + 2, sound as u16);
    m.sb(at, kind as u8);
    let index = m.lbu(GP + 0x315);
    m.sb(GP + 0x315, index.wrapping_add(1) & 0x1f);
}

/// 8002D724: start sound effect `sound` on a free channel (definitions
/// 0x2E..0x31 first try 8002D7B8). Returns the channel or 0xFF.
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

/// 8002D7B8: start `sound` on its fixed channel, the channel already
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

/// 8002D8D0: channel 12..23 currently assigned `sound`, else 0xFF.
pub fn find_channel_8002d8d0(m: &mut Machine, sound: u32) -> u32 {
    for channel in 0xc..0x18 {
        if u32::from(m.lbu(SOUND_CHANNELS + channel * 20)) == sound {
            return channel;
        }
    }
    0xff
}

/// 8002D910: claim the first idle channel 12..23 (state byte +3 := 2).
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

/// 8002E0F8: load definition `sound` into `channel`; returns the channel
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

/// 8002C4B0: queue stream request `{3, 0, a0, a1, -, a2}` at the write index.
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

/// 800550F0: play speech stream `index` (table 0x800B049C) unless it is
/// already playing; 0xFF only cancels. Returns `index`.
pub fn play_stream_800550f0(m: &mut Machine, index: u32) -> u32 {
    if index == 0xff {
        shared::cancel_stream_requests(m);
        return 0xff;
    }
    let slot = 0x800b_049cu32.wrapping_add(index.wrapping_mul(4));
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

/// 8004AB78: enable group `index` (words `[gp+0x394]`, 20-byte records
/// `[gp+0x388]` naming two entities that gain flags 0x200000/0x400000).
pub fn enable_group_8004ab78(m: &mut Machine, index: u32) {
    let groups = m.gw(0x394);
    or_word(m, groups.wrapping_add(index.wrapping_mul(4)), 1);
    let records = m.gw(0x388);
    let record = records.wrapping_add(index.wrapping_mul(20));
    let first = u32::from(m.lbu(record.wrapping_add(2)));
    or_word(m, flag_word(first), 0x20_0000);
    let second = u32::from(m.lbu(record.wrapping_add(3)));
    or_word(m, flag_word(second), 0x40_0000);
}

/// 8004ABE8: disable group `index` if enabled (inverse of 8004AB78).
pub fn disable_group_8004abe8(m: &mut Machine, index: u32) {
    let groups = m.gw(0x394);
    let at = groups.wrapping_add(index.wrapping_mul(4));
    let word = m.lw(at);
    if word & 1 == 0 {
        return;
    }
    m.sw(at, word & !1);
    let records = m.gw(0x388);
    let record = records.wrapping_add(index.wrapping_mul(20));
    let first = u32::from(m.lbu(record.wrapping_add(2)));
    and_word(m, flag_word(first), 0xffdf_ffff);
    let second = u32::from(m.lbu(record.wrapping_add(3)));
    and_word(m, flag_word(second), 0xffbf_ffff);
}

/// 8005D918: gameplay word bit 2 set (`on != 0`) or cleared.
pub fn set_gameplay_bit2_8005d918(m: &mut Machine, on: u32) {
    let gameplay = m.gw(GAMEPLAY);
    if on != 0 {
        or_word(m, gameplay, 4);
    } else {
        and_word(m, gameplay, !4);
    }
}

/// 80054C20: put message `{slot byte, value, timer 0}` into the first free
/// of four queue entries (`[gp+0x4A8]`: bytes +0x14, halfwords +0xC/+4).
pub fn queue_message_80054c20(m: &mut Machine, slot: u32, value: u32) {
    for i in 0..4 {
        let queue = m.gw(0x4a8);
        let at = queue.wrapping_add(i);
        if m.lbu(at.wrapping_add(0x14)) == 0xff {
            m.sb(at.wrapping_add(0x14), slot as u8);
            let queue = m.gw(0x4a8);
            let at = queue.wrapping_add(i * 2);
            m.sh(at.wrapping_add(0xc), value as u16);
            m.sh(at.wrapping_add(4), 0);
            return;
        }
    }
}

// ---- messages --------------------------------------------------------------------------

/// 8004B760: show text message `message` (0xFF none): set up the glyph
/// list in the primitive control block `[gp+0x470]` (+0x110 count, +0x114
/// /+0x118 layout, +0x11C current message, 8-byte glyphs from +8).
/// Messages 6 and 7 also play sound 0x66/0x67.
pub fn show_message_8004b760(m: &mut Machine, message: u32) {
    if message == 0xff {
        return;
    }
    let control = m.gw(0x470);
    if m.lw(control.wrapping_add(0x11c)) == message {
        return;
    }
    let resources = m.lw(0x800b_adc0);
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
    const LINES: u32 = 0x800c_5598;
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
    let control = m.gw(0x470);
    let mut index = m.lw(control.wrapping_add(0x110));
    while (index as i32) < 0x20 {
        let control = m.gw(0x470);
        m.sb(control.wrapping_add(index << 3).wrapping_add(8), 0);
        index = index.wrapping_add(1);
    }
    let mut control = m.gw(0x470);
    if (m.lw(control.wrapping_add(0x110)) as i32) <= 0 {
        return;
    }
    let mut index = 0u32;
    loop {
        m.sb(control.wrapping_add(index << 3).wrapping_add(8), 2);
        control = m.gw(0x470);
        index = index.wrapping_add(1);
        if (index as i32) >= (m.lw(control.wrapping_add(0x110)) as i32) {
            break;
        }
    }
}

/// 8004B920: append glyphs for the text line `{u16 x, u16 y, u32 text}` at
/// `line` (widths from 0x800B0070; accented letters add an accent glyph).
pub fn layout_text_8004b920(m: &mut Machine, line: u32) {
    const GLYPHS: u32 = 0x800b_0070;
    let mut x = u32::from(m.lhu(line));
    let y = u32::from(m.lhu(line.wrapping_add(2)));
    let mut index = 0u32;
    loop {
        let text = m.lw(line.wrapping_add(4));
        let character = m.lbu(text.wrapping_add(index));
        if character == 0 {
            return;
        }
        index = index.wrapping_add(1);
        let mut glyph = glyph_8004ba88(character);
        if glyph == 0x80 {
            x = x.wrapping_add(10);
            continue;
        }
        if glyph == u32::MAX {
            return;
        }
        if glyph & 0xff00 != 0 {
            let accent = glyph >> 8;
            let control = m.gw(0x470);
            let count = m.lw(control.wrapping_add(0x110));
            glyph &= 0xff;
            let at = control.wrapping_add(count.wrapping_shl(3)).wrapping_add(8);
            m.sw(control.wrapping_add(0x110), count.wrapping_add(1));
            m.sb(at.wrapping_add(1), accent as u8);
            let accent_info = (accent & 0xff) * 4 + GLYPHS;
            m.sb(at, 0);
            m.sh(at.wrapping_add(2), 0);
            let width = i32::from(m.lbu(glyph * 4 + GLYPHS + 2));
            let accent_width = i32::from(m.lbu(accent_info + 2));
            let offset = (width - accent_width) >> 1;
            m.sh(at.wrapping_add(4), x.wrapping_add(offset as u32) as u16);
            let raise = u32::from(m.lbu(accent_info + 3));
            m.sh(
                at.wrapping_add(6),
                y.wrapping_sub(raise).wrapping_add(4) as u16,
            );
        }
        let control = m.gw(0x470);
        let count = m.lw(control.wrapping_add(0x110));
        let at = control.wrapping_add(count.wrapping_shl(3)).wrapping_add(8);
        m.sw(control.wrapping_add(0x110), count.wrapping_add(1));
        m.sb(at, 0);
        m.sb(at.wrapping_add(1), glyph as u8);
        m.sh(at.wrapping_add(2), 0);
        m.sh(at.wrapping_add(4), x as u16);
        m.sh(at.wrapping_add(6), y as u16);
        let width = u32::from(m.lbu(glyph.wrapping_mul(4).wrapping_add(GLYPHS + 2)));
        x = width.wrapping_add(x).wrapping_sub(1);
    }
}

/// 8004BA88: glyph code for a text byte: A-Z → 0..25, punctuation 0x1A..
/// 0x1E, space → 0x80 (advance only), accented capitals → accent << 8 |
/// letter, anything else → -1.
pub fn glyph_8004ba88(character: u8) -> u32 {
    match character {
        b'A'..=b'Z' => u32::from(character - b'A'),
        b'.' => 0x1a,
        b'!' => 0x1b,
        b'?' => 0x1c,
        b'\'' => 0x1d,
        b' ' => 0x80,
        b'-' => 0x1e,
        0xc0 => 0x1f00,
        0xc1 => 0x2000,
        0xc2 => 0x2100,
        0xc4 => 0x2200,
        0xc8 => 0x1f04,
        0xc9 => 0x2004,
        0xca => 0x2104,
        0xd3 => 0x200e,
        0xd6 => 0x220e,
        0xdc => 0x2214,
        _ => u32::MAX,
    }
}

/// Every routine translated here, by original address (`a0..a3` in, `v0`
/// out; routines without a result return 0).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let _ = a3;
    Some(match target {
        0x8005_3704 => interpret(m, a0),
        0x8003_7b24 => {
            step_80037b24(m);
            0
        }
        0x8005_4b04 => read_i16_80054b04(m, a0) as u32,
        0x8005_4b20 => read_u32_80054b20(m, a0),
        0x8005_44e4 => block_word_below_800544e4(m, a0, a1),
        0x8005_4554 => block_mission_word4_80054554(m, a0),
        0x8005_45c4 => block_byte22_800545c4(m, a0),
        0x8005_462c => block_byte21_8005462c(m, a0),
        0x8005_4694 => block_byte20_80054694(m, a0),
        0x8005_46fc => block_byte2f_800546fc(m, a0),
        0x8005_4770 => block_bytes2d2e_80054770(m, a0),
        0x8005_47d4 => block_context_flag_800547d4(m, a0),
        0x8005_483c => block_random_8005483c(m, a0),
        0x8005_489c => block_entity_value_8005489c(m, a0),
        0x8005_4918 => block_gameplay_byte8_80054918(m, a0),
        0x8005_497c => block_context_byte398_8005497c(m, a0),
        0x8005_49e0 => block_all_targets_800549e0(m, a0),
        0x8005_4aa4 => block_context_flag10_80054aa4(m, a0),
        0x8005_3604 => rank_80053604(m),
        0x8003_7d34 => {
            set_result_80037d34(m, a0 as u8, a1 as u8);
            0
        }
        0x8003_7d58 => {
            set_ending_80037d58(m, a0 as u8);
            0
        }
        0x8004_7cf4 => {
            record_result_80047cf4(m);
            0
        }
        0x8007_82b4 => {
            restore_player_model_800782b4(m);
            0
        }
        0x8002_ee78 => {
            queue_sound_event_8002ee78(m, a0, a1);
            0
        }
        0x8002_d724 => play_sound_8002d724(m, a0),
        0x8002_d7b8 => play_sound_reuse_8002d7b8(m, a0),
        0x8002_d8d0 => find_channel_8002d8d0(m, a0),
        0x8002_d910 => claim_channel_8002d910(m),
        0x8002_e0f8 => start_channel_8002e0f8(m, a0, a1),
        0x8002_c4b0 => {
            queue_stream_request_8002c4b0(m, a0, a1, a2);
            0
        }
        0x8005_50f0 => play_stream_800550f0(m, a0),
        0x8004_ab78 => {
            enable_group_8004ab78(m, a0);
            0
        }
        0x8004_abe8 => {
            disable_group_8004abe8(m, a0);
            0
        }
        0x8005_d918 => {
            set_gameplay_bit2_8005d918(m, a0);
            0
        }
        0x8005_4c20 => {
            queue_message_80054c20(m, a0, a1);
            0
        }
        0x8004_b760 => {
            show_message_8004b760(m, a0);
            0
        }
        0x8004_b920 => {
            layout_text_8004b920(m, a0);
            0
        }
        0x8004_ba88 => glyph_8004ba88(a0 as u8),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_are_unaligned_little_endian() {
        let mut m = Machine::default();
        m.write_bytes(0x8010_0001, &[0x34, 0x82, 0x78, 0x56]);
        assert_eq!(read_i16_80054b04(&mut m, 0x8010_0001), -0x7dcc);
        assert_eq!(read_u32_80054b20(&mut m, 0x8010_0001), 0x5678_8234);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn glyph_table() {
        assert_eq!(glyph_8004ba88(b'A'), 0);
        assert_eq!(glyph_8004ba88(b'Z'), 25);
        assert_eq!(glyph_8004ba88(b' '), 0x80);
        assert_eq!(glyph_8004ba88(0xdc), 0x2214);
        assert_eq!(glyph_8004ba88(0xd6), 0x220e);
        assert_eq!(glyph_8004ba88(b'a'), u32::MAX);
    }
}
