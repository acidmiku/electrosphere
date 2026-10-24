//! Flight replay (state 80033DA8, update 80032E34 / 8007E284) and other
//! resident routines no subsystem owns: the replay recorder/player over the
//! `[gp+0x54C]` buffer, the particle table at `[gp+0x514]`, the in-engine
//! sequence player of phases 0x10/0x20/0x40 (8003931C) with its stream,
//! command, sound and fade routines, and the helpers of the remaining
//! flight states.
//!
//! Translated from the original instructions in their original order.
//! Routines owned by other modules are called by original address through
//! [`dispatch::call`]; every routine translated here is claimed by
//! [`dispatch`]. Stack locals whose address the original passes on live in
//! the routine's guest frame.
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY, request_transition},
    dispatch,
    machine::{Fault, GP, Machine, SmallData},
    sdk,
};
use ac3_gte::{ctrl, reg};

/// `[gp+0x54C]`: replay buffer. +0 record count, +4/+8 ring start/end,
/// +0x10 first record, +0x14 frame-done flag, +0x18 entity snapshots (64 x
/// 0x20), +0x1018 frame, +0x101C cursor, +0x1020 entity copies (64 x 0x20),
/// +0x1820 camera offset, +0x1830 target offset, +0x1840..0x1848 camera
/// target state, +0x1850 saved entities (64 x 0x20).
const REPLAY: u32 = 0x54c;
/// `[gp+0x514]`: particle block (+0x33D0 active bit masks, +0x15D0 particles
/// of 0x3C bytes, +0xC8 emitters of 0xA4 bytes, +0x1548 emitter mask).
const PARTICLES: u32 = 0x514;
/// 80024278 / 800240D8: jump tables of 8007DB58 and 8006AF90.
const RECORD_CASES: u32 = 0x8002_4278;
const EMITTER_CASES: u32 = 0x8002_40d8;
/// 800B049C: stream table; 800B21B4: replay music sequences (12 bytes each).
const STREAMS: u32 = 0x800b_049c;
const REPLAY_MUSIC: u32 = 0x800b_21b4;

fn call(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, address, args)
}
fn replay_buffer(m: &mut Machine) -> u32 {
    m.gw(REPLAY)
}
fn entity(m: &mut Machine, index: u32) -> u32 {
    let table = m.gw(ENTITIES);
    m.lw((index << 2).wrapping_add(table))
}
/// 800270FC (shared): copy `words` words from `src` to `dst`.
fn copy_words(m: &mut Machine, dst: u32, src: u32, words: u32) {
    call(m, 0x8002_70fc, [dst, src, words, 0]);
}
/// Record size: low nibble of the header word, in words.
fn next_record(m: &mut Machine, record: u32) -> u32 {
    let header = m.lw(record);
    record.wrapping_add((header & 0xf) << 2)
}
/// Wrap a record cursor at the ring end (+8) back to the ring start (+4).
fn wrap_cursor(m: &mut Machine, cursor: u32) -> u32 {
    let buffer = replay_buffer(m);
    let end = m.lw(buffer + 8);
    if (cursor as i32) < (end as i32) {
        cursor
    } else {
        m.lw(buffer + 4)
    }
}
/// Inline `SetRotMatrix(matrix)`: five words into R11R12..R33.
fn set_rot_matrix(m: &mut Machine, matrix: u32) {
    for (i, register) in [
        ctrl::R11R12,
        ctrl::R13R21,
        ctrl::R22R23,
        ctrl::R31R32,
        ctrl::R33,
    ]
    .into_iter()
    .enumerate()
    {
        let word = m.lw(matrix.wrapping_add(4 * i as u32));
        m.gte.ctc2(register, word);
    }
}
/// `ldv0(vector)`, MVMVA (sf=1, RT x V0, no translation) and `stlvnl(out)`.
fn rotate_vector(m: &mut Machine, vector: u32, out: u32) {
    let xy = m.lw(vector);
    let z = m.lw(vector.wrapping_add(4));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    for (i, register) in [reg::MAC1, reg::MAC2, reg::MAC3].into_iter().enumerate() {
        let value = m.gte.mfc2(register);
        m.sw(out.wrapping_add(4 * i as u32), value);
    }
}

// ---- small resident helpers -------------------------------------------------------

/// 8002B0DC: stream state `[[gp+0x34]+0xC]` (5 reads as -1).
pub fn stream_state(m: &mut Machine) -> u32 {
    let block = m.gw(0x34);
    let state = m.lw(block.wrapping_add(0xc));
    if state == 5 { u32::MAX } else { state }
}

/// 8002C730: 1 when stream state is 1..3 and the 8002953C entry of `id`
/// is negative.
pub fn stream_entry_negative(m: &mut Machine, id: u32) -> u32 {
    let state = stream_state(m);
    if state.wrapping_sub(1) >= 3 {
        return 0;
    }
    let entry = call(m, 0x8002_953c, [id, 0, 0, 0]);
    u32::from((m.lw(entry) as i32) < 0)
}

// ---- camera views of the replay ------------------------------------------------

/// 80078010: point the camera at the player with mode 0x200, then
/// 80035248(0).
pub fn camera_replay_view(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(ENTITIES);
    let camera = m.lw(gameplay + 0x28);
    let player = m.lw(table);
    call(m, 0x8007_7d54, [camera, 0x200, player, 0]);
    call(m, 0x8003_5248, [0; 4]);
}

/// 80078048: camera +0x10C = position (3 words) and angles (3 halfwords at
/// +0x10) of `source`; camera +0x124 = 3 words of `offset`.
pub fn camera_set_eye(m: &mut Machine, source: u32, offset: u32) {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    let eye = camera.wrapping_add(0x10c);
    for i in 0..3 {
        let word = m.lw(source.wrapping_add(4 * i));
        m.sw(eye.wrapping_add(4 * i), word);
    }
    for offset16 in [0x10, 0x12, 0x14] {
        let half = m.lhu(source.wrapping_add(offset16));
        m.sh(eye.wrapping_add(offset16), half);
    }
    let base = camera.wrapping_add(0x124);
    for i in 0..3 {
        let word = m.lw(offset.wrapping_add(4 * i));
        m.sw(base.wrapping_add(4 * i), word);
    }
}

/// 800780C0: camera target `source`/`offset` with mode `mode` (camera +0x134
/// /+0x14C, +0x15C mode, +0x15E = 1 when it was 0); a null source or mode 0
/// only clears the mode.
pub fn camera_set_target(m: &mut Machine, source: u32, offset: u32, mode: u32) {
    if source == 0 || mode == 0 {
        let gameplay = m.gw(GAMEPLAY);
        let camera = m.lw(gameplay + 0x28);
        m.sh(camera.wrapping_add(0x15c), 0);
        return;
    }
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    let target = camera.wrapping_add(0x134);
    for i in 0..3 {
        let word = m.lw(source.wrapping_add(4 * i));
        m.sw(target.wrapping_add(4 * i), word);
    }
    for offset16 in [0x10, 0x12, 0x14] {
        let half = m.lhu(source.wrapping_add(offset16));
        m.sh(target.wrapping_add(offset16), half);
    }
    let base = camera.wrapping_add(0x14c);
    for i in 0..3 {
        let word = m.lw(offset.wrapping_add(4 * i));
        m.sw(base.wrapping_add(4 * i), word);
    }
    let camera = m.lw(gameplay + 0x28);
    if m.lh(camera.wrapping_add(0x15c)) == 0 {
        m.sh(camera.wrapping_add(0x15e), 1);
    }
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    m.sh(camera.wrapping_add(0x15c), mode as u16);
}

/// 8007E76C: allocate one object from a pool: `bitmap` has `words` mask
/// words, the objects (`size` words each, 32 per mask word) follow the
/// masks. Marks and returns the first free object, or 0 when full.
pub fn pool_alloc(m: &mut Machine, bitmap: u32, words: u32, size: u32) -> u32 {
    let mut object = bitmap.wrapping_add(words << 2);
    let mut at = bitmap;
    let mut index = 0i32;
    while index < words as i32 {
        let mask = m.lw(at);
        index += 1;
        if mask != u32::MAX {
            let mut bit = 1u32;
            while mask & bit != 0 {
                bit <<= 1;
                object = object.wrapping_add(size << 2);
            }
            m.sw(at, mask | bit);
            return object;
        }
        at = at.wrapping_add(4);
        object = object.wrapping_add(size << 7);
    }
    0
}

// ---- replay records ---------------------------------------------------------

/// 8007D4D4: effect record: 8005CD38(byte +4, byte +5, header >> 23).
pub fn record_effect_start(m: &mut Machine, record: u32) {
    let header = m.lw(record);
    let a = u32::from(m.lbu(record.wrapping_add(4)));
    let b = u32::from(m.lbu(record.wrapping_add(5)));
    call(m, 0x8005_cd38, [a, b, header >> 23, 0]);
}

/// 8007D504: effect record (rewind): 8005CD38(byte +4, byte +5,
/// header >> 14 & 0x1FF).
pub fn record_effect_rewind(m: &mut Machine, record: u32) {
    let header = m.lw(record);
    let a = u32::from(m.lbu(record.wrapping_add(4)));
    let b = u32::from(m.lbu(record.wrapping_add(5)));
    call(m, 0x8005_cd38, [a, b, (header >> 14) & 0x1ff, 0]);
}

/// 8007D538: replay one record while skipping (type 0 sets `*done` at a
/// frame end, type 4 effects run with `forward`); returns the next record.
pub fn record_skip(m: &mut Machine, record: u32, done: u32, forward: u32) -> u32 {
    m.sw(done, 0);
    let header = m.lw(record);
    let kind = (header >> 4) & 7;
    if kind == 0 {
        if (header >> 7) & 7 == 0 {
            m.sw(done, 1);
        }
    } else if kind == 4 {
        if forward != 0 {
            record_effect_start(m, record);
        } else {
            record_effect_rewind(m, record);
        }
    }
    next_record(m, record)
}

/// 8007D5D8: run every recorded frame through [`record_skip`] (effects only,
/// `forward` selects 8007D4D4 or 8007D504).
pub fn replay_skip_frames(m: &mut Machine, forward: u32) {
    let frame = m.push_frame(0x28);
    let done = frame + 0x10;
    let buffer = replay_buffer(m);
    let count = m.lw(buffer);
    let mut cursor = m.lw(buffer + 0x10);
    if (count as i32) > 0 {
        let mut index = 0u32;
        loop {
            m.sw(done, 0);
            loop {
                let record = wrap_cursor(m, cursor);
                cursor = record_skip(m, record, done, forward);
                if m.lw(done) != 0 {
                    break;
                }
            }
            let buffer = replay_buffer(m);
            index += 1;
            if (index as i32) >= (m.lw(buffer) as i32) {
                break;
            }
        }
    }
    m.pop_frame(0x28);
}

/// 8007DB58: replay one record (jump table 80024278 on the type: entity
/// position/state 8007C5C0/8007C6DC, particles 8007D31C, spawns 8007D9CC,
/// effects 8007D4D4); returns the next record.
pub fn record_play(m: &mut Machine, record: u32) -> u32 {
    let copies = replay_buffer(m).wrapping_add(0x1020);
    let kind = (m.lw(record) >> 4) & 7;
    if kind < 5 {
        let target = m.lw(RECORD_CASES + kind * 4);
        match target {
            0x8007_dbb0 => {
                call(m, 0x8007_c5c0, [record, copies, 0, 0]);
            }
            0x8007_dbc0 => {
                call(m, 0x8007_c6dc, [record, copies, 0, 0]);
            }
            0x8007_dbd0 => record_particles(m, record),
            0x8007_dbe0 => record_spawn(m, record),
            0x8007_dbf0 => record_effect_start(m, record),
            _ => m.raise(Fault::Untranslated { address: target }),
        }
    }
    next_record(m, record)
}

/// 8007D31C: particle record: emitter (8006AF90 / 8006AEE8 with positions
/// unpacked from 20.12 words) or stop (8006B1CC / 8006AF70).
pub fn record_particles(m: &mut Machine, record: u32) {
    let frame = m.push_frame(0x58);
    let header = m.lw(record);
    let index = (header >> 9) & 0x7f;
    match (header >> 7) & 3 {
        0 => {
            let a = m.lh(record.wrapping_add(2)) as i32 as u32;
            let b = m.lh(record.wrapping_add(6)) as i32 as u32;
            let c = m.lh(record.wrapping_add(4)) as i32 as u32;
            m.sw(frame + 0x28, a << 6);
            m.sw(frame + 0x2c, c << 6);
            m.sw(frame + 0x30, b << 6);
            let x = m.lw(record.wrapping_add(8));
            let y = m.lw(record.wrapping_add(0xc));
            let z = m.lw(record.wrapping_add(0x10));
            m.sh(frame + 0x24, (((z << 20) as i32) >> 18) as u16);
            m.sw(frame + 0x10, (((x as i32) >> 12) << 12) as u32);
            m.sw(frame + 0x14, (((y as i32) >> 12) << 12) as u32);
            m.sw(frame + 0x18, (((z as i32) >> 12) << 12) as u32);
            m.sh(frame + 0x20, (((x << 20) as i32) >> 18) as u16);
            m.sh(frame + 0x22, (((y << 20) as i32) >> 18) as u16);
            let header = m.lw(record);
            let kind = m.lw(record.wrapping_add(0x14));
            emitter_start(m, (header >> 9) & 0x7f, frame + 0x10, frame + 0x28, kind);
        }
        1 => particle_stop(m, index),
        2 => {
            let x = m.lw(record.wrapping_add(4));
            let y = m.lw(record.wrapping_add(8));
            let z = m.lw(record.wrapping_add(0xc));
            m.sw(frame + 0x38, (((x as i32) >> 12) << 12) as u32);
            m.sw(frame + 0x3c, (((y as i32) >> 12) << 12) as u32);
            m.sw(frame + 0x40, (((z as i32) >> 12) << 12) as u32);
            m.sh(frame + 0x48, (((x << 20) as i32) >> 18) as u16);
            m.sh(frame + 0x4a, (((y << 20) as i32) >> 18) as u16);
            m.sh(frame + 0x4c, (((z << 20) as i32) >> 18) as u16);
            let header = m.lw(record);
            let value = u32::from(m.lhu(record.wrapping_add(2)));
            emitter_set(m, (header >> 9) & 0x7f, value, frame + 0x38);
        }
        _ => emitter_stop(m, index),
    }
    m.pop_frame(0x58);
}

/// 8007E384: spawn request at `[gp+0x40C]+0x3948`: flag 1, halfwords a, b.
pub fn spawn_request(m: &mut Machine, a: u32, b: u32) {
    let block = m.gw(0x40c);
    let request = block.wrapping_add(0x3948);
    m.sw(request, 1);
    m.sh(request + 4, a as u16);
    m.sh(request + 6, b as u16);
}

/// 8007D9CC: spawn record (low nibble 1: 8007E384, 3: 8007E3A0, 4: 8007E4DC,
/// 5: 8007E658).
pub fn record_spawn(m: &mut Machine, record: u32) {
    let header = m.lw(record);
    match header & 0xf {
        1 => spawn_request(m, (header >> 9) & 0xff, (header >> 17) & 0xfff),
        3 => {
            let kind = u32::from(m.lbu(record.wrapping_add(4)));
            spawn_object(m, kind, record);
        }
        4 => {
            let kind = u32::from(m.lbu(record.wrapping_add(3)));
            spawn_debris(m, kind, record);
        }
        5 => {
            let kind = u32::from(m.lbu(record.wrapping_add(3)));
            spawn_smoke(m, kind, record);
        }
        _ => {}
    }
}

// ---- replay spawn records (8007D9CC types 3..5) ---------------------------------

/// 80024304 / 80024324: jump tables of 8007E3A0 and 8007E4DC.
const SPAWN_CASES: u32 = 0x8002_4304;
const DEBRIS_CASES: u32 = 0x8002_4324;

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// Effect pool at `[gp+0x40C] + offset` ([`pool_alloc`]).
fn effect_alloc(m: &mut Machine, offset: u32, words: u32, size: u32) -> u32 {
    let block = m.gw(0x40c);
    pool_alloc(m, block.wrapping_add(offset), words, size)
}

