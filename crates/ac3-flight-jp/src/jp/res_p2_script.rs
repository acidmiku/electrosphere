//! Patches of `script` routines whose JP instructions differ from PAL
//! (REVIEW.md §2, part `resident-part2`). Each is listed in
//! [`REPLACES`](super::REPLACES) (80038054 in `TAKES_OVER`: its generated
//! arm truncates the arguments to bytes and returns 0, the JP caller passes
//! the mission index halfword and the routine returns the byte it stores).
//!
//! The JP script interpreter (80054EBC) has three opcodes fewer, the
//! context fields from +0x390 at their JP offsets, and the JP replacements
//! of PAL-only routines (message display 8004CEB0, ending 80038078, result
//! 80048A40). The mission step (80037E50) works from the mission index
//! (context half +0x38C) where PAL used the mission number byte
//! (`0x800B0210[index]`, a REVIEW.md site): JP has no such table.
//!
//! The interpreter and its opcode switch are copies of the generated ones
//! with those changes; the unchanged condition blocks and helpers are the
//! generated module's.
use crate::common::{self, CONTEXT, ENTITIES, GAMEPLAY};
use crate::dispatch;
use crate::machine::{Fault, GP, Machine, SmallData};
use crate::script::{
    ENTITY_AUX, ENTITY_DISPLAY, ENTITY_FLAGS, LOCK, MISSION, block_byte2f_800546fc,
    block_byte20_80054694, block_byte21_8005462c, block_byte22_800545c4, block_bytes2d2e_80054770,
    block_entity_value_8005489c, block_gameplay_byte8_80054918, block_mission_word4_80054554,
    block_random_8005483c, block_word_below_800544e4, disable_group_8004abe8,
    enable_group_8004ab78, play_stream_800550f0, queue_message_80054c20,
    queue_sound_event_8002ee78, read_i16_80054b04, read_u32_80054b20,
    restore_player_model_800782b4, set_gameplay_bit2_8005d918,
};
use crate::shared;

/// 8004CEB0 (JP-only): show text message `a0` (PAL 8004B760).
const SHOW_MESSAGE: u32 = 0x8004_ceb0;
/// 80038078 (JP-only): context byte +0x3A8 = byte `0x800B0E94[a0 * 2 + a1]`
/// (PAL 80037D58, the ending by mission number).
const SET_ENDING: u32 = 0x8003_8078;
/// 80048A40 (JP-only): the rank (80054DBC) and score into the result block
/// (PAL 80047CF4).
const RECORD_RESULT: u32 = 0x8004_8a40;

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