/// 8007E3A0: spawn record `kind` (jump table 80024304) at the record's
/// position (three 20.12 words, fraction cleared) in a guest stack vector.
pub fn spawn_object(m: &mut Machine, kind: u32, record: u32) {
    let frame = m.push_frame(0x28);
    let position = frame + 0x10;
    let x = m.lw(record);
    let y = m.lw(record.wrapping_add(4));
    let z = m.lw(record.wrapping_add(8));
    m.sw(position, sra(x, 12) << 12);
    m.sw(position + 4, sra(y, 12) << 12);
    m.sw(position + 8, sra(z, 12) << 12);
    if kind < 7 {
        let target = m.lw(SPAWN_CASES + kind * 4);
        let word1 = m.lw(record.wrapping_add(4));
        let word2 = m.lw(record.wrapping_add(8));
        let byte = (word2 >> 4) & 0xff;
        let call_args = match target {
            0x8007_e400 => Some((0x8007_e7e0, [position, byte, 0, 0])),
            0x8007_e41c => Some((0x8007_eaf8, [position, word2 & 0xf, byte, 0])),
            0x8007_e43c => Some((0x8008_0f4c, [position, (word1 >> 8) & 0xf, 0, 0])),
            0x8007_e458 => Some((0x8008_1738, [position, byte, 0, 0])),
            0x8007_e474 => Some((0x8008_8444, [position, byte, 0, 0])),
            0x8007_e490 => {
                spawn_variant(m, position, byte);
                None
            }
            0x8007_e4ac => Some((0x8008_25f4, [position, byte, (word1 >> 8) & 0xf, 0])),
            _ => {
                m.raise(Fault::Untranslated { address: target });
                None
            }
        };
        if let Some((routine, args)) = call_args {
            call(m, routine, args);
        }
    }
    m.pop_frame(0x28);
}

/// The three motion words of a debris/smoke record (+4, +8, +0xC).
fn motion_words(m: &mut Machine, record: u32) -> [u32; 3] {
    [
        m.lw(record.wrapping_add(4)),
        m.lw(record.wrapping_add(8)),
        m.lw(record.wrapping_add(0xc)),
    ]
}

/// 8007E4DC: debris record `kind` (jump table 80024324) with the position
/// (low 20 bits as x.12) and velocity (high 12 bits) in a guest frame.
pub fn spawn_debris(m: &mut Machine, kind: u32, record: u32) {
    let frame = m.push_frame(0x38);
    let (position, velocity) = (frame + 0x10, frame + 0x20);
    let words = motion_words(m, record);
    for (i, word) in words.into_iter().enumerate() {
        m.sw(position + 4 * i as u32, word << 12);
    }
    let words = motion_words(m, record);
    let a2 = sra(words[0], 20);
    let a3 = sra(words[1], 20);
    m.sw(velocity, a2 << 12);
    m.sw(velocity + 4, a3 << 12);
    m.sw(velocity + 8, sra(words[2], 20) << 12);
    if kind < 10 {
        let target = m.lw(DEBRIS_CASES + kind * 4);
        match target {
            0x8007_e564 => debris_burst(m, position, velocity),
            0x8007_e578 | 0x8007_e590 | 0x8007_e5fc => {
                let routine = match target {
                    0x8007_e578 => 0x8007_f928,
                    0x8007_e590 => 0x8008_0580,
                    _ => 0x8008_1198,
                };
                let byte = u32::from(m.lbu(record.wrapping_add(2)));
                call(m, routine, [position, velocity, byte, a3]);
            }
            0x8007_e5a8 => {
                call(m, 0x8008_0704, [position, velocity, a2, a3]);
            }
            0x8007_e5bc => {
                let header = m.lw(record);
                call(
                    m,
                    0x8008_0994,
                    [position, velocity, (header >> 9) & 0x7f, a3],
                );
            }
            0x8007_e5dc => {
                let header = m.lw(record);
                debris_explosion(m, position, velocity, (header >> 9) & 0x7f);
            }
            0x8007_e614 => {
                call(m, 0x8008_13f4, [position, velocity, a2, a3]);
            }
            0x8007_e628 => {
                call(m, 0x8008_15d4, [position, velocity, a2, a3]);
            }
            0x8007_e63c => debris_column(m, position, velocity),
            _ => m.raise(Fault::Untranslated { address: target }),
        }
    }
    m.pop_frame(0x38);
}

/// 8007E658: smoke record `kind` 0 (8007F15C, size packed from word +0x10)
/// or 1 (80081A0C with the velocity scaled by `1 << (word+0x10 & 0xFFF)`
/// and moved to the position).
pub fn spawn_smoke(m: &mut Machine, kind: u32, record: u32) {
    let frame = m.push_frame(0x38);
    let (position, velocity) = (frame + 0x10, frame + 0x20);
    let words = motion_words(m, record);
    let base = words.map(|word| word << 12);
    for (i, value) in base.into_iter().enumerate() {
        m.sw(position + 4 * i as u32, value);
    }
    let words = motion_words(m, record);
    let speed = words.map(|word| sra(word, 20) << 12);
    m.sw(velocity, speed[0]);
    m.sw(velocity + 8, speed[2]);
    m.sw(velocity + 4, speed[1]);
    if kind == 0 {
        let size = m.lw(record.wrapping_add(0x10));
        let packed = (size & 0xfff) | (((size >> 12) & 0xfff) << 16);
        smoke_trail(m, position, velocity, packed);
    } else if kind == 1 {
        let size = m.lw(record.wrapping_add(0x10));
        let byte = u32::from(m.lbu(record.wrapping_add(2)));
        let shift = size & 0xfff;
        let scale = (size >> 12) & 0xfff;
        if shift != 0 {
            for (i, value) in speed.into_iter().enumerate() {
                m.sw(velocity + 4 * i as u32, value << (shift & 31));
            }
        }
        let vx = m.lw(velocity);
        let vz = m.lw(velocity + 8);
        m.sw(velocity, vx.wrapping_add(base[0]));
        let vy = m.lw(velocity + 4);
        m.sw(velocity + 8, vz.wrapping_add(base[2]));
        m.sw(velocity + 4, vy.wrapping_add(base[1]));
        call(m, 0x8008_1a0c, [position, velocity, scale, byte]);
    }
    m.pop_frame(0x38);
}

/// 8007F86C: call 8007F44C (`variant` even) or 8007F5C0 (odd) with the
/// three position words.
pub fn spawn_variant(m: &mut Machine, position: u32, variant: u32) {
    let routine = if variant & 1 == 0 {
        0x8007_f44c
    } else {
        0x8007_f5c0
    };
    let x = m.lw(position);
    let y = m.lw(position.wrapping_add(4));
    let z = m.lw(position.wrapping_add(8));
    call(m, routine, [x, y, z, 0]);
}

/// The flash (pool +0x600) and light (pool +0x33C0, random lifetime and
/// size; 8007F15C also stores `light`) of 8007EEEC and 8007F15C.
fn flash_and_light(m: &mut Machine, x: u32, y: u32, z: u32, light: Option<u32>) {
    let flash = effect_alloc(m, 0x600, 3, 7);
    if flash != 0 {
        m.sw(flash, 0x000e_ffff);
        m.sw(flash + 4, x);
        m.sw(flash + 8, y);
        m.sw(flash + 0xc, z);
        m.sh(flash + 0x16, 0);
        m.sw(flash + 0x18, 0x0100_0100);
    }
    let glow = effect_alloc(m, 0x33c0, 1, 6);
    if glow != 0 {
        let random = sdk::rand(m);
        let life = (random & 3) + 9;
        let size = (random & 0x3f) + 0x190;
        if let Some(value) = light {
            m.sh(glow + 2, life as u16);
            m.sh(glow, 0);
            m.sw(glow + 4, x);
            m.sw(glow + 8, y);
            m.sw(glow + 0xc, z);
            m.sw(glow + 0x10, value);
            m.sh(glow + 0x14, 1);
        } else {
            m.sh(glow, 0);
            m.sh(glow + 2, life as u16);
            m.sw(glow + 4, x);
            m.sw(glow + 8, y);
            m.sw(glow + 0xc, z);
            m.sh(glow + 0x14, 0);
        }
        m.sh(glow + 0x16, size as u16);
    }
}

/// 8007EEEC: explosion burst: flash, light and eight debris pieces (pool
/// +0x1098) scattered around `position` with the scaled `velocity`.
pub fn debris_burst(m: &mut Machine, position: u32, velocity: u32) {
    let x = m.lw(position);
    let y = m.lw(position.wrapping_add(4));
    let z = m.lw(position.wrapping_add(8));
    flash_and_light(m, x, y, z, None);
    let vx = sra(m.lw(velocity), 9);
    let vy = sra(m.lw(velocity.wrapping_add(4)), 9);
    let vz = sra(m.lw(velocity.wrapping_add(8)), 9).wrapping_sub(0x80);
    for piece in 0..8u32 {
        let debris = effect_alloc(m, 0x1098, 0xa, 7);
        if debris == 0 {
            return;
        }
        m.sh(debris, 0);
        m.sh(debris + 2, (piece + 0x20) as u16);
        let random = sdk::rand(m);
        let spin = (random >> 8) | (random << 7);
        let dx = ((random & 0xff).wrapping_sub(0x80) << 8).wrapping_add(x);
        let dy = (((random >> 3) & 0xff).wrapping_sub(0x80) << 8).wrapping_add(y);
        let dz = (((random >> 6) & 0xff).wrapping_sub(0x80) << 8).wrapping_add(z);
        m.sw(debris + 4, dx);
        let sx = vx.wrapping_sub(0x80).wrapping_add(spin & 0xff);
        m.sh(debris + 0x10, sx as u16);
        let sy = ((spin >> 3) & 0xff).wrapping_sub(0x100).wrapping_add(vy);
        m.sh(debris + 0x12, sy as u16);
        let sz = ((spin >> 6) & 0xff).wrapping_add(vz);
        m.sh(debris + 0x14, sz as u16);
        m.sw(debris + 8, dy);
        m.sw(debris + 0xc, dz);
        let last = if piece < 2 {
            m.sh(debris + 0x16, 0x93);
            m.sh(debris + 0x18, 0x1504);
            (spin & 0x1f) + 0xf0
        } else {
            m.sh(debris + 0x16, 0x188);
            m.sh(debris + 0x18, 0);
            piece + 0x14
        };
        m.sh(debris + 0x1a, last as u16);
    }
}

/// 8007F15C: smoke trail: flash, light (`packed` size) and eight pieces
/// thrown against `velocity`.
pub fn smoke_trail(m: &mut Machine, position: u32, velocity: u32, packed: u32) {
    let x = m.lw(position);
    let y = m.lw(position.wrapping_add(4));
    let z = m.lw(position.wrapping_add(8));
    flash_and_light(m, x, y, z, Some(packed));
    let vx = sra(0u32.wrapping_sub(m.lw(velocity)), 11);
    let vy = sra(0u32.wrapping_sub(m.lw(velocity.wrapping_add(4))), 11);
    let vz = sra(0u32.wrapping_sub(m.lw(velocity.wrapping_add(8))), 11);
    for piece in 0..8u32 {
        let debris = effect_alloc(m, 0x1098, 0xa, 7);
        if debris == 0 {
            return;
        }
        m.sh(debris, 0);
        m.sh(debris + 2, (piece + 0x18) as u16);
        let random = sdk::rand(m);
        let spin = (random >> 8) | (random << 7);
        let dx = ((random & 0xff).wrapping_sub(0x80) << 9).wrapping_add(x);
        let dy = (((random >> 3) & 0xff).wrapping_sub(0x80) << 9).wrapping_add(y);
        let dz = (((random >> 6) & 0xff).wrapping_sub(0x80) << 9).wrapping_add(z);
        let sx = ((spin & 0xff).wrapping_sub(0x80) << 1).wrapping_add(vx);
        m.sh(debris + 0x10, sx as u16);
        let sy = (sra(spin, 2) & 0x1fe).wrapping_sub(0x130).wrapping_add(vy);
        m.sh(debris + 0x12, sy as u16);
        let sz = (((spin >> 6) & 0xff).wrapping_sub(0x80) << 1).wrapping_add(vz);
        m.sh(debris + 0x14, sz as u16);
        m.sw(debris + 4, dx);
        m.sw(debris + 8, dy);
        m.sw(debris + 0xc, dz);
        if piece < 2 {
            m.sh(debris + 0x16, 0x8b);
            m.sh(debris + 0x18, 0x1504);
            let rise = m.lhu(debris + 0x12);
            m.sh(debris + 0x1a, ((spin & 0x1f) + 0xf0) as u16);
            m.sh(debris + 0x12, rise.wrapping_sub(0x80));
        } else {
            m.sh(debris + 0x16, 0x188);
            m.sh(debris + 0x18, 0);
            m.sh(debris + 0x1a, (piece + 0x14) as u16);
        }
    }
}

/// 80080C5C: explosion at `position` + `velocity` + random jitter: with
/// `mode` 1 an 80081738 blast, otherwise a flash and a fireball (pool
/// +0x600); on odd random values also one debris piece (pool +0x1098).
pub fn debris_explosion(m: &mut Machine, position: u32, velocity: u32, mode: u32) {
    let mut vx = m.lw(velocity);
    let mut vy = m.lw(velocity.wrapping_add(4));
    let mut vz = m.lw(velocity.wrapping_add(8));
    let jitter = |random: u32| ((random & 0x3f).wrapping_sub(0x20)) << 12;
    let random = sdk::rand(m);
    let x = m.lw(position).wrapping_add(vx).wrapping_add(jitter(random));
    let random = sdk::rand(m);
    vx = sra(vx, 9);
    let y = m
        .lw(position.wrapping_add(4))
        .wrapping_add(vy)
        .wrapping_add(jitter(random));
    let random = sdk::rand(m);
    vy = sra(vy, 9);
    let z = m
        .lw(position.wrapping_add(8))
        .wrapping_add(vz)
        .wrapping_add(jitter(random));
    vz = sra(vz, 9);
    let random = sdk::rand(m);
    if mode == 1 {
        call(m, 0x8008_1738, [position, 0x200, mode, 0]);
    } else {
        let flash = effect_alloc(m, 0x600, 3, 7);
        if flash != 0 {
            m.sw(flash, 0x0010_0000);
            m.sh(flash + 0x18, 0x100);
            m.sw(flash + 4, x);
            m.sw(flash + 8, y);
            m.sw(flash + 0xc, z);
            m.sh(flash + 0x16, 0);
            m.sh(flash + 0x1a, ((random & 0xf) + 0x5a) as u16);
        }
        let fire = effect_alloc(m, 0x600, 3, 7);
        if fire != 0 {
            m.sw(fire, 0x0064_0000);
            let sx = 0u32
                .wrapping_sub(vx)
                .wrapping_sub(0x40)
                .wrapping_add(random & 0x7f);
            m.sh(fire + 0x10, sx as u16);
            let sy = ((random >> 3) & 0x7f).wrapping_sub(0xb0);
            let sz = 0u32
                .wrapping_sub(vz)
                .wrapping_sub(0x40)
                .wrapping_add((random >> 6) & 0x7f);
            m.sh(fire + 0x12, sy as u16);
            m.sh(fire + 0x16, 0x80);
            m.sh(fire + 0x18, 0x210);
            m.sw(fire + 4, x);
            m.sw(fire + 8, y);
            m.sw(fire + 0xc, z);
            m.sh(fire + 0x14, sz as u16);
            m.sh(fire + 0x1a, ((random & 0x3f) + 0x44) as u16);
        }
    }
    if random & 1 == 0 {
        return;
    }
    let debris = effect_alloc(m, 0x1098, 0xa, 7);
    if debris == 0 {
        return;
    }
    m.sw(debris, 0x0018_0000);
    m.sw(debris + 4, x);
    m.sw(debris + 8, y);
    m.sw(debris + 0xc, z);
    let random = sdk::rand(m);
    let sx = 0u32
        .wrapping_sub(vx)
        .wrapping_sub(0x80)
        .wrapping_add(random & 0xff);
    let sy = 0u32
        .wrapping_sub(vy)
        .wrapping_sub(0x80)
        .wrapping_add((random >> 3) & 0xff);
    m.sh(debris + 0x10, sx as u16);
    m.sh(debris + 0x12, sy as u16);
    let sz = 0u32
        .wrapping_sub(vz)
        .wrapping_sub(0x80)
        .wrapping_add((random >> 6) & 0xff);
    m.sh(debris + 0x16, 0x188);
    m.sh(debris + 0x14, sz as u16);
    m.sh(debris + 0x18, 0);
    m.sh(debris + 0x1a, ((random & 7) + 0x10) as u16);
}