/// 80054EBC (JP; PAL 80053704, near, `call-:3 del:22 imm:12 ins:2 op:1 reg:8`):
/// run the script at `script` until its zero byte; returns the address of
/// that byte. JP has 0x4D opcodes (PAL 0x50: the PAL-only 0x4D..0x4F are
/// skipped like any opcode past the table); opcode 0x08 stores context byte
/// +0x3BD (PAL +0x3C1), 0x0A byte +0x956 (PAL +0x95A), 0x33 shows its
/// message through JP 8004CEB0 (PAL 8004B760), and the step of 0x01 is the
/// JP 80037E50 ([`step`]).
pub fn interpret(m: &mut Machine, script: u32) -> u32 {
    // The 0x50-byte frame is guest memory: opcode 0x25 builds its camera
    // frame at +0x10 and passes that address on (the original also copies
    // the never-written pads +0x1C/+0x26 from there).
    let frame = m.push_frame(INTERPRETER_FRAME);
    // `s2` survives between opcodes; only 0x18 and 0x2B set it. Its entry
    // value is the caller's register, which 80037E50's undefined default
    // path forwards (see `step`).
    let mut s2 = None;
    let mut at = script;
    let mut op = m.lbu(at);
    while op != 0 {
        at = at.wrapping_add(1);
        if op < 0x4d {
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

/// Stack frame size of 80054EBC.
const INTERPRETER_FRAME: u32 = 0x50;

/// One opcode (jump table 0x8002423C, 0x4D entries); `at` is just past the opcode byte.
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
            m.sb(context.wrapping_add(0x3bd), value);
        }
        0x09 => or_mission_byte(m, 0x2a, 1),
        0x0a => {
            let context = m.gw(CONTEXT);
            let value = m.lbu(at);
            at = at.wrapping_add(1);
            m.sb(context.wrapping_add(0x956), value);
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
                        let records = m.gw(0x384);
                        m.lw(records.wrapping_add(index << 6).wrapping_add(0x10))
                    } else {
                        let records = m.gw(0x388);
                        m.lw(records.wrapping_add(index * 40).wrapping_sub(0x4f4))
                    };
                    let value = m.lhu(source);
                    m.sh(object.wrapping_add(0x4e), value);
                }
                -2 => {
                    let value = m.lhu(GP + 0x4a4);
                    m.sh(object.wrapping_add(0x4e), value);
                }
                -3 => {
                    let value = m.lhu(object.wrapping_add(0x4e));
                    m.sh(GP + 0x4a4, value);
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
            let objects = m.gw(0x34c);
            m.sb(objects.wrapping_add(index * 0xb8).wrapping_add(0x1c), 0xff);
            let value = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            let records = m.gw(0x384);
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
            let records = m.gw(0x388);
            let record = records.wrapping_add(object * 40);
            let patterns = m.gw(0x3bc);
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
                1 => 0x800b_20d4,
                2 => 0x800b_20fc,
                _ => {
                    let sources = m.gw(0x38c);
                    let kind = m.lbu(sources.wrapping_add(index * 44).wrapping_add(3));
                    let patterns = m.gw(0x3b0);
                    patterns.wrapping_add(u32::from(kind) * 40)
                }
            };
            let records = m.gw(0x384);
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
            dispatch::call(m, SHOW_MESSAGE, [message, 0, 0, 0]);
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
        // 0x26-0x28, 0x2C, 0x3F-0x45: no operation.
        _ => {}
    }
    at
}

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

/// 80055F3C (JP; PAL 800547D4, near, `imm:1`) `[index][skip][else]`:
/// then-block when context byte `+0xD0C+index` (PAL +0xD50) is nonzero.
pub fn block_context_flag_800547d4(m: &mut Machine, at: u32) -> u32 {
    let index = u32::from(m.lbu(at));
    let (next, at) = block_header(m, at.wrapping_add(1));
    let context = m.gw(CONTEXT);
    let value = m.lbu(index.wrapping_add(context).wrapping_add(0xd0c));
    run_block(m, at, value == 0, 0x18);
    next
}

/// 800560E4 (JP; PAL 8005497C, near, `imm:1`) `[value][skip][else]`:
/// then-block when context byte +0x394 (PAL +0x398, the difficulty) ==
/// value.
pub fn block_context_byte398_8005497c(m: &mut Machine, at: u32) -> u32 {
    let value = m.lbu(at);
    let (next, at) = block_header(m, at.wrapping_add(1));
    let context = m.gw(CONTEXT);
    let current = m.lbu(context.wrapping_add(0x394));
    run_block(m, at, value != current, 0x18);
    next
}

/// 80037E50 (JP; PAL 80037B24, near, `del:7 imm:11 ins:4 op:3`): advance
/// the mission state machine and request the resident transition for the
/// current phase (mission byte +0x29) or pending flag (byte +0x2A bits
/// 0..2). See [`step`].
pub fn step_80037b24(m: &mut Machine) {
    step(m, None);
}