/// 80081D80: water column at `position` + 8 x `velocity` (block
/// `[gp+0x40C]+0x3B28`), clipped against the 80098DB0 hit test.
pub fn debris_column(m: &mut Machine, position: u32, velocity: u32) {
    // One implementation: effects.rs owns 80081D80.
    crate::dispatch::call(m, 0x8008_1d80, [position, velocity, 0, 0]);
}

// ---- particles -----------------------------------------------------------------

fn particle(m: &mut Machine, index: u32) -> u32 {
    let block = m.gw(PARTICLES);
    block
        .wrapping_add(index.wrapping_mul(0x3c))
        .wrapping_add(0x15d0)
}

/// 8006B1CC: clear particle `index` word +4 (its lifetime).
pub fn particle_stop(m: &mut Machine, index: u32) {
    let block = m.gw(PARTICLES);
    let at = index.wrapping_mul(0x3c).wrapping_add(block);
    m.sw(at.wrapping_add(0x15d4), 0);
}

/// 8006AEE8: emitter `index` on (mask +0x1548), parameter `value` (two
/// bytes), position (3 words) and angles (3 halfwords at +0x10) of `source`.
pub fn emitter_set(m: &mut Machine, index: u32, value: u32, source: u32) {
    let block = m.gw(PARTICLES);
    let emitter = block
        .wrapping_add(0xc8)
        .wrapping_add(index.wrapping_mul(0xa4));
    let mask = m.lw(block.wrapping_add(0x1548));
    m.sw(block.wrapping_add(0x1548), mask | (1 << (index & 31)));
    m.sb(emitter + 0x34, value as u8);
    m.sb(emitter + 0x35, ((value as i32) >> 8) as u8);
    for (i, offset) in [0x1c, 0x20, 0x24].into_iter().enumerate() {
        let word = m.lw(source.wrapping_add(4 * i as u32));
        m.sw(emitter + offset, word);
    }
    for (from, to) in [(0x10, 0x2c), (0x12, 0x2e), (0x14, 0x30)] {
        let half = m.lhu(source.wrapping_add(from));
        m.sh(emitter + to, half);
    }
}

/// 8006AF70: emitter `index` off (mask +0x1548).
pub fn emitter_stop(m: &mut Machine, index: u32) {
    let block = m.gw(PARTICLES);
    let mask = m.lw(block.wrapping_add(0x1548));
    m.sw(block.wrapping_add(0x1548), mask & !(1 << (index & 31)));
}

/// 8006AF90: start particle `index` of `kind` (jump table 800240D8) at
/// `position` (3 words, angles at +0x10) with velocity `velocity`: the
/// launch offset is the kind's vector rotated by the angles.
pub fn emitter_start(m: &mut Machine, index: u32, position: u32, velocity: u32, kind: u32) {
    let frame = m.push_frame(0x60);
    let block = m.gw(PARTICLES);
    let mask = block
        .wrapping_add(0x33d0)
        .wrapping_add((((index as i32) >> 5) << 2) as u32);
    let bits = m.lw(mask);
    m.sw(mask, bits | (1 << (index & 0x1f)));
    call(
        m,
        0x8003_0534,
        [position.wrapping_add(0x10), frame + 0x10, 0, 0],
    );
    set_rot_matrix(m, frame + 0x10);
    let particle = particle(m, index);
    if kind < 6 {
        let target = m.lw(EMITTER_CASES + kind * 4);
        let (flags, life, z) = match target {
            0x8006_b05c => (0x0002_0410, 0x25, 0x800),
            0x8006_b070 => (0x0002_0820, 0x18, 0x990),
            0x8006_b08c => (0x0004_1040, 0x3e, 0x4c0),
            0x8006_b0a8 => (0x0008_2080, 0x23, 0),
            0x8006_b0bc => (0x0009_4103, 0x10, 0),
            0x8006_b0e0 => (0x0009_8202, 0x10, 0x800),
            _ => {
                m.raise(Fault::Untranslated { address: target });
                (0, 0, 0)
            }
        };
        m.sw(particle, flags);
        m.sw(particle + 4, life);
        m.sh(frame + 0x40, 0);
        m.sh(frame + 0x42, 0);
        m.sh(frame + 0x44, z);
    }
    // Other kinds keep whatever the guest frame holds, as the original does.
    rotate_vector(m, frame + 0x40, frame + 0x30);
    for (i, offset) in [8, 0xc, 0x10].into_iter().enumerate() {
        let word = m.lw(position.wrapping_add(4 * i as u32));
        m.sw(particle + offset, word);
    }
    for (i, (from, to)) in [(8, 0x18), (0xc, 0x1c), (0x10, 0x20)]
        .into_iter()
        .enumerate()
    {
        let offset = m.lw(frame + 0x30 + 4 * i as u32);
        let base = m.lw(particle + from);
        m.sw(particle + to, base.wrapping_add(offset << 8));
    }
    for (i, offset) in [0x28, 0x2c, 0x30].into_iter().enumerate() {
        let word = m.lw(velocity.wrapping_add(4 * i as u32));
        m.sw(particle + offset, word);
    }
    m.pop_frame(0x60);
}

/// 8006B1E8: advance the 128 particles: lifetime, position (+8 and +0x18
/// by the velocity), gravity (0x444 on y unless flag 2), and GTE drag
/// (GPF by IR0 = 0xFA0) unless flag 1.
pub fn particles_update(m: &mut Machine) {
    let block = m.gw(PARTICLES);
    let mut masks = block.wrapping_add(0x33d0);
    m.gte.mtc2(reg::IR0, 0xfa0);
    for group in (0..0x80u32).step_by(0x20) {
        let mut active = m.lw(masks);
        if active != 0 {
            for bit in 0..0x20u32 {
                let flag = 1u32 << bit;
                if active & flag == 0 {
                    continue;
                }
                let particle = particle(m, group + bit);
                let life = m.lw(particle + 4).wrapping_sub(1);
                m.sw(particle + 4, life);
                if (life as i32) < 0 {
                    active &= !flag;
                    continue;
                }
                let mut vx = m.lw(particle + 0x28);
                let mut vy = m.lw(particle + 0x2c);
                let x = m.lw(particle + 8);
                let mut vz = m.lw(particle + 0x30);
                let z = m.lw(particle + 0x10);
                m.sw(particle + 8, x.wrapping_add(vx));
                let y = m.lw(particle + 0xc);
                m.sw(particle + 0x10, z.wrapping_add(vz));
                let tail_y = m.lw(particle + 0x1c);
                m.sw(particle + 0xc, y.wrapping_add(vy));
                let tail_x = m.lw(particle + 0x18);
                m.sw(particle + 0x1c, tail_y.wrapping_add(vy));
                let flags = !m.lw(particle);
                m.sw(particle + 0x18, tail_x.wrapping_add(vx));
                let tail_z = m.lw(particle + 0x20);
                m.sw(particle + 0x20, tail_z.wrapping_add(vz));
                if flags & 2 != 0 {
                    vy = vy.wrapping_add(0x444);
                }
                vx = ((vx as i32) >> 6) as u32;
                vy = ((vy as i32) >> 6) as u32;
                vz = ((vz as i32) >> 6) as u32;
                if flags & 1 != 0 {
                    m.gte.mtc2(reg::IR1, vx);
                    m.gte.mtc2(reg::IR2, vy);
                    m.gte.mtc2(reg::IR3, vz);
                    m.gte.gpf(true, false);
                    vx = m.gte.mfc2(reg::IR1);
                    vy = m.gte.mfc2(reg::IR2);
                    vz = m.gte.mfc2(reg::IR3);
                }
                m.sw(particle + 0x28, vx << 6);
                m.sw(particle + 0x2c, vy << 6);
                m.sw(particle + 0x30, vz << 6);
            }
            m.sw(masks, active);
        }
        masks = masks.wrapping_add(4);
    }
}

// ---- replay control ---------------------------------------------------------------

/// 8007DC1C: rewind the replay: reset the frame/cursor and camera state,
/// compute the initial camera offset (0, 0, 0x4800) rotated by the angles at
/// +0x1038 (x8), reset the effect lists, rings and slots, restore the entity
/// snapshots (+0x18) into the copies and the entities, and set the camera.
pub fn replay_rewind(m: &mut Machine) {
    let frame = m.push_frame(0x48);
    let buffer = replay_buffer(m);
    let first = m.lw(buffer + 0x10);
    m.sw(buffer + 0x1018, 0);
    m.sw(buffer + 0x1840, u32::MAX);
    m.sw(buffer + 0x1844, u32::MAX);
    m.sw(buffer + 0x1848, 0);
    m.sw(buffer + 0x101c, first);
    sdk::memset(m, frame + 0x10, 0, 8);
    let buffer = replay_buffer(m);
    m.sh(frame + 0x14, 0x4800);
    call(m, 0x8003_0534, [buffer + 0x1038, frame + 0x18, 0, 0]);
    set_rot_matrix(m, frame + 0x18);
    let offset = replay_buffer(m).wrapping_add(0x1820);
    rotate_vector(m, frame + 0x10, offset);
    let buffer = replay_buffer(m);
    let x = m.lw(buffer + 0x1820);
    let z = m.lw(buffer + 0x1828);
    m.sw(buffer + 0x1830, 0);
    m.sw(buffer + 0x1834, 0);
    m.sw(buffer + 0x1838, 0);
    m.sw(buffer + 0x1820, x << 3);
    let y = m.lw(buffer + 0x1824);
    m.sw(buffer + 0x1828, z << 3);
    m.sw(buffer + 0x1824, y << 3);
    call(m, 0x8008_33a4, [0; 4]);
    call(m, 0x8007_c3a0, [0; 4]);
    call(m, 0x8006_ad60, [0; 4]);
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        copy_words(m, buffer + 0x1020 + i * 0x20, buffer + 0x18 + i * 0x20, 8);
    }
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        let target = entity(m, i);
        copy_words(m, target, buffer + 0x18 + i * 0x20, 8);
    }
    let player = entity(m, 0);
    let buffer = replay_buffer(m);
    camera_set_eye(m, player.wrapping_add(8), buffer + 0x1820);
    camera_set_target(m, 0, 0, 0);
    m.pop_frame(0x48);
}

/// 8007DF54: play one replay frame; returns 1 once every recorded frame
/// was played (else 0).
pub fn replay_frame(m: &mut Machine) -> u32 {
    let buffer = replay_buffer(m);
    let played = m.lw(buffer + 0x1018);
    if (played as i32) >= (m.lw(buffer) as i32) {
        return 1;
    }
    let target = m.lw(buffer + 0x1840);
    let mut cursor = m.lw(buffer + 0x101c);
    m.sw(buffer + 0x1018, played.wrapping_add(1));
    m.sw(buffer + 0x14, 0);
    loop {
        let record = wrap_cursor(m, cursor);
        cursor = record_play(m, record);
        let buffer = replay_buffer(m);
        if m.lw(buffer + 0x14) != 0 {
            break;
        }
    }
    let buffer = replay_buffer(m);
    let current = m.lw(buffer + 0x1840);
    m.sw(buffer + 0x101c, cursor);
    if target != current {
        m.sw(buffer + 0x1848, 0x96);
        if target == u32::MAX {
            let current = m.lw(buffer + 0x1840);
            m.sw(buffer + 0x1844, current);
        }
    }
    let buffer = replay_buffer(m);
    let delay = m.lw(buffer + 0x1848);
    if delay != 0 {
        m.sw(buffer + 0x1848, delay.wrapping_sub(1));
    } else {
        let current = m.lw(buffer + 0x1840);
        m.sw(buffer + 0x1844, current);
    }
    // Camera offset: the player's recorded copy minus the live player.
    let table = m.gw(ENTITIES);
    let player = m.lw(table);
    let buffer = replay_buffer(m);
    let px = m.lw(player + 8);
    let py = m.lw(player + 0xc);
    let pz = m.lw(player + 0x10);
    let copy = buffer + 0x1020;
    let cx = m.lw(copy + 8);
    let cy = m.lw(copy + 0xc);
    let cz = m.lw(copy + 0x10);
    let followed = m.lw(buffer + 0x1844);
    m.sw(buffer + 0x1820, cx.wrapping_sub(px));
    m.sw(buffer + 0x1824, cy.wrapping_sub(py));
    m.sw(buffer + 0x1828, cz.wrapping_sub(pz));
    if (followed as i32) >= 0 {
        let target = m.lw((followed << 2).wrapping_add(table));
        let tx = m.lw(target + 8);
        let ty = m.lw(target + 0xc);
        let tz = m.lw(target + 0x10);
        let copy = buffer.wrapping_add((followed << 5).wrapping_add(0x1020));
        let cx = m.lw(copy + 8);
        let cz = m.lw(copy + 0x10);
        m.sw(buffer + 0x1830, cx.wrapping_sub(tx));
        m.sw(buffer + 0x1838, cz.wrapping_sub(tz));
        let cy = m.lw(copy + 0xc);
        m.sw(buffer + 0x1834, cy.wrapping_sub(ty));
    }
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        let target = entity(m, i);
        copy_words(m, target, buffer + 0x1020 + i * 0x20, 8);
    }
    let player = entity(m, 0);
    let buffer = replay_buffer(m);
    camera_set_eye(m, player.wrapping_add(8), buffer + 0x1820);
    let buffer = replay_buffer(m);
    let followed = m.lw(buffer + 0x1844);
    let (source, mode) = if (followed as i32) < 0 {
        (0, 0)
    } else {
        let target = entity(m, followed);
        let source = target.wrapping_add(8);
        let mode = if (followed as i32) < 0x20 {
            1
        } else {
            match m.lh(target + 0x48) {
                0x32 | 0x33 | 0x3d | 0x4d => 1,
                _ => 2,
            }
        };
        (source, mode)
    };
    let buffer = replay_buffer(m);
    camera_set_target(m, source, buffer + 0x1830, mode);
    0
}

/// Save the 64 entities into the replay buffer (+0x1850) with 800270FC.
fn save_entities(m: &mut Machine) {
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        let source = entity(m, i);
        copy_words(m, buffer + 0x1850 + i * 0x20, source, 8);
    }
}

/// Restore the 64 entities from the replay buffer (+0x1850) with 800270FC.
fn restore_entities(m: &mut Machine) {
    for i in 0..0x40u32 {
        let buffer = replay_buffer(m);
        let target = entity(m, i);
        copy_words(m, target, buffer + 0x1850 + i * 0x20, 8);
    }
}

/// 8007DDCC: start the replay (phase 0x80): clear the status bytes, block
/// pausing, queue the replay music (the stage's stream when its entry is
/// not negative, else sequence 800B21B4 + 12 * (context +0x390 mod 6 + 1)),
/// reset slots, set the replay camera, save the entities, rewind, skip the
/// effects of every frame, leave bit 0x10000000 of gameplay and request
/// transition 0x100.
pub fn replay_start(m: &mut Machine) {
    // 80028F5C (boot.rs): clear the six slot bytes at `[gp+0x30]`.
    call(m, 0x8002_8f5c, [0; 4]);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    let mode = m.lbu(context + 0x3a9);
    m.sw(context, flags | 0x1000_0000);
    if mode == 0 {
        call(m, 0x8002_c4b0, [REPLAY_MUSIC, 0, 0, 0]);
    } else {
        let stage = m.lh(context + 0x390) as i32 as u32;
        let stream = call(m, 0x8005_50d8, [stage, 0, 0, 0]);
        let mut sequence = m.lw((stream << 2).wrapping_add(STREAMS));
        let id = m.lh(sequence) as i32 as u32;
        if stream_entry_negative(m, id) == 0 {
            let context = m.gw(CONTEXT);
            let value = i32::from(m.lh(context + 0x390));
            let high = ((i64::from(value) * 0x2aaa_aaab) >> 32) as i32;
            let quotient = high.wrapping_sub(value >> 31);
            let remainder = value.wrapping_sub(quotient.wrapping_mul(6));
            let index = i32::from(remainder as i16) + 1;
            sequence = (index.wrapping_mul(12) as u32).wrapping_add(REPLAY_MUSIC);
        }
        let context = m.gw(CONTEXT);
        if m.lbu(context + 0x3a9) < 3 {
            call(m, 0x8002_c4b0, [sequence, 0, 0, 0]);
        }
    }
    call(m, 0x8006_ad60, [0; 4]);
    camera_replay_view(m);
    save_entities(m);
    replay_rewind(m);
    replay_skip_frames(m, 0);
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    let stage = m.lw(gameplay + 8);
    m.sw(gameplay, flags & 0xefff_ffff);
    request_transition(m, 0x100, stage);
}

/// 8007E1A8: end the replay (phase 0x200): flush the sequencer, rewind,
/// replay the effects forward, clear the buffer counters, allow pausing,
/// restore the entities and slots, then either block pausing again (replay
/// mode) or run the mission step 80037B24.
pub fn replay_finish(m: &mut Machine) {
    call(m, 0x8002_c2f4, [0; 4]);
    replay_rewind(m);
    replay_skip_frames(m, 1);
    let buffer = replay_buffer(m);
    let context = m.gw(CONTEXT);
    m.sw(buffer, 0);
    m.sw(buffer + 0x1018, 0);
    let flags = m.lw(context);
    m.sw(context, flags & 0xefff_ffff);
    restore_entities(m);
    call(m, 0x8006_ad60, [0; 4]);
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3a9) != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let flags = m.lw(gameplay);
        m.sw(gameplay, flags | 0x1000_0000);
    } else {
        call(m, 0x8003_7b24, [0; 4]);
    }
}

// ---- in-engine sequences (phase 0x10 / 0x20 / 0x40, update 8003931C) --------------
//
// gp+0x3DC current record, +0x3E0 fade state, +0x3E4 frame count, +0x3E8
// playing, +0x3EC skip requested, +0x3F0 frames left (halfword), +0x3F4
// stream finished, +0x3FC/+0x400 stream chunk / header, +0x404/+0x406 record
// count / index (halfwords), +0x408 wait counter. Context +0xF7C is the
// sequence mode (1..; 0 is no sequence).

/// 800AFA3C: object slot of each sequence mode (byte per mode).
const SEQUENCE_OBJECTS: u32 = 0x800a_fa3c;
/// 800AFA00: stream file of each sequence mode (halfword per mode).
const SEQUENCE_STREAMS: u32 = 0x800a_fa00;
/// 800AF9DC: sequence command handlers, by opcode (header & 0x7F).
const SEQUENCE_COMMANDS: u32 = 0x800a_f9dc;
/// 8002398C: jump table of command 5 (80038F38), by (header >> 8) & 0x1F.
const EFFECT_CASES: u32 = 0x8002_398c;
/// 800C43D8: sound source count; 800C43E8: listener (4 words); 800C43F8:
/// sound sources of 0x5C bytes.
const SOUND_SOURCE_COUNT: u32 = 0x800c_43d8;
const SOUND_LISTENER: u32 = 0x800c_43e8;
const SOUND_SOURCES: u32 = 0x800c_43f8;
/// 800AE7CC: music table, 20 bytes per entry.
const MUSIC_TABLE: u32 = 0x800a_e7cc;

/// The sequence object of the current mode: word 0 is its frame count, the
/// commands of its final state follow at +4.
fn sequence_object(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let mode = m.lw(context + 0xf7c);
    let gameplay = m.gw(GAMEPLAY);
    let slot = u32::from(m.lbu(mode.wrapping_add(SEQUENCE_OBJECTS)));
    let table = m.lw(gameplay + 0x26c);
    m.lw((slot << 2).wrapping_add(table))
}

/// 8002C500: queue a type-4 (stream) event in the event ring `[gp+0x48]`
/// (16 entries of 20 bytes from +8; +4 counts queued events). The fifth
/// argument (`callback`) is the caller's stack word +0x10. Returns 4.
pub fn stream_queue(
    m: &mut Machine,
    file: u32,
    buffer: u32,
    size: u32,
    chunk: u32,
    callback: u32,
) -> u32 {
    let queue = m.gw(0x48);
    let count = m.lw(queue + 4);
    m.sw(queue + 4, count.wrapping_add(1));
    let entry = queue.wrapping_add((count & 0xf) * 20 + 8);
    m.sb(entry, 4);
    m.sh(entry + 2, file as u16);
    m.sw(entry + 4, buffer);
    m.sw(entry + 8, size);
    m.sw(entry + 0xc, chunk);
    m.sb(entry + 1, 0);
    m.sw(entry + 0x10, callback);
    4
}

/// 8002C81C: select music `id` (0xFF: none) for the three channels of
/// `[gp+0x304]`: clear the channel state, then copy the entry's track count
/// and flags from the music table 800AE7CC. Returns 0.
pub fn music_select(m: &mut Machine, id: u32, a1: u32, a2: u32) -> u32 {
    for offset in [0x312, 0x311, 0x310, 0x30b, 0x30a] {
        m.sb(GP + offset, 0);
    }
    let mut block = 0;
    for i in 0..3u32 {
        let channels = m.gw(0x304);
        m.sb(channels.wrapping_add(i) + 4, 0);
        let channels = m.gw(0x304);
        m.sb(channels.wrapping_add(i) + 7, 0);
        block = m.gw(0x304);
        m.sh(block.wrapping_add(i << 1) + 0xa, 0);
    }
    if id == 0xff {
        m.sb(GP + 0x308, 0);
        return 0;
    }
    let entry = id.wrapping_mul(20).wrapping_add(MUSIC_TABLE);
    m.sb(GP + 0x308, 1);
    m.sw(block, id);
    m.sw(block + 0x10, a1);
    m.sw(block + 0x14, a2);
    let tracks = u32::from(m.lbu(entry + 2));
    m.sh(block + 0x18, (tracks << 2) as u16);
    let tracks = u32::from(m.lbu(entry + 2));
    m.sh(block + 0x1a, tracks.wrapping_mul(6) as u16);
    let flags = u32::from(m.lhu(entry + 0x10));
    m.set_gw(0x30c, u32::MAX);
    m.sh(block + 0x1c, (((flags & 0x3ff) >> 6) | 0x30) as u16);
    0
}

/// 800302B0: 8002F0E4(2) and clear the sound listener words 800C43E8..F0.
pub fn sound_listener_reset(m: &mut Machine) -> u32 {
    call(m, 0x8002_f0e4, [2, 0, 0, 0]);
    m.sw(SOUND_LISTENER + 8, 0);
    m.sw(SOUND_LISTENER + 4, 0);
    m.sw(SOUND_LISTENER, 0);
    0
}

/// 8003050C: 8002D9B8 and 8002F204 (stop voices and sound channels).
pub fn sound_stop_all(m: &mut Machine) -> u32 {
    call(m, 0x8002_d9b8, [0; 4]);
    call(m, 0x8002_f204, [0; 4]);
    0
}

/// 8002F2C8: sound source `index` word +0x24 = `value`.
pub fn sound_source_set(m: &mut Machine, index: u32, value: u32) -> u32 {
    let source = index.wrapping_mul(0x5c).wrapping_add(SOUND_SOURCES);
    m.sw(source + 0x24, value);
    source
}

/// 8007827C: camera target mode 0x1000 on the player (80077D54), then
/// 80035248(0).
pub fn camera_follow_player(m: &mut Machine) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let entities = m.gw(ENTITIES);
    let camera = m.lw(gameplay + 0x28);
    let player = m.lw(entities);
    call(m, 0x8007_7d54, [camera, 0x1000, player, 0]);
    call(m, 0x8003_5248, [0; 4])
}

/// 8002B87C: chunks left in the stream (`[gp+0x40]` +0x34) while the stream
/// state `[gp+0x38]` +0x20 is 0x40 (playing), else 0.
pub fn stream_pending(m: &mut Machine) -> u32 {
    let state = m.gw(0x38);
    if m.lw(state + 0x20) != 0x40 {
        return 0;
    }
    let stream = m.gw(0x40);
    m.lw(stream + 0x34)
}

/// 8002B5C4: take the next stream chunk. Returns 1 when none is ready
/// (clearing the stream's bit in `[gp+0x3C]` when it is not playing),
/// otherwise stores the chunk's data / header at `*data_out` /
/// `*chunk_out`, advances the ring and returns 0.
pub fn stream_next(m: &mut Machine, data_out: u32, chunk_out: u32) -> u32 {
    let state = m.gw(0x38);
    let mut ready = false;
    if m.lw(state + 0x20) == 0x40 {
        let stream = m.gw(0x40);
        ready = (m.lw(stream + 0x34) as i32) > 0;
    }
    if !ready {
        let state = m.gw(0x38);
        let flags = m.lw(state);
        let channel = u32::from(m.lbu(state + 0x1d));
        if flags & 0x8000 != 0 {
            let mask = m.gw(0x3c);
            let bits = m.lw(mask);
            m.sw(mask, bits & !(1u32.wrapping_shl(channel)));
        }
        return 1;
    }
    let stream = m.gw(0x40);
    let mut chunk = m.lw(stream + 0x3c);
    let mut kind = m.lbu(chunk);
    if kind == 4 {
        m.sb(chunk, 0);
        let stream = m.gw(0x40);
        chunk = m.lw(stream + 0x10);
        m.sw(stream + 0x3c, chunk);
        let header = m.lw(stream + 0x14);
        m.sw(stream + 0x38, 0);
        m.sw(stream + 0x40, header);
        kind = m.lbu(chunk);
    }
    if kind != 2 {
        return 1;
    }
    let state = m.gw(0x38);
    if m.lw(state) & 0x8000 != 0 {
        let channel = u32::from(m.lbu(state + 0x1d));
        let mask = m.gw(0x3c);
        let out = mask.wrapping_add((channel << 4) + 4);
        let bits = m.lw(mask);
        m.sw(mask, bits | 1u32.wrapping_shl(channel));
        for i in 0..4u32 {
            let word = m.lw(chunk.wrapping_add(0x10 + 4 * i));
            m.sw(out.wrapping_add(4 * i), word);
        }
    }
    let stream = m.gw(0x40);
    let current = m.lw(stream + 0x3c);
    m.sw(chunk_out, current);
    let stream = m.gw(0x40);
    let data = m.lw(stream + 0x40);
    m.sw(data_out, data);
    let stream = m.gw(0x40);
    let parts = u32::from(m.lbu(chunk + 2));
    let position = m.lw(stream + 0x38).wrapping_add(parts);
    let limit = m.lw(stream + 0xc);
    let chunk_words = m.lw(stream);
    m.sw(stream + 0x38, position);
    let header_words = m.lw(stream + 4);
    if (position as i32) >= (limit as i32) {
        let first = m.lw(stream + 0x10);
        let header = m.lw(stream + 0x14);
        m.sw(stream + 0x38, 0);
        m.sw(stream + 0x3c, first);
        m.sw(stream + 0x40, header);
    } else if m.lbu(chunk + 2) != 0 {
        let mut i = 0u32;
        loop {
            let at = m.lw(stream + 0x3c);
            let header = m.lw(stream + 0x40);
            m.sw(stream + 0x3c, at.wrapping_add(chunk_words << 2));
            m.sw(stream + 0x40, header.wrapping_add(header_words << 2));
            i += 1;
            if (i as i32) >= i32::from(m.lbu(chunk + 2)) {
                break;
            }
        }
    }
    let stream = m.gw(0x40);
    let left = m.lw(stream + 0x34);
    m.sw(stream + 0x34, left.wrapping_sub(1));
    0
}

/// 8002B7B8: release the `chunk +2` consumed chunks from `chunk` (type 2 ->
/// 0, stepping `[gp+0x40]` word 0 words). A chunk of another type prints the
/// stream state, stops the CD (CdControlB 9) and its callbacks; returns 1.
pub fn stream_release(m: &mut Machine, chunk: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let count = u32::from(m.lbu(chunk + 2));
    let mut at = chunk;
    let mut index = 0u32;
    let mut result = 0;
    while index < count {
        let kind = m.lbu(at);
        if kind != 2 {
            m.sw(frame + 0x10, u32::from(kind));
            let flags = u32::from(m.lbu(at + 1));
            m.sw(frame + 0x14, flags);
            let parts = u32::from(m.lbu(at + 2));
            m.sw(frame + 0x18, parts);
            sdk::printf(m, [0x8001_041c, index, chunk, at]);
            let stream = m.gw(0x40);
            let position = m.lw(stream + 0x2c);
            sdk::printf(m, [0x8001_0450, position, 0, 0]);
            let result_buffer = m.gw(0x34).wrapping_add(0x28);
            crate::mainloop::pending_sdk::cd_control_b(m, 9, 0, result_buffer);
            sdk::stop_callback(m);
            result = 1;
            break;
        }
        m.sb(at, 0);
        let stream = m.gw(0x40);
        let words = m.lw(stream);
        index += 1;
        at = at.wrapping_add(words << 2);
    }
    m.pop_frame(0x28);
    result
}

/// 80038B50: clear the emitter mask (particle block +0x1548).
pub fn emitters_clear(m: &mut Machine) -> u32 {
    let block = m.gw(PARTICLES);
    m.sw(block + 0x1548, 0);
    block
}

/// 80038B60: clear bit 0x10000 of word 0 of all 64 entities.
pub fn entities_clear_hidden(m: &mut Machine) {
    for i in 0..0x40 {
        let object = entity(m, i);
        let flags = m.lw(object);
        m.sw(object, flags & 0xfffe_ffff);
    }
}

/// 80091248: release the used objects of the three effect pools of
/// `[gp+0x40C]` (+0x108C used masks cleared from the +0x600 bitmaps) and
/// clear its +0x33C0 / +0x3964 / +0x3B28 counters.
pub fn effect_pools_release(m: &mut Machine) -> u32 {
    let base = m.gw(0x40c);
    for i in 0..3u32 {
        let at = base.wrapping_add(4 * i);
        let used = m.lw(at + 0x108c);
        let bitmap = m.lw(at + 0x600);
        m.sw(at + 0x108c, 0);
        m.sw(at + 0x600, bitmap & !used);
    }
    let base = m.gw(0x40c);
    m.sw(base + 0x33c0, 0);
    m.sw(base + 0x3964, 0);
    m.sw(base + 0x3b28, 0);
    base
}

/// 800390C0: run the sequence commands from `record` through the handler
/// table 800AF9DC until opcode 0; returns the end of the list.
pub fn sequence_commands(m: &mut Machine, record: u32) -> u32 {
    let mut record = record;
    loop {
        let opcode = m.lw(record) & 0x7f;
        if opcode == 0 {
            return record;
        }
        let handler = m.lw((opcode << 2).wrapping_add(SEQUENCE_COMMANDS));
        record = call(m, handler, [record, 0, 0, 0]);
    }
}

/// Command words +0xC/+0x10/+0x14 shifted to 20.12 fixed point.
fn command_position(m: &mut Machine, record: u32) -> [u32; 3] {
    [0xc, 0x10, 0x14].map(|offset| m.lw(record + offset) << 12)
}

/// 800390B8 / 80039088: commands 0 and 6, one word (no effect).
pub fn command_skip(record: u32) -> u32 {
    record.wrapping_add(4)
}

/// 80038BA0: command 1, camera pose: angles +4..+8 into camera +0x14..+0x18,
/// position +0xC..+0x14 (<< 12) into camera +4..+0xC.
pub fn command_camera_pose(m: &mut Machine, record: u32) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    for (from, to) in [(4, 0x14), (6, 0x16), (8, 0x18)] {
        let half = m.lhu(record + from);
        m.sh(camera + to, half);
    }
    let position = command_position(m, record);
    for (i, word) in position.into_iter().enumerate() {
        m.sw(camera + 4 + 4 * i as u32, word);
    }
    record.wrapping_add(0x18)
}