/// 80037E50 with the caller's `(s2, s1)` registers, which the original
/// passes on unchanged to 80032F18 when the phase is none of 0-5 or 9
/// (without them that undefined path raises `Fault::Untranslated` at
/// 80038030), in its 0x28-byte frame. JP differs from PAL in the phases:
/// 1 sets the result from the mission index (context half +0x38C) through
/// 80038054; 2 and 4 set the ending through JP 80038078 (index, byte +0x26;
/// PAL-only 80037D58 took the mission number), 4 stores 3 at context
/// +0x3A0 (PAL +0x3A4); 4, 5 and 9 record the result through JP 80048A40
/// (PAL-only 80047CF4); 5 copies context byte +0x3BD (PAL +0x3C1) to the
/// mission index and, when the index (PAL: its mission number) is 0x36,
/// sets context +0x3A9 = 5, half +0x38E = 0x19 (PAL 0x13) and +0x3AA/+0x3AB
/// = 1 (PAL +0x3AD..+0x3AF).
fn step(m: &mut Machine, caller: Option<(u32, u32)>) {
    const SIZE: u32 = 0x28;
    m.push_frame(SIZE);
    step_body(m, caller);
    m.pop_frame(SIZE);
}

fn mission_index(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    m.lh(context.wrapping_add(0x38c)) as i32 as u32
}

fn step_body(m: &mut Machine, caller: Option<(u32, u32)>) {
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
                let value = u32::from(m.lbu(state.wrapping_add(0x24)));
                let index = mission_index(m);
                set_result(m, index, value);
                (0x10, value)
            }
            2 => {
                let value = u32::from(m.lbu(state.wrapping_add(0x26)));
                let index = mission_index(m);
                dispatch::call(m, SET_ENDING, [index, value, 0, 0]);
                (0x4000, value)
            }
            3 => (0x400, u32::from(m.lbu(state.wrapping_add(0x25)))),
            4 => {
                let index = mission_index(m);
                let context = m.gw(CONTEXT);
                m.sw(context.wrapping_add(0x3a0), 3);
                let value = u32::from(m.lbu(state.wrapping_add(0x26)));
                dispatch::call(m, SET_ENDING, [index, value, 0, 0]);
                dispatch::call(m, RECORD_RESULT, [0; 4]);
                shared::stop_all_sound(m);
                (0x100_0000, 0)
            }
            5 => {
                dispatch::call(m, RECORD_RESULT, [0; 4]);
                let context = m.gw(CONTEXT);
                let next = m.lbu(context.wrapping_add(0x3bd));
                m.sh(context.wrapping_add(0x38c), u16::from(next));
                shared::stop_all_sound(m);
                let state = mission(m);
                if m.lbu(state.wrapping_add(0x29)) == 5 {
                    let context = m.gw(CONTEXT);
                    let index = m.lh(context.wrapping_add(0x38c));
                    if index == 0x36 {
                        m.sb(context.wrapping_add(0x3a9), 5);
                        let current = m.gw(CONTEXT);
                        m.sh(context.wrapping_add(0x38e), 0x19);
                        m.sb(current.wrapping_add(0x3aa), 1);
                        let current = m.gw(CONTEXT);
                        m.sb(current.wrapping_add(0x3ab), 1);
                    }
                }
                (0x200_0000, 0)
            }
            9 => {
                dispatch::call(m, RECORD_RESULT, [0; 4]);
                shared::stop_all_sound(m);
                (0x80_0000, 0)
            }
            _ => match caller {
                Some(registers) => registers,
                None => {
                    m.raise(Fault::Untranslated {
                        address: 0x8003_8030,
                    });
                    return;
                }
            },
        }
    };
    common::request_transition(m, first, second);
}

/// 80038054 (JP; PAL 80037D34, near, `imm:1`): context word +0x1038 (PAL
/// +0xF7C) = byte `0x800B0DA0[a0 * 4 + a1]` (JP passes the mission index,
/// PAL the mission number). Returns `v0` (the byte).
fn set_result(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    let at = a1.wrapping_add(a0 << 2).wrapping_add(0x800b_0da0);
    let context = m.gw(CONTEXT);
    let value = u32::from(m.lbu(at));
    m.sw(context.wrapping_add(0x1038), value);
    value
}

/// The routines this patch module claims instead of the generated crate
/// (`TAKES_OVER`), by JP address with the original `a0..a3`.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, _, _] = args;
    match target {
        0x8003_8054 => Some(set_result(m, a0, a1)),
        _ => None,
    }
}