/// 80038C08: command 7, camera lens and target: a nonzero, changed +2
/// (projection) applies 800760F8; a changed +4 / +6 (near / far) applies
/// 80076140; then the camera targets entity (header >> 10) & 0x3F in mode
/// 1 / 2 (header bits 8..9; 0 targets the player in mode 0x2000).
pub fn command_camera_lens(m: &mut Machine, record: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    let mut projection = m.lh(record + 2) as i32;
    let current = m.lh(camera + 0x24) as i32;
    let mut near = m.lh(camera + 0x28) as i32;
    let mut far = m.lh(camera + 0x2a) as i32;
    let mut change = 0;
    if projection != 0 && current != projection {
        change = 1;
    } else {
        projection = current;
    }
    let value = m.lh(record + 4) as i32;
    if value != 0 && near != value {
        near = value;
        change = 2;
    }
    let value = m.lh(record + 6) as i32;
    if value != 0 && far != value {
        far = value;
        change = 2;
    }
    if change == 1 {
        camera_set_projection(m, camera, projection as u32);
    } else if change == 2 {
        let gameplay = m.gw(GAMEPLAY);
        let depth = m.lw(gameplay + 0x288);
        m.sw(frame + 0x10, depth);
        camera_set_range(m, camera, near as u32, far as u32, projection as u32, depth);
    }
    let header = m.lw(record);
    let entities = m.gw(ENTITIES);
    let kind = (header >> 8) & 3;
    let mut target = m.lw(entities);
    let mut mode = 0x2000;
    if kind != 0 {
        target = m.lw(((header >> 8) & 0xfc).wrapping_add(entities));
        if kind != 3 {
            mode = kind;
        }
    }
    call(m, 0x8007_7d54, [camera, mode, target, 0]);
    m.pop_frame(0x28);
    record.wrapping_add(8)
}

/// Commands 2 / 3: entity (header >> 8) & 0x3F gains 0x14000, angles +4..+8
/// into +0x18..+0x1C, position (<< 12) into +8..+0x10.
fn command_entity(m: &mut Machine, record: u32) -> u32 {
    let header = m.lw(record);
    let entities = m.gw(ENTITIES);
    let object = m.lw(((header >> 6) & 0xfc).wrapping_add(entities));
    let flags = m.lw(object);
    m.sw(object, flags | 0x1_4000);
    for (from, to) in [(4, 0x18), (6, 0x1a), (8, 0x1c)] {
        let half = m.lhu(record + from);
        m.sh(object + to, half);
    }
    let position = command_position(m, record);
    for (i, word) in position.into_iter().enumerate() {
        m.sw(object + 8 + 4 * i as u32, word);
    }
    object
}

/// 80038D30: command 2, entity pose; header bit 14 sets (else clears) bit 3
/// of the entity's byte +5.
pub fn command_entity_pose(m: &mut Machine, record: u32) -> u32 {
    let object = command_entity(m, record);
    let header = m.lw(record);
    let flags = m.lbu(object + 5);
    if (header >> 14) & 1 != 0 {
        m.sb(object + 5, flags | 8);
    } else {
        m.sb(object + 5, flags & 0xf7);
    }
    record.wrapping_add(0x18)
}

/// 80038DEC: command 3, entity pose without the flag.
pub fn command_entity_place(m: &mut Machine, record: u32) -> u32 {
    command_entity(m, record);
    record.wrapping_add(0x18)
}

/// 80038E74: command 4, emitter (header >> 11) & 0x1F: when header bits
/// 8..10 are 0, bit 21 starts it (8006AEE8 with 0x1801 and the position /
/// angles of the record) or else stops it (8006AF70).
pub fn command_emitter(m: &mut Machine, record: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let position = command_position(m, record);
    for (i, word) in position.into_iter().enumerate() {
        m.sw(frame + 0x10 + 4 * i as u32, word);
    }
    for (from, to) in [(4, 0x20), (6, 0x22), (8, 0x24)] {
        let half = m.lhu(record + from);
        m.sh(frame + to, half);
    }
    let header = m.lw(record);
    if (header >> 8) & 7 == 0 {
        let index = (header >> 11) & 0x1f;
        if (header >> 21) & 1 != 0 {
            emitter_set(m, index, 0x1801, frame + 0x10);
        } else {
            emitter_stop(m, index);
        }
    }
    m.pop_frame(0x30);
    record.wrapping_add(0x18)
}

/// 80038F38: command 5, effect: sub-command (header >> 8) & 0x1F through the
/// jump table 8002398C to the 8009xxxx effect routines.
pub fn command_effect(m: &mut Machine, record: u32) -> u32 {
    let header = m.lw(record);
    let case = (header >> 8) & 0x1f;
    let target = m.lw((case << 2).wrapping_add(EFFECT_CASES));
    let half = m.lh(record + 8) as i32 as u32;
    match target {
        0x8003_8f80 => {
            call(m, 0x8009_1cac, [record, case, 0, 0]);
        }
        0x8003_8f90 => {
            call(m, 0x8009_1c14, [record, case.wrapping_sub(8), 0, 0]);
        }
        0x8003_8fa4 => {
            call(m, 0x8009_1d3c, [record, half, 0, 0]);
        }
        0x8003_8fbc => {
            call(m, 0x8009_1d3c, [record, half, 1, 0]);
        }
        0x8003_8fd4 => {
            call(m, 0x8009_1d3c, [record, half, 2, 0]);
        }
        0x8003_8fec => {
            call(m, 0x8009_1310, [record, case, 0, 0]);
        }
        0x8003_8ffc => {
            call(m, 0x8009_1520, [record, case, 0, 0]);
        }
        0x8003_900c => {
            call(m, 0x8009_17d0, [record, case, 0, 0]);
        }
        0x8003_901c => {
            call(m, 0x8009_192c, [record, case, 0, 0]);
        }
        0x8003_902c => {
            call(m, 0x8009_1ab4, [record, 0, 0, 0]);
        }
        0x8003_9040 => {
            call(m, 0x8009_1ab4, [record, 1, 0, 0]);
        }
        0x8003_9054 => {
            call(m, 0x8009_1ab4, [record, 2, 0, 0]);
        }
        0x8003_9068 => {
            call(m, 0x8009_1f68, [record, 4, 0, 0]);
        }
        0x8003_9074 => {}
        _ => m.raise(Fault::Untranslated { address: target }),
    }
    record.wrapping_add(0x18)
}

/// 80039090: command 8, 800920D4(record); eight words.
pub fn command_overlay(m: &mut Machine, record: u32) -> u32 {
    call(m, 0x8009_20d4, [record, 0, 0, 0]);
    record.wrapping_add(0x20)
}

/// 800760F8: camera +0x24 = projection `h`, SetGeomScreen(h), and the depth
/// cue for the camera's near/far (8005D0C4).
pub fn camera_set_projection(m: &mut Machine, camera: u32, h: u32) -> u32 {
    m.sh(camera + 0x24, h as u16);
    sdk::set_geom_screen(m, h);
    let near = m.lh(camera + 0x28) as i32 as u32;
    let far = m.lh(camera + 0x2a) as i32 as u32;
    call(m, 0x8005_d0c4, [near, far, h, 0])
}

/// 80076140: camera near/far clamped to 1000..0x7C17 (0 if far < near):
/// depth buckets 8005D390(near, far, `depth`), camera +0x28/+0x2A, then the
/// projection (`h`, or the camera's own when 0). The fifth argument is the
/// caller's stack word +0x10. Returns 1.
pub fn camera_set_range(
    m: &mut Machine,
    camera: u32,
    near: u32,
    far: u32,
    h: u32,
    depth: u32,
) -> u32 {
    let mut near = near as i32;
    let mut far = far as i32;
    if far < near {
        return 0;
    }
    near = near.clamp(1000, 0x7c17);
    far = far.clamp(1000, 0x7c17);
    call(m, 0x8005_d390, [near as u32, far as u32, depth, 0]);
    m.sh(camera + 0x28, near as u16);
    m.sh(camera + 0x2a, far as u16);
    let h = if h == 0 {
        m.lh(camera + 0x24) as i32 as u32
    } else {
        h
    };
    camera_set_projection(m, camera, h);
    1
}

/// 800302E4: sound listener and sources for one sequence frame: listener
/// pose from the record (800827B4 into 800C43E8), source ids (8002F354),
/// source positions / angles / volumes, 8002F590, then the one-shot sounds
/// from +0x78 until id -1 (8002EFA4).
pub fn sequence_sound(m: &mut Machine, record: u32) -> u32 {
    let frame = m.push_frame(0x70);
    let position = command_position(m, record);
    for (i, word) in position.into_iter().enumerate() {
        m.sw(frame + 0x10 + 4 * i as u32, word);
    }
    for (from, to) in [(4, 0x20), (6, 0x22), (8, 0x24)] {
        let half = m.lhu(record + from);
        m.sh(frame + to, half);
    }
    let sources = record.wrapping_add(0x18);
    call(m, 0x8008_27b4, [frame + 0x10, SOUND_LISTENER, 0, 0]);
    for i in 0..4 {
        let word = m.lw(frame + 0x10 + 4 * i);
        m.sw(SOUND_LISTENER + 4 * i, word);
    }
    let mut i = 0u32;
    if (m.lw(SOUND_SOURCE_COUNT) as i32) > 0 {
        let mut at = sources;
        loop {
            let id = m.lh(at) as i32 as u32;
            at = at.wrapping_add(0x18);
            m.sw(frame + 0x38 + 4 * i, id);
            i += 1;
            if (i as i32) >= (m.lw(SOUND_SOURCE_COUNT) as i32) {
                break;
            }
        }
    }
    call(m, 0x8002_f354, [frame + 0x38, frame + 0x48, 0, 0]);
    if (m.lw(SOUND_SOURCE_COUNT) as i32) > 0 {
        let mut i = 0u32;
        let mut at = sources;
        loop {
            let index = m.lw(frame + 0x48 + 4 * i);
            let source = index.wrapping_mul(0x5c).wrapping_add(SOUND_SOURCES);
            for (j, offset) in [8, 0xc, 0x10].into_iter().enumerate() {
                let word = m.lw(at + offset);
                m.sw(source + 0xc + 4 * j as u32, word << 12);
            }
            let value = m.lw(at + 0x14);
            sound_source_set(m, index, value);
            i += 1;
            for (from, to) in [(2, 0x1c), (4, 0x1e), (6, 0x20)] {
                let half = m.lhu(at + from);
                m.sh(source + to, half);
            }
            at = at.wrapping_add(0x18);
            if (i as i32) >= (m.lw(SOUND_SOURCE_COUNT) as i32) {
                break;
            }
        }
    }
    call(m, 0x8002_f590, [frame + 0x10, 0, 0, 0]);
    let sounds = sources.wrapping_add(0x60);
    while m.lh(sounds) != -1 {
        let position = [8, 0xc, 0x10].map(|offset| m.lw(sounds + offset) << 12);
        for (i, word) in position.into_iter().enumerate() {
            m.sw(frame + 0x28 + 4 * i as u32, word);
        }
        let id = m.lh(sounds) as i32 as u32;
        call(m, 0x8002_efa4, [id, frame + 0x28, 4, 0]);
    }
    m.pop_frame(0x70);
    0
}

/// 80039128: stream callback, the sequence stream ended (gp+0x3F4 = 1).
pub fn sequence_stream_done(m: &mut Machine) {
    m.set_gw(0x3f4, 1);
}

/// 80039138: start the sequence of the current mode: frame count from its
/// object, camera on the player, gameplay bit 0 and context bit 0x10000000,
/// event queue reset, a 0x48C0-byte stream buffer for its stream file
/// (8002C500 with callback 80039128), sound and effect resets, music 1 and
/// transition 0x20.
pub fn sequence_start(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x20);
    m.set_gw(0x3dc, 0x8020_1000);
    let object = sequence_object(m);
    let frames = m.lw(object).wrapping_sub(1);
    m.set_gw(0x3e4, frames);
    let left = m.lhu(GP + 0x3e4);
    m.set_gw(0x3ec, 0);
    m.sh(GP + 0x3f0, left);
    camera_follow_player(m);
    let gameplay = m.gw(GAMEPLAY);
    let flags = m.lw(gameplay);
    m.sw(gameplay, flags | 1);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags | 0x1000_0000);
    call(m, 0x8002_c280, [0; 4]);
    let buffer = crate::arena::alloc(m, 0x48c0);
    let context = m.gw(CONTEXT);
    let mode = m.lw(context + 0xf7c);
    m.set_gw(0x3f4, 0);
    let file = m.lh((mode << 1).wrapping_add(SEQUENCE_STREAMS)) as i32 as u32;
    m.sw(frame + 0x10, 0x8003_9128);
    stream_queue(m, file, buffer, 0x48c0, 0x3000, 0x8003_9128);
    sound_listener_reset(m);
    call(m, 0x8008_33a4, [0; 4]);
    call(m, 0x8007_c3a0, [0; 4]);
    call(m, 0x8006_ad60, [0; 4]);
    music_select(m, 1, 0, 0);
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    m.set_gw(0x3e8, 0);
    let v0 = request_transition_v0(m, 0x20, stage);
    m.set_gw(0x3e0, 0);
    m.pop_frame(0x20);
    v0
}

/// [`request_transition`] and its return value (gameplay word 0).
fn request_transition_v0(m: &mut Machine, first: u32, second: u32) -> u32 {
    request_transition(m, first, second);
    let gameplay = m.gw(GAMEPLAY);
    m.lw(gameplay)
}

/// 8003926C: end the sequence: event queue reset, its final-state commands
/// (object +4), music off, sounds stopped, context bit 0x10000000 cleared and
/// 80038A24(0).
pub fn sequence_end(m: &mut Machine) -> u32 {
    call(m, 0x8002_c280, [0; 4]);
    let object = sequence_object(m);
    m.set_gw(0x3dc, object.wrapping_add(4));
    emitters_clear(m);
    entities_clear_hidden(m);
    let record = m.gw(0x3dc);
    sequence_commands(m, record);
    music_select(m, 0xff, 0, 0);
    sound_stop_all(m);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags & 0xefff_ffff);
    sequence_restore(m, 0)
}

/// 80038A24: restore the camera (0x63 words) and the 64 entity snapshots
/// (8 words each; `flags_only` restores word 0 only and re-places the
/// player with 800710FC) saved at `[gp+0x3D4]`, reset the arena to
/// `[gp+0x3D8]`, clear context bit 0x10000000, reset the effect lists and
/// choose the next event (80037B24).
pub fn sequence_restore(m: &mut Machine, flags_only: u32) -> u32 {
    let frame = m.push_frame(0x48);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    let saved = m.gw(0x3d4);
    copy_words(m, camera, saved, 0x63);
    let near = m.lh(camera + 0x28) as i32 as u32;
    let far = m.lh(camera + 0x2a) as i32 as u32;
    let gameplay = m.gw(GAMEPLAY);
    let h = m.lh(camera + 0x24) as i32 as u32;
    let depth = m.lw(gameplay + 0x288);
    m.sw(frame + 0x10, depth);
    camera_set_range(m, camera, near, far, h, depth);
    let mut offset = 0x18c;
    for i in 0..0x40 {
        let saved = m.gw(0x3d4).wrapping_add(offset);
        if flags_only != 0 {
            copy_words(m, frame + 0x18, saved, 8);
            let object = entity(m, i);
            let word = m.lw(frame + 0x18);
            m.sw(object, word);
            if i == 0 {
                call(m, 0x8007_10fc, [object.wrapping_add(8), 0, 0, 0]);
            }
        } else {
            let object = entity(m, i);
            copy_words(m, object, saved, 8);
        }
        offset += 0x20;
    }
    let pointer = m.gw(0x3d8);
    crate::arena::reset(m, pointer);
    call(m, 0x8008_33a4, [0; 4]);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags & 0xefff_ffff);
    call(m, 0x8006_ad60, [0; 4]);
    let v0 = call(m, 0x8003_7b24, [0; 4]);
    m.pop_frame(0x48);
    v0
}

/// 80039598: sequence fade and letterbox. gp+0x3E0 is the fade state (1/3
/// fading in, 2 shown, 4 fading out, 5 done), gp+0x18C the fade level and
/// gp+0x190 the letterbox level. Pad bit 0x40 skips (state 1 -> 3, 2 -> 4);
/// fading out ends with transition 0x40. Then the fade (800923E8) and the
/// letterbox (80092164) packets, and for modes 0x17/0x18/0x19/0x1B the
/// title overlay 8008E42C over its frame window (the original's second,
/// 80082E2C overlay needs bit 0 of a mask that is never set).
pub fn sequence_fade(m: &mut Machine) {
    let left = u32::from(m.lhu(GP + 0x3f0));
    let frames = m.gw(0x3e4);
    let context = m.gw(CONTEXT);
    let elapsed = frames.wrapping_sub(left).wrapping_add(2) as i32;
    if m.lw(context + 0x12c) & 0x40 != 0 {
        let state = m.gw(0x3e0);
        m.set_gw(0x3ec, 1);
        if state == 1 {
            m.set_gw(0x3e0, 3);
        } else if state == 2 {
            fade_out(m);
        }
    }
    let state = m.gw(0x3e0);
    if state.wrapping_sub(1) < 3 {
        if (m.gw(0x3e4) as i32) <= 0 {
            m.set_gw(0x18c, 0);
            m.set_gw(0x190, 0);
        } else {
            let step = elapsed.wrapping_sub(1) as u32;
            m.set_gw(0x18c, step << 7);
            m.set_gw(0x190, step << 8);
        }
        if (m.gw(0x18c) as i32) < 0 {
            m.set_gw(0x18c, 0);
        }
        if (m.gw(0x190) as i32) < 0 {
            m.set_gw(0x190, 0);
        }
        if (m.gw(0x18c) as i32) >= 0x1800 {
            let state = m.gw(0x3e0);
            if state == 1 {
                m.set_gw(0x3e0, 2);
            } else if state == 3 {
                fade_out(m);
            }
        }
        if m.lhu(GP + 0x3f0) < 0x31 {
            fade_out(m);
        }
    } else if state == 4 {
        let level = m.gw(0x18c).wrapping_sub(0x80);
        m.set_gw(0x18c, level);
        if (level as i32) <= 0 {
            let gameplay = m.gw(GAMEPLAY);
            m.set_gw(0x18c, 0);
            let stage = m.lw(gameplay + 8);
            m.set_gw(0x3e0, 5);
            request_transition(m, 0x40, stage);
        }
        if m.gw(0x3ec) == 0 && m.lhu(GP + 0x3f0) < 0x19 {
            let level = m.gw(0x190).wrapping_sub(0x100);
            m.set_gw(0x190, level);
        }
        if (m.gw(0x190) as i32) <= 0 {
            m.set_gw(0x190, 0);
        }
    } else if state == 0 || state == 5 {
        m.set_gw(0x18c, 0);
        m.set_gw(0x190, 0);
        m.set_gw(0x194, 0);
    }
    let context = m.gw(CONTEXT);
    let gameplay = m.gw(GAMEPLAY);
    let level = m.gw(0x18c);
    let ot = m.lw(context + 0xc);
    let packets = m.lw(gameplay + 0x14);
    let packets = screen_fade(m, ot, packets, 0, level);
    let gameplay = m.gw(GAMEPLAY);
    let letterbox_level = m.gw(0x190).wrapping_sub(0x800);
    let ot = m.lw(gameplay + 0x10);
    m.sw(gameplay + 0x14, packets);
    let packets = letterbox(m, ot, packets, letterbox_level);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, packets);
    // Overlay windows: (mode, first frame, divisor base); the overlay
    // position is gp+0x194 << 11 over the frames after the base.
    let mut overlay = false;
    let mut position = 0i32;
    let windows = [
        (0x17, 0x12d, 0x12c),
        (0x18, 0x97, 0x96),
        (0x19, 0x152, 0x151),
    ];
    for (wanted, first, base) in windows {
        let context = m.gw(CONTEXT);
        if m.lw(context + 0xf7c) != wanted || elapsed < first {
            continue;
        }
        let frames = m.gw(0x3e4) as i32;
        if frames < elapsed {
            continue;
        }
        overlay = true;
        let shown = (m.gw(0x194) << 11) as i32;
        position = shown.wrapping_div(frames.wrapping_sub(base));
    }
    let context = m.gw(CONTEXT);
    if m.lw(context + 0xf7c) == 0x1b && (0xaa..0xf4).contains(&elapsed) {
        overlay = true;
        position = ((m.gw(0x194) << 11) as i32) / 74;
    }
    if overlay {
        let gameplay = m.gw(GAMEPLAY);
        let ot = m.lw(gameplay + 0x10);
        let packets = m.lw(gameplay + 0x14);
        let next = call(m, 0x8008_e42c, [ot, packets, position as u32, 0]);
        let shown = m.gw(0x194).wrapping_add(1);
        let gameplay = m.gw(GAMEPLAY);
        m.sw(gameplay + 0x14, next);
        m.set_gw(0x194, shown);
    }
}

/// Fade state 4 (fading out) from full level.
fn fade_out(m: &mut Machine) {
    m.set_gw(0x3e0, 4);
    m.set_gw(0x18c, 0x1680);
    m.set_gw(0x190, 0x1800);
}

/// 80092164: letterbox: with `level` > 0 (0x1000 = full, else the table
/// 800B4F84), narrow the drawing area of the 16 ordering-table slots (the
/// last one, and the 15 of `[gameplay+0x288]` unless gameplay bit 0x200 is
/// set) to the middle band and add the two black bars at slot 1. Returns
/// the next free packet.
pub fn letterbox(m: &mut Machine, ot: u32, packets: u32, level: u32) -> u32 {
    if (level as i32) <= 0 {
        return packets;
    }
    let level = if (level as i32) < 0x1000 {
        let index = ((level as i32) >> 2) as u32;
        m.lh((index << 2).wrapping_add(0x800b_4f84)) as i32
    } else {
        0x1000
    };
    let mut packet = packets;
    let context = m.gw(CONTEXT);
    let buffer = m.lw(context + 4);
    let bar = ((level << 5) >> 12) as u32;
    m.sw(packet, 0x0200_0000);
    let top = u32::from(buffer != 0) << 8;
    let start_y = top.wrapping_add(bar);
    let start = ((start_y & 0x3ff) << 10) | 0xe300_0000;
    let end_y = start_y
        .wrapping_add(0x100u32.wrapping_sub(bar).wrapping_sub(bar))
        .wrapping_sub(1);
    let end = ((end_y & 0x3ff) << 10) | 0xe400_013f;
    m.sw(packet + 4, start);
    m.sw(packet + 8, end);
    let last = m.lw(ot + 0x3ffc);
    m.sw(packet, last | 0x0200_0000);
    m.sw(ot + 0x3ffc, (packet << 8) >> 8);
    let gameplay = m.gw(GAMEPLAY);
    if !m.lw(gameplay) & 0x200 != 0 {
        let mut slots = m.lw(gameplay + 0x288).wrapping_add(2);
        for _ in 0..0xf {
            packet = packet.wrapping_add(0xc);
            let slot = m.lh(slots) as i32 as u32;
            m.sw(packet, 0x0200_0000);
            m.sw(packet + 4, start);
            m.sw(packet + 8, end);
            let entry = (slot << 2).wrapping_add(ot);
            let next = m.lw(entry);
            m.sw(packet, next | 0x0200_0000);
            m.sw(entry, (packet << 8) >> 8);
            slots = slots.wrapping_add(2);
        }
    }
    packet = packet.wrapping_add(0xc);
    let bars = packet.wrapping_add(0xc);
    m.sw(packet, ((bars << 8) >> 8) | 0x0200_0000);
    m.sw(packet + 4, (top << 10) | 0xe300_0000);
    m.sw(packet + 8, ((top + 0xff) << 10) | 0xe400_013f);
    m.sw(packet + 0xc, 0x0600_0000);
    m.sw(bars + 4, 0x6000_0000);
    m.sw(bars + 0x10, 0x6000_0000);
    m.sh(bars + 0x16, 0x100u32.wrapping_sub(bar) as u16);
    m.sw(bars + 0xc, (bar << 16) | 0x140);
    m.sh(bars + 0x14, 0);
    m.sh(bars + 8, 0);
    m.sh(bars + 0xa, 0);
    m.sw(bars + 0x18, (bar << 16) | 0x140);
    // lwl/swl pair: the bars join slot 1 (ot + 4) in front of its chain.
    let entry = ot.wrapping_add(4);
    let next = m.lw(entry);
    let tag = m.lw(bars);
    m.sw(bars, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    m.sw(entry, (next & 0xff00_0000) | (packet & 0x00ff_ffff));
    packet.wrapping_add(0x28)
}

/// 800923E8: fade (`level` < 0x1000): a full-screen rectangle of strength
/// 0x1000 - `level` (negated when `additive` is 0) drawn by 80092350, then a
/// mask-bit packet (E6000001); returns the next free packet.
pub fn screen_fade(m: &mut Machine, ot: u32, packets: u32, additive: u32, level: u32) -> u32 {
    if (level as i32) >= 0x1000 {
        return packets;
    }
    let mut amount = 0x1000u32.wrapping_sub(level);
    m.sw(0x1f80_0000, 0);
    m.sw(0x1f80_0004, 0x0100_0140);
    if additive == 0 {
        amount = amount.wrapping_neg();
    }
    let effects = m.gw(0x40c);
    m.sw(effects + 0x5c8, 1);
    let packet = call(m, 0x8009_2350, [ot, packets, 0x1f80_0000, amount]);
    m.sw(packet, 0x0100_0000);
    m.sw(packet + 4, 0xe600_0001);
    let head = m.lw(ot);
    m.sw(packet, 0x0100_0000 | (head & 0x00ff_ffff));
    let head = m.lw(ot);
    m.sw(ot, (head & 0xff00_0000) | (packet & 0x00ff_ffff));
    packet.wrapping_add(8)
}

/// 800924B4: red tint (far colour 0x30/0/0, RGB 0x6203135F, IR0 =
/// 0x1000 - `level`, DPCS) as a full-screen semi-transparent rectangle at
/// ordering-table entry `ot`; returns the next free packet (nothing when
/// `level` is 0).
pub fn tint_rectangle(m: &mut Machine, ot: u32, packets: u32, level: u32) -> u32 {
    if level == 0 {
        return packets;
    }
    m.gte.ctc2(ctrl::RFC, 0x30);
    m.gte.ctc2(ctrl::GFC, 0);
    m.gte.ctc2(ctrl::BFC, 0);
    m.gte.mtc2(reg::RGBC, 0x6203_135f);
    m.gte.mtc2(reg::IR0, 0x1000u32.wrapping_sub(level));
    let packet = packets;
    m.gte.dpcs(true, false);
    let head = m.lw(ot);
    m.sw(packet, head | 0x0300_0000);
    m.sw(ot, (packet << 8) >> 8);
    m.sw(packet + 8, 0xff80_ff60);
    m.sw(packet + 0xc, 0x0100_0140);
    let rgb = m.gte.mfc2(reg::RGB2);
    m.sw(packet + 4, rgb);
    packet.wrapping_add(0x10)
}

/// `(rand() - 0x4000) << 9`: a random offset of +-0x800000.
fn random_spread(m: &mut Machine) -> u32 {
    let value = sdk::rand(m);
    value.wrapping_sub(0x4000) << 9
}

/// 80092540: sparks at (`x`, `y`, `z`): on odd `phase` a spark of the pool
/// at effects +0x600 (3 masks, 7 words), then a random-chance ember and a
/// smoke puff of the pool at +0x1098 (10 masks, 7 words), all with random
/// spread, life and size.
pub fn sequence_sparks(m: &mut Machine, x: u32, y: u32, z: u32, phase: u32) {
    if phase & 1 != 0 {
        let pool = m.gw(0x40c).wrapping_add(0x600);
        let spark = pool_alloc(m, pool, 3, 7);
        if spark != 0 {
            let large = sdk::rand(m) & 1 != 0;
            m.sh(spark, 0xffff);
            let value = sdk::rand(m);
            let life = if large {
                (value & 7) + 9
            } else {
                (value & 7) + 0x14
            };
            m.sh(spark + 2, life as u16);
            let spread = random_spread(m);
            m.sw(spark + 4, x.wrapping_add(spread));
            m.sw(spark + 8, y);
            let spread = random_spread(m);
            m.sw(spark + 0xc, z.wrapping_add(spread));
            m.sh(spark + 0x16, 0);
            m.sh(spark + 0x18, if large { 0x100 } else { 0x210 });
            let value = sdk::rand(m);
            let size = if large {
                (value & 0x1ff) + 0x3e8
            } else {
                (value & 0xff) + 0x4b0
            };
            m.sh(spark + 0x1a, size as u16);
        }
    }
    if sdk::rand(m) & 1 != 0 {
        let pool = m.gw(0x40c).wrapping_add(0x1098);
        let ember = pool_alloc(m, pool, 10, 7);
        if ember != 0 {
            m.sh(ember, 0);
            let value = sdk::rand(m);
            m.sh(ember + 2, ((value & 0xf) + 0x3c) as u16);
            let spread = random_spread(m);
            m.sw(ember + 4, x.wrapping_add(spread));
            m.sw(ember + 8, y.wrapping_add(0xffce_0000));
            let spread = random_spread(m);
            m.sw(ember + 0xc, z.wrapping_add(spread));
            m.sh(ember + 0x16, 0x98);
            let value = sdk::rand(m);
            m.sh(ember + 0x10, (value & 0xff).wrapping_sub(0x80) as u16);
            let value = sdk::rand(m);
            m.sh(ember + 0x14, (value & 0xff).wrapping_sub(0x80) as u16);
            m.sh(ember + 0x12, 0);
            m.sh(ember + 0x18, 0);
            let value = sdk::rand(m);
            m.sh(ember + 0x1a, ((value & 0xff) + 0xf0) as u16);
        }
    }
    let pool = m.gw(0x40c).wrapping_add(0x1098);
    let smoke = pool_alloc(m, pool, 10, 7);
    if smoke == 0 {
        return;
    }
    m.sh(smoke, 0x1e);
    let value = sdk::rand(m);
    m.sh(smoke + 2, ((value & 7) + 0x50) as u16);
    let spread = sra(random_spread(m), 1);
    m.sw(smoke + 4, x.wrapping_add(spread));
    m.sw(smoke + 8, y.wrapping_add(0xfff3_8000));
    let spread = sra(random_spread(m), 1);
    m.sw(smoke + 0xc, z.wrapping_add(spread));
    m.sh(smoke + 0x16, 0x98);
    let value = sdk::rand(m);
    m.sh(smoke + 0x10, (value & 0x7f).wrapping_sub(0x40) as u16);
    let value = sdk::rand(m);
    m.sh(smoke + 0x14, (value & 0x7f).wrapping_sub(0x40) as u16);
    m.sh(smoke + 0x12, 0);
    m.sh(smoke + 0x18, 0x1e04);
    let value = sdk::rand(m);
    m.sh(smoke + 0x1a, ((value & 0x3ff) + 0x7d0) as u16);
}

/// 800927E4: the burning-sky effect of sequence mode 0x16 at sequence frame
/// `frame` (0xB..0x2DF): a pulsing red tint (rsin, 800924B4), then until
/// frame 0x186 a random camera shake (gameplay +0x290/+0x292) and on odd
/// frames sparks (80092540) along four fixed paths.
pub fn sequence_burn(m: &mut Machine, frame: u32) {
    let frame = frame as i32;
    if !(0xb..0x2e0).contains(&frame) {
        return;
    }
    let sine = call(m, 0x800a_45c8, [(frame << 6) as u32, 0, 0, 0]) as i32;
    let mut level = sine.wrapping_abs();
    if frame >= 0x186 {
        level = (level >> 1) + 0x800;
    }
    let gameplay = m.gw(GAMEPLAY);
    let ot = m.lw(gameplay + 0x10).wrapping_add(8);
    let packets = m.lw(gameplay + 0x14);
    let next = tint_rectangle(m, ot, packets, 0x1000u32.wrapping_sub(level as u32));
    let gameplay = m.gw(GAMEPLAY);
    m.sw(gameplay + 0x14, next);
    let effects = m.gw(0x40c);
    m.sw(effects + 0x5c8, 1);
    if frame >= 0x187 {
        return;
    }
    let value = sdk::rand(m);
    if value & 1 != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let shake = m.lhu(gameplay + 0x290);
        m.sh(
            gameplay + 0x290,
            shake.wrapping_add(((value >> 1) & 1) as u16),
        );
        let shake = m.lhu(gameplay + 0x292);
        m.sh(
            gameplay + 0x292,
            shake.wrapping_add(((value >> 3) & 7) as u16),
        );
    }
    if frame & 1 == 0 {
        return;
    }
    let (x, y, z) = if frame < 0x6f {
        (0xfdae_4000, 0xff6a_0000, 0xfec7_8000)
    } else if frame < 0xc9 {
        (0xfe4a_8000, 0xff63_c000, 0xff25_4000)
    } else if frame < 0x119 {
        let step = ((frame - 0xdc) << 12) / 50;
        let along = step * 125;
        let x = ((-(along << 3)) / 4096 - 0x3e80) << 12;
        let y = ((along << 2) / 4096 - 0x9c4) << 12;
        let z = ((step * 375) << 5) / 4096 + 0xd6d8;
        (x as u32, y as u32, (z << 12) as u32)
    } else {
        let step = ((frame - 0x140) << 12) / 40;
        let along = step * 125;
        let x = ((along << 3) / 4096 - 0x4650) << 12;
        let y = ((step * 25) << 5) / 4096 - 0x9c4;
        let z = (((along << 3) - along) << 3) / 4096 + 0xf618;
        (x as u32, (y << 12) as u32, (z << 12) as u32)
    };
    sequence_sparks(m, x, y, z, (frame >> 1) as u32);
}

// ---- session end helpers --------------------------------------------------------

/// 800C4D98: per-entity session flags (one word per entity).
const ENTITY_SESSION_FLAGS: u32 = 0x800c_4d98;
/// 800C4E98: per-entity session bytes.
const ENTITY_SESSION_BYTES: u32 = 0x800c_4e98;

/// 80044E24: at the end of a session before a non-mission event: 80038010
/// for entities 2..0x1F whose session flags have bit 1, then for entities
/// 2..0x3F with bit 2: clear their session byte, the bit, and entity word 0
/// bit 0x4000.
pub fn session_release_entities(m: &mut Machine) {
    for index in 2..0x20u32 {
        let flags = m.lw(ENTITY_SESSION_FLAGS + 4 * index);
        if flags & 2 != 0 {
            call(m, 0x8003_8010, [index, 0, 0, 0]);
        }
    }
    for index in 2..0x40u32 {
        let at = ENTITY_SESSION_FLAGS + 4 * index;
        let object = entity(m, index);
        if m.lw(at) & 4 == 0 {
            continue;
        }
        m.sb(ENTITY_SESSION_BYTES + index, 0);
        let flags = m.lw(at);
        m.sw(at, flags & 0xffff_fffb);
        let word = m.lw(object);
        m.sw(object, word & 0xffff_bfff);
    }
}

// ---- phase-0x400 courses (update 8005A874) --------------------------------------
//
// gp+0x324 is the course index (800B0100 + 4 x index: kind byte, two target
// slots at +2/+3), gp+0x4EC the course flags set by 80059C4C (0x300 / 0x400
// / 0x800 / 0x1000 / 0x2000 / 0x4000 / 0x8000 select the lesson).

/// 800B0100: course table (4 bytes per course).
const COURSES: u32 = 0x800b_0100;
/// 80024220: jump table of 80075894, by camera view gp+0x28C.
const VIEW_CASES: u32 = 0x8002_4220;
/// 800C35F8: sound channel table (20 bytes per channel, 0x18 channels).
const SOUND_CHANNELS: u32 = 0x800c_35f8;

/// 800302A8: no-op music hook; returns 0.
pub fn music_none() -> u32 {
    0
}

/// 8002DB18: channel `index` (< 0x18) volume byte +0x10 = `volume` * 128 /
/// 100 and halfword +0x12 = `time`; returns the byte, or 0xFF for a bad
/// channel.
pub fn sound_channel_volume(m: &mut Machine, index: u32, volume: u32, time: u32) -> u32 {
    if index >= 0x18 {
        return 0xff;
    }
    let channel = index.wrapping_mul(20).wrapping_add(SOUND_CHANNELS);
    m.sh(channel + 0x12, time as u16);
    let value = ((volume << 7) as i32 / 100) as u32;
    m.sb(channel + 0x10, value as u8);
    value
}

/// 8004CA68: reset the (up to two) target objects of the current course:
/// object `slot` of `[gp+0x33C]` (0xB8 bytes) gains 0x200000, its +0x1C
/// takes the model byte of `[gp+0x374]` slot +0x30 and +0x1D/+0x1E/+0x38/
/// +0x3C/+0x3D are reset.
pub fn course_targets_reset(m: &mut Machine) {
    for i in 0..2u32 {
        let course = (m.gw(0x324) << 2).wrapping_add(COURSES);
        let slot = u32::from(m.lbu(course.wrapping_add(i) + 2));
        if slot == 0 {
            continue;
        }
        let models = m.gw(0x374);
        let model = m.lw((slot << 6).wrapping_add(models) + 0x30);
        let object = m.gw(0x33c).wrapping_add(slot.wrapping_mul(0xb8));
        let model = m.lbu(model);
        let flags = m.lw(object);
        m.sb(object + 0x1d, 0);
        m.sb(object + 0x1e, 0);
        m.sw(object + 0x38, 0);
        m.sb(object + 0x3d, 0xff);
        m.sb(object + 0x3c, 0xff);
        m.sw(object, flags | 0x20_0000);
        m.sb(object + 0x1c, model);
    }
}

/// 80075894: apply camera view gp+0x28C (0..6) through the jump table
/// 80024220 (800754A8 / 800755E4 / 80074CF0 / 8007577C, second argument 0).
pub fn camera_view_apply(m: &mut Machine) {
    let view = m.gw(0x28c);
    if view >= 7 {
        return;
    }
    let target = m.lw((view << 2).wrapping_add(VIEW_CASES));
    match target {
        0x8007_58c8 => call(m, 0x8007_54a8, [0, 0, 0, 0]),
        0x8007_58d0 => call(m, 0x8007_54a8, [1, 0, 0, 0]),
        0x8007_58e4 => call(m, 0x8007_55e4, [0, 0, 0, 0]),
        0x8007_58ec => call(m, 0x8007_55e4, [1, 0, 0, 0]),
        0x8007_5900 => call(m, 0x8007_4cf0, [0, 0, 0, 0]),
        0x8007_5910 => call(m, 0x8007_577c, [0, 0, 0, 0]),
        0x8007_5918 => call(m, 0x8007_577c, [1, 0, 0, 0]),
        _ => {
            m.raise(Fault::Untranslated { address: target });
            0
        }
    };
}

/// 800723AC: clear the player flight state +0x10 and `[gp+0x52C]` +0x24,
/// then re-enter the camera mode `[gp+0x528]` +4 (8 / 0x100 / 0x4000 /
/// 0x8000 / 0x4000000) with its first-frame flag.
pub fn camera_mode_restart(m: &mut Machine) {
    let state = m.gw(0x528);
    let other = m.gw(0x52c);
    m.sw(state + 0x10, 0);
    m.sw(other + 0x24, 0);
    let mode = m.lw(state + 4);
    match mode {
        0x4000 => {
            call(m, 0x8007_577c, [0, 1, 0, 0]);
        }
        8 => {
            let near = u32::from(m.gw(0x28c) != 0);
            call(m, 0x8007_54a8, [near, 1, 0, 0]);
        }
        0x100 => {
            let near = u32::from(m.gw(0x28c) != 2);
            call(m, 0x8007_55e4, [near, 1, 0, 0]);
        }
        0x8000 => {
            call(m, 0x8007_577c, [1, 1, 0, 0]);
        }
        0x0400_0000 => {
            call(m, 0x8007_4cf0, [1, 0, 0, 0]);
        }
        _ => {}
    }
}

/// Course end request: radio off, progress +0x25 = +0x28 and gameplay +8 =
/// +0x28, transition 0x2000 (1), context bit 0x10000000.
fn course_skip(m: &mut Machine) {
    call(m, 0x8005_5044, [0; 4]);
    let progress = m.gw(0x330);
    let next = m.lbu(progress + 0x28);
    m.sb(progress + 0x25, next);
    let progress = m.gw(0x330);
    let gameplay = m.gw(GAMEPLAY);
    let next = u32::from(m.lbu(progress + 0x28));
    m.sw(gameplay + 8, next);
    request_transition(m, 0x2000, 1);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags | 0x1000_0000);
}

/// Lesson cleared: progress +0x31 = 2, +0x30 = 1, +0x32 = 0x1E, +0x1A = 0.
fn course_cleared(m: &mut Machine) {
    let progress = m.gw(0x330);
    m.sb(progress + 0x31, 2);
    let progress = m.gw(0x330);
    m.sb(progress + 0x30, 1);
    let progress = m.gw(0x330);
    m.sh(progress + 0x32, 0x1e);
    m.sh(progress + 0x1a, 0);
}

/// Leave the course once no radio message plays and the lesson is not
/// marked: reset its targets, music (800302A8 / 80030248 by the next event
/// type) and choose the next event.
fn course_leave(m: &mut Machine) {
    if call(m, 0x8005_5038, [0; 4]) != 0 {
        return;
    }
    let progress = m.gw(0x330);
    if m.lbu(progress + 0x30) != 0 {
        return;
    }
    course_targets_reset(m);
    leave_to_next_event(m);
}

fn leave_to_next_event(m: &mut Machine) {
    let progress = m.gw(0x330);
    if m.lbu(progress + 0x29) == 0 {
        music_none();
    } else {
        call(m, 0x8003_0248, [0; 4]);
    }
    call(m, 0x8003_7b24, [0; 4]);
}

/// 8005A268: phase 0x400 (course intro): a fade-in for lessons 0x6000 with
/// progress +0x3F = 1; after 0x3D frames and no radio message, the course
/// camera (80074C60 for 0x8000, else 80075894), the lesson's first radio
/// message (8004B760) and transition 0x800. Any of pad 0x7000 skips the
/// course (transition 0x2000).
pub fn course_start(m: &mut Machine) {
    let progress = m.gw(0x330);
    let lesson_flag = m.lbu(progress + 0x3f);
    let flags = m.gw(0x4ec);
    if lesson_flag == 1 && flags & 0x6000 != 0 {
        let gameplay = m.gw(GAMEPLAY);
        let frame = m.lw(gameplay + 0xc);
        if (frame as i32) < 0x10 {
            let level = 0x1000u32.wrapping_sub(frame << 8);
            let ot = m.lw(gameplay + 0x10).wrapping_add(8);
            let packets = m.lw(gameplay + 0x14);
            let next = call(m, 0x8008_2f18, [ot, packets, level, 0]);
            let gameplay = m.gw(GAMEPLAY);
            m.sw(gameplay + 0x14, next);
        }
    }
    let gameplay = m.gw(GAMEPLAY);
    if (m.lw(gameplay + 0xc) as i32) >= 0x3d && call(m, 0x8005_5038, [0; 4]) == 0 {
        if flags & 0x8000 != 0 {
            call(m, 0x8007_4c60, [0; 4]);
        } else {
            camera_view_apply(m);
        }
        // Flags outside the course set leave the original's message register
        // to its caller; the native port uses message 0 then.
        let message = [
            (0x300, 0xb),
            (0x400, 0x13),
            (0x800, 0x1d),
            (0x2000, 0xf),
            (0x4000, 0x1b),
            (0x1000, 0x27),
            (0x8000, 0x2d),
        ]
        .into_iter()
        .find(|&(bits, _)| flags & bits != 0)
        .map_or(0, |(_, message)| message);
        call(m, 0x8004_b760, [message, 0, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x800, stage);
    }
    let pressed = call(m, 0x8006_b360, [0; 4]);
    if pressed & 0x7000 != 0 {
        call(m, 0x8005_5044, [0; 4]);
        let progress = m.gw(0x330);
        let next = m.lbu(progress + 0x28);
        m.sb(progress + 0x25, next);
        request_transition(m, 0x2000, 1);
        let context = m.gw(CONTEXT);
        let flags = m.lw(context);
        m.sw(context, flags | 0x1000_0000);
    }
}

/// 8005A410: phase 0x2000 (course end) by result gameplay +8: 2 (cleared)
/// marks the lesson at its end frame and leaves after the lesson's window
/// (lesson 0x8000 always waits for pad 0x200); 0x20 (failed) leaves from
/// frame 0x78; another nonzero result restarts the course (transition
/// 0x1000) from frame 0x78; 0 leaves from frame 0x96. Pad 0x7000 skips.
/// The lesson's result message (8004B760) shows on frame 0.
pub fn course_end(m: &mut Machine) {
    let mut pressed = call(m, 0x8006_b360, [0; 4]);
    let gameplay = m.gw(GAMEPLAY);
    let result = m.lw(gameplay + 8);
    let mut message = 0xff;
    if result & 2 != 0 {
        if pressed & 0x7000 != 0 {
            course_skip(m);
            return;
        }
        let flags = m.gw(0x4ec);
        // (message, window end, lesson end frame); flags outside the course
        // set leave the original's s0/s1 to its caller: no window here.
        let lesson: Option<(u32, u32, u32)> = if flags & 0x300 != 0 {
            if m.lw(gameplay + 0xc) == 0 {
                sound_channel_volume(m, 0, 0, 0x96);
                sound_channel_volume(m, 1, 0x1e, 0x96);
                sound_channel_volume(m, 2, 0x14, 0x96);
            }
            Some((0xc, 0x10e, 0x10e))
        } else if flags & 0x400 != 0 {
            Some((0x14, 0x168, 0x168))
        } else if flags & 0x800 != 0 {
            Some((0x2b, 0x168, 0x168))
        } else if flags & 0x6000 != 0 {
            Some((0x31, 0x78, u32::MAX))
        } else if flags & 0x1000 != 0 {
            Some((0x2f, 0xb4, 0xb4))
        } else if flags & 0x8000 != 0 {
            pressed = 0x200;
            Some((0xff, 0x5a, 0x5a))
        } else {
            None
        };
        if let Some((text, window, end)) = lesson {
            message = text;
            let gameplay = m.gw(GAMEPLAY);
            if m.lw(gameplay + 0xc) == end {
                course_cleared(m);
            }
            let gameplay = m.gw(GAMEPLAY);
            if pressed & 0x200 != 0 && (window as i32) < (m.lw(gameplay + 0xc) as i32) {
                course_leave(m);
            }
        }
    } else if result & 0x20 != 0 {
        if pressed & 0x7000 != 0 {
            course_skip(m);
        }
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 0xc) == 0x78 {
            course_cleared(m);
        }
        let gameplay = m.gw(GAMEPLAY);
        if (m.lw(gameplay + 0xc) as i32) >= 0x78 {
            course_leave(m);
        }
    } else if result != 0 {
        let flags = m.gw(0x4ec);
        if flags & 0x300 != 0 {
            message = 0x29;
        } else if flags & 0x400 != 0 {
            message = 0x2a;
        } else if flags & 0x800 != 0 {
            message = 0x2c;
        } else if flags & 0x1000 != 0 {
            message = 0x30;
        }
        let gameplay = m.gw(GAMEPLAY);
        if m.lw(gameplay + 0xc) == 0 {
            call(m, 0x8005_5044, [0; 4]);
        }
        let gameplay = m.gw(GAMEPLAY);
        if (m.lw(gameplay + 0xc) as i32) >= 0x78 {
            course_targets_reset(m);
            call(m, 0x8003_0248, [0; 4]);
            call(m, 0x8005_5044, [0; 4]);
            let progress = m.gw(0x330);
            let next = m.lbu(progress + 0x28);
            m.sb(progress + 0x25, next);
            let progress = m.gw(0x330);
            let next = u32::from(m.lbu(progress + 0x28));
            request_transition(m, 0x1000, next);
            call(m, 0x8007_82b4, [0; 4]);
        }
    } else if (m.lw(gameplay + 0xc) as i32) >= 0x96 && call(m, 0x8005_5038, [0; 4]) == 0 {
        course_targets_reset(m);
        leave_to_next_event(m);
    }
    let gameplay = m.gw(GAMEPLAY);
    if m.lw(gameplay + 0xc) == 0 && message != 0xff {
        call(m, 0x8004_b760, [message, 0, 0, 0]);
    }
}

// ---- movie (phase 0x4000) and picture (phase 0x20000) events --------------------
//
// Context +0x3AC selects the entry of `[gp+0x20C]` (8 bytes: +0 file, +2
// music flag); `[gp+0x204]` / `[gp+0x218]` +4 is the music-on flag of the
// movie / picture screens. ClearImage, LoadImage and StoreImage are libgpu
// calls (800A67E8 / 800A6910 / 800A6970) made through `dispatch`.

const CLEAR_IMAGE: u32 = 0x800a_67e8;
const LOAD_IMAGE: u32 = 0x800a_6910;
const STORE_IMAGE: u32 = 0x800a_6970;

/// Music-on flag of an event screen: the context's music option (+0x3A0 &
/// +0x3AB) and the entry's +2 halfword being 1.
fn screen_music_flag(m: &mut Machine, table: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let enabled = m.lbu(context + 0x3a0) & m.lbu(context + 0x3ab);
    if enabled == 0 {
        return 0;
    }
    let entry = u32::from(m.lbu(context + 0x3ac));
    u32::from(m.lh((entry << 3).wrapping_add(table) + 2) == 1)
}

/// Clear the two 320x256 halves of VRAM (gp+0x4CC rectangle, colour 1/1/1).
fn clear_both_buffers(m: &mut Machine) {
    let rect = GP + 0x4cc;
    m.sh(GP + 0x4cc, 0);
    m.sh(GP + 0x4ce, 0);
    m.sh(GP + 0x4d0, 0x140);
    m.sh(GP + 0x4d2, 0x100);
    call(m, CLEAR_IMAGE, [rect, 1, 1, 1]);
    m.sh(GP + 0x4cc, 0);
    m.sh(GP + 0x4ce, 0x100);
    call(m, CLEAR_IMAGE, [rect, 1, 1, 1]);
}

/// 80055AF0: phase 0x4000 (movie start): entry 2 of `[gp+0x20C]` goes to
/// the picture screen (transition 0x20000). Otherwise music flag, context
/// bit 0x10000000, arena reset to gameplay +0x2B8, the 0x44-byte player
/// block `[gp+0x4D8]` with two 0x400-byte buffers and a 320x256 frame, both
/// VRAM halves cleared, music 7, transition 0x8000.
pub fn movie_start(m: &mut Machine) {
    sdk::draw_sync(m, 0);
    let context = m.gw(CONTEXT);
    if m.lbu(context + 0x3ac) == 2 {
        let gameplay = m.gw(GAMEPLAY);
        let stage = m.lw(gameplay + 8);
        request_transition(m, 0x2_0000, stage);
        return;
    }
    let table = m.gw(0x20c);
    let flag = screen_music_flag(m, table);
    let screen = m.gw(0x204);
    m.sw(screen + 4, flag);
    let context = m.gw(CONTEXT);
    let status = m.gw(0x2c4);
    let flags = m.lw(context);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(context, flags | 0x1000_0000);
    m.sw(status + 0x14, 0);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let block = crate::arena::alloc(m, 0x44);
    m.set_gw(0x4d8, block);
    let first = crate::arena::alloc(m, 0x400);
    let block = m.gw(0x4d8);
    m.sw(block + 4, first);
    let second = crate::arena::alloc(m, 0x400);
    let block = m.gw(0x4d8);
    let context = m.gw(CONTEXT);
    m.sw(block + 8, second);
    m.sw(block, 0);
    for (offset, value) in [
        (0x24, 0),
        (0x26, 0),
        (0x28, 0),
        (0x2a, 0x100),
        (0x2c, 0x140),
        (0x2e, 0x100),
        (0x30, 0),
        (0x32, 0x20),
    ] {
        m.sh(block + offset, value);
    }
    let entry = u32::from(m.lbu(context + 0x3ac));
    let table = m.gw(0x20c);
    let file = m.lh((entry << 3).wrapping_add(table)) as i32 as u32;
    m.sw(block + 0x38, 0);
    m.sw(block + 0x3c, 0);
    m.sw(block + 0x40, 0x78);
    m.sw(block + 0x34, file);
    clear_both_buffers(m);
    let screen = m.gw(0x204);
    if m.lw(screen + 4) != 0 {
        music_select(m, 7, 0, 0);
    }
    let context = m.gw(CONTEXT);
    let next = if m.lbu(context + 0x3ac) == 2 {
        0x1_0000
    } else {
        0x8000
    };
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    request_transition(m, next, stage);
}

/// 80055CEC: phase 0x10000 (movie end): context bit 0x10000000 cleared,
/// status +0x14 = 2, music off, arena and resident reload (800321B4) from
/// gameplay +0x2B8, both VRAM halves cleared, next event (80037B24).
pub fn movie_end(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    let status = m.gw(0x2c4);
    m.sw(context, flags & 0xefff_ffff);
    let screen = m.gw(0x204);
    m.sw(status + 0x14, 2);
    if m.lw(screen + 4) != 0 {
        music_select(m, 0xff, 0, 0);
    }
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    call(m, 0x8003_21b4, [base, 0, 0, 0]);
    clear_both_buffers(m);
    call(m, 0x8003_7b24, [0; 4]);
}

/// 800594E4: phase 0x20000 (picture start): context bit 0x10000000, status
/// +0x14 = 0, arena reset, status flags (+4 = 1, +0x10..+0x12 = 0, word 0
/// | 9), the 0x64-byte block `[gp+0x4E4]` with two 0x2000-byte buffers and
/// a 320x480 frame, VRAM saved (rectangle 800BAC18 into 0x2800 bytes and
/// 768,0 256x284 into 0x23800 bytes), the screen cleared, music 10 and
/// transition 0x40000.
pub fn picture_start(m: &mut Machine) {
    let frame = m.push_frame(0x30);
    let context = m.gw(CONTEXT);
    let status = m.gw(0x2c4);
    let flags = m.lw(context);
    let gameplay = m.gw(GAMEPLAY);
    m.sw(context, flags | 0x1000_0000);
    m.sw(status + 0x14, 0);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let status = m.gw(0x2c4);
    m.sb(status + 0x10, 0);
    m.sw(status + 4, 1);
    m.sb(status + 0x11, 0);
    m.sb(status + 0x12, 0);
    let flags = m.lw(status);
    m.sw(status, flags | 9);
    let block = crate::arena::alloc(m, 0x64);
    m.set_gw(0x4e4, block);
    let first = crate::arena::alloc(m, 0x2000);
    let block = m.gw(0x4e4);
    m.sw(block + 4, first);
    let second = crate::arena::alloc(m, 0x2000);
    let block = m.gw(0x4e4);
    let context = m.gw(CONTEXT);
    m.sw(block + 8, second);
    m.sh(block + 0x4e, 0x1e0);
    m.sw(block, 0);
    m.sh(block + 0x4c, 0x140);
    m.sw(block + 0x60, 0x78);
    let entry = u32::from(m.lbu(context + 0x3ac));
    let table = m.gw(0x20c);
    let file = m.lh((entry << 3).wrapping_add(table)) as i32 as u32;
    m.sw(block + 0x54, 0);
    m.sw(block + 0x50, file);
    let rect = frame + 0x10;
    for i in 0..2 {
        let word = m.lw(0x800b_ac18 + 4 * i);
        m.sw(rect + 4 * i, word);
    }
    let pixels = crate::arena::alloc(m, 0x2800);
    let block = m.gw(0x4e4);
    m.sw(block + 0x58, pixels);
    call(m, STORE_IMAGE, [rect, pixels, 0, 0]);
    m.sh(rect, 0x300);
    m.sh(rect + 4, 0x100);
    m.sh(rect + 2, 0);
    m.sh(rect + 6, 0x11c);
    let pixels = crate::arena::alloc(m, 0x2_3800);
    let block = m.gw(0x4e4);
    m.sw(block + 0x5c, pixels);
    call(m, STORE_IMAGE, [rect, pixels, 0, 0]);
    let clear = frame + 0x18;
    m.sh(clear, 0);
    m.sh(clear + 2, 0);
    m.sh(clear + 4, 0x140);
    m.sh(clear + 6, 0x1f0);
    call(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
    sdk::draw_sync(m, 0);
    let table = m.lw(0x800b_ac00);
    let flag = screen_music_flag(m, table);
    let screen = m.gw(0x218);
    m.sw(screen + 4, flag);
    let screen = m.gw(0x218);
    if m.lw(screen + 4) != 0 {
        music_select(m, 0xa, 0, 0);
    }
    let gameplay = m.gw(GAMEPLAY);
    let stage = m.lw(gameplay + 8);
    request_transition(m, 0x4_0000, stage);
    m.pop_frame(0x30);
}

/// 8005982C: phase 0x80000 (picture end): event queue reset, arena and
/// resident reload from gameplay +0x2B8, the screen cleared, the two saved
/// VRAM areas restored (rectangles gp+0x22C / gp+0x234), music off and the
/// next event (80037B24).
pub fn picture_end(m: &mut Machine) {
    let frame = m.push_frame(0x20);
    call(m, 0x8002_c280, [0; 4]);
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    call(m, 0x8003_21b4, [base, 0, 0, 0]);
    let clear = frame + 0x10;
    m.sh(clear + 4, 0x140);
    m.sh(clear, 0);
    m.sh(clear + 2, 0);
    m.sh(clear + 6, 0x1e0);
    call(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
    let block = m.gw(0x4e4);
    let pixels = m.lw(block + 0x58);
    call(m, LOAD_IMAGE, [GP + 0x22c, pixels, 0, 0]);
    let block = m.gw(0x4e4);
    let pixels = m.lw(block + 0x5c);
    call(m, LOAD_IMAGE, [GP + 0x234, pixels, 0, 0]);
    let screen = m.gw(0x218);
    if m.lw(screen + 4) != 0 {
        music_select(m, 0xff, 0, 0);
    }
    call(m, 0x8003_7b24, [0; 4]);
    m.pop_frame(0x20);
}

/// Native targets owned by this module, by original address.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let v0 = match target {
        0x8004_4e24 => {
            session_release_entities(m);
            0
        }
        0x8003_02a8 => music_none(),
        0x8002_db18 => sound_channel_volume(m, a0, a1, a2),
        0x8004_ca68 => {
            course_targets_reset(m);
            0
        }
        0x8007_5894 => {
            camera_view_apply(m);
            0
        }
        0x8007_23ac => {
            camera_mode_restart(m);
            0
        }
        0x8005_a268 => {
            course_start(m);
            0
        }
        0x8005_a410 => {
            course_end(m);
            0
        }
        0x8005_5af0 => {
            movie_start(m);
            0
        }
        0x8005_5cec => {
            movie_end(m);
            0
        }
        0x8005_94e4 => {
            picture_start(m);
            0
        }
        0x8005_982c => {
            picture_end(m);
            0
        }
        0x8002_b0dc => stream_state(m),
        0x8002_c730 => stream_entry_negative(m, a0),
        0x8007_8010 => {
            camera_replay_view(m);
            0
        }
        0x8007_8048 => {
            camera_set_eye(m, a0, a1);
            0
        }
        0x8007_80c0 => {
            camera_set_target(m, a0, a1, a2);
            0
        }
        0x8007_d4d4 => {
            record_effect_start(m, a0);
            0
        }
        0x8007_d504 => {
            record_effect_rewind(m, a0);
            0
        }
        0x8007_d538 => record_skip(m, a0, a1, a2),
        0x8007_d5d8 => {
            replay_skip_frames(m, a0);
            0
        }
        0x8007_db58 => record_play(m, a0),
        0x8007_d31c => {
            record_particles(m, a0);
            0
        }
        0x8007_e384 => {
            spawn_request(m, a0, a1);
            0
        }
        0x8007_d9cc => {
            record_spawn(m, a0);
            0
        }
        0x8006_b1cc => {
            particle_stop(m, a0);
            0
        }
        0x8006_aee8 => {
            emitter_set(m, a0, a1, a2);
            0
        }
        0x8006_af70 => {
            emitter_stop(m, a0);
            0
        }
        0x8006_af90 => {
            emitter_start(m, a0, a1, a2, a3);
            0
        }
        0x8006_b1e8 => {
            particles_update(m);
            0
        }
        0x8007_dc1c => {
            replay_rewind(m);
            0
        }
        0x8007_df54 => replay_frame(m),
        0x8007_ddcc => {
            replay_start(m);
            0
        }
        0x8007_e1a8 => {
            replay_finish(m);
            0
        }
        0x8007_e76c => pool_alloc(m, a0, a1, a2),
        0x8007_e3a0 => {
            spawn_object(m, a0, a1);
            0
        }
        0x8007_e4dc => {
            spawn_debris(m, a0, a1);
            0
        }
        0x8007_e658 => {
            spawn_smoke(m, a0, a1);
            0
        }
        0x8007_f86c => {
            spawn_variant(m, a0, a1);
            0
        }
        0x8007_eeec => {
            debris_burst(m, a0, a1);
            0
        }
        0x8007_f15c => {
            smoke_trail(m, a0, a1, a2);
            0
        }
        0x8008_0c5c => {
            debris_explosion(m, a0, a1, a2);
            0
        }
        0x8002_c500 => {
            let callback = m.lw(m.sp.wrapping_add(0x10));
            stream_queue(m, a0, a1, a2, a3, callback)
        }
        0x8002_c81c => music_select(m, a0, a1, a2),
        0x8003_02b0 => sound_listener_reset(m),
        0x8003_050c => sound_stop_all(m),
        0x8002_f2c8 => sound_source_set(m, a0, a1),
        0x8007_827c => camera_follow_player(m),
        0x8002_b87c => stream_pending(m),
        0x8002_b5c4 => stream_next(m, a0, a1),
        0x8002_b7b8 => stream_release(m, a0),
        0x8003_8b50 => emitters_clear(m),
        0x8003_8b60 => {
            entities_clear_hidden(m);
            0
        }
        0x8009_1248 => effect_pools_release(m),
        0x8003_90c0 => sequence_commands(m, a0),
        0x8003_90b8 | 0x8003_9088 => command_skip(a0),
        0x8003_8ba0 => command_camera_pose(m, a0),
        0x8003_8c08 => command_camera_lens(m, a0),
        0x8003_8d30 => command_entity_pose(m, a0),
        0x8003_8dec => command_entity_place(m, a0),
        0x8003_8e74 => command_emitter(m, a0),
        0x8003_8f38 => command_effect(m, a0),
        0x8003_9090 => command_overlay(m, a0),
        0x8007_60f8 => camera_set_projection(m, a0, a1),
        0x8007_6140 => {
            let depth = m.lw(m.sp.wrapping_add(0x10));
            camera_set_range(m, a0, a1, a2, a3, depth)
        }
        0x8003_02e4 => sequence_sound(m, a0),
        0x8003_9128 => {
            sequence_stream_done(m);
            1
        }
        0x8003_9138 => sequence_start(m),
        0x8003_926c => sequence_end(m),
        0x8003_8a24 => sequence_restore(m, a0),
        0x8003_9598 => {
            sequence_fade(m);
            0
        }
        0x8009_2164 => letterbox(m, a0, a1, a2),
        0x8009_24b4 => tint_rectangle(m, a0, a1, a2),
        0x8009_2540 => {
            sequence_sparks(m, a0, a1, a2, a3);
            0
        }
        0x8009_27e4 => {
            sequence_burn(m, a0);
            0
        }
        0x8009_23e8 => screen_fade(m, a0, a1, a2, a3),
        _ => return None,
    };
    Some(v0)
}
