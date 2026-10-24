//! Flight replay (state 80034040, update 800330CC / 8007F8E4) and other
//! resident routines no subsystem owns: the replay recorder/player over the
//! `[gp+0x544]` buffer, the particle table at `[gp+0x50C]`, the in-engine
//! sequence player of phases 0x10/0x20/0x40 (800396E8) with its stream,
//! command, sound and fade routines, and the helpers of the remaining
//! flight states.
//!
//! Translated from the original instructions in their original order.
//! Routines owned by other modules are called by original address through
//! [`dispatch::call`]; every routine translated here is claimed by
//! [`dispatch`]. Stack locals whose address the original passes on live in
//! the routine's guest frame.
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY},
    dispatch,
    machine::{Fault, GP, Machine, SmallData},
    sdk,
};
use ac3_gte::{ctrl, reg};

/// `[gp+0x544]`: replay buffer. +0 record count, +4/+8 ring start/end,
/// +0x10 first record, +0x14 frame-done flag, +0x18 entity snapshots (64 x
/// 0x20), +0x1018 frame, +0x101C cursor, +0x1020 entity copies (64 x 0x20),
/// +0x1820 camera offset, +0x1830 target offset, +0x1840..0x1848 camera
/// target state, +0x1850 saved entities (64 x 0x20).
const REPLAY: u32 = 0x544;
/// `[gp+0x50C]`: particle block (+0x33D0 active bit masks, +0x15D0 particles
/// of 0x3C bytes, +0xC8 emitters of 0xA4 bytes, +0x1548 emitter mask).
const PARTICLES: u32 = 0x50c;
/// 80024788 / 80024618: jump tables of 8007F1D4 and 8006C760.
const RECORD_CASES: u32 = 0x8002_4788;
const EMITTER_CASES: u32 = 0x8002_4618;

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
/// 8002759C (shared): copy `words` words from `src` to `dst`.
fn copy_words(m: &mut Machine, dst: u32, src: u32, words: u32) {
    call(m, 0x8002_759c, [dst, src, words, 0]);
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

/// 8002B504: stream state `[[gp+0x34]+0xC]` (5 reads as -1).
pub fn stream_state(m: &mut Machine) -> u32 {
    let block = m.gw(0x34);
    let state = m.lw(block.wrapping_add(0xc));
    if state == 5 { u32::MAX } else { state }
}

/// 8002CB58: 1 when stream state is 1..3 and the 8002995C entry of `id`
/// is negative.
pub fn stream_entry_negative(m: &mut Machine, id: u32) -> u32 {
    let state = stream_state(m);
    if state.wrapping_sub(1) >= 3 {
        return 0;
    }
    let entry = call(m, 0x8002_995c, [id, 0, 0, 0]);
    u32::from((m.lw(entry) as i32) < 0)
}

// ---- camera views of the replay ------------------------------------------------

/// 80079680: point the camera at the player with mode 0x200, then
/// 80035740(0).
pub fn camera_replay_view(m: &mut Machine) {
    let gameplay = m.gw(GAMEPLAY);
    let table = m.gw(ENTITIES);
    let camera = m.lw(gameplay + 0x28);
    let player = m.lw(table);
    call(m, 0x8007_93c4, [camera, 0x200, player, 0]);
    call(m, 0x8003_5740, [0; 4]);
}

/// 800796B8: camera +0x10C = position (3 words) and angles (3 halfwords at
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

/// 80079730: camera target `source`/`offset` with mode `mode` (camera +0x134
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

/// 8007FDB4: allocate one object from a pool: `bitmap` has `words` mask
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

/// 8007EB50: effect record: 8005E460(byte +4, byte +5, header >> 23).
pub fn record_effect_start(m: &mut Machine, record: u32) {
    let header = m.lw(record);
    let a = u32::from(m.lbu(record.wrapping_add(4)));
    let b = u32::from(m.lbu(record.wrapping_add(5)));
    call(m, 0x8005_e460, [a, b, header >> 23, 0]);
}

/// 8007EB80: effect record (rewind): 8005E460(byte +4, byte +5,
/// header >> 14 & 0x1FF).
pub fn record_effect_rewind(m: &mut Machine, record: u32) {
    let header = m.lw(record);
    let a = u32::from(m.lbu(record.wrapping_add(4)));
    let b = u32::from(m.lbu(record.wrapping_add(5)));
    call(m, 0x8005_e460, [a, b, (header >> 14) & 0x1ff, 0]);
}

/// 8007EBB4: replay one record while skipping (type 0 sets `*done` at a
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

/// 8007EC54: run every recorded frame through [`record_skip`] (effects only,
/// `forward` selects 8007EB50 or 8007EB80).
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

/// 8007F1D4: replay one record (jump table 80024788 on the type: entity
/// position/state 8007DC3C/8007DD58, particles 8007E998, spawns 8007F048,
/// effects 8007EB50); returns the next record.
pub fn record_play(m: &mut Machine, record: u32) -> u32 {
    let copies = replay_buffer(m).wrapping_add(0x1020);
    let kind = (m.lw(record) >> 4) & 7;
    if kind < 5 {
        let target = m.lw(RECORD_CASES + kind * 4);
        match target {
            0x8007_f22c => {
                call(m, 0x8007_dc3c, [record, copies, 0, 0]);
            }
            0x8007_f23c => {
                call(m, 0x8007_dd58, [record, copies, 0, 0]);
            }
            0x8007_f24c => record_particles(m, record),
            0x8007_f25c => record_spawn(m, record),
            0x8007_f26c => record_effect_start(m, record),
            _ => m.raise(Fault::Untranslated { address: target }),
        }
    }
    next_record(m, record)
}

/// 8007E998: particle record: emitter (8006C760 / 8006C6B8 with positions
/// unpacked from 20.12 words) or stop (8006C99C / 8006C740).
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

/// 8007F9CC: spawn request at `[gp+0x41C]+0x3948`: flag 1, halfwords a, b.
pub fn spawn_request(m: &mut Machine, a: u32, b: u32) {
    let block = m.gw(0x41c);
    let request = block.wrapping_add(0x3948);
    m.sw(request, 1);
    m.sh(request + 4, a as u16);
    m.sh(request + 6, b as u16);
}

/// 8007F048: spawn record (low nibble 1: 8007F9CC, 3: 8007F9E8, 4: 8007FB24,
/// 5: 8007FCA0).
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

// ---- replay spawn records (8007F048 types 3..5) ---------------------------------

/// 80024814 / 80024834: jump tables of 8007F9E8 and 8007FB24.
const SPAWN_CASES: u32 = 0x8002_4814;
const DEBRIS_CASES: u32 = 0x8002_4834;

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// Effect pool at `[gp+0x41C] + offset` ([`pool_alloc`]).
fn effect_alloc(m: &mut Machine, offset: u32, words: u32, size: u32) -> u32 {
    let block = m.gw(0x41c);
    pool_alloc(m, block.wrapping_add(offset), words, size)
}

/// 8007F9E8: spawn record `kind` (jump table 80024814) at the record's
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
            0x8007_fa48 => Some((0x8007_fe28, [position, byte, 0, 0])),
            0x8007_fa64 => Some((0x8008_0140, [position, word2 & 0xf, byte, 0])),
            0x8007_fa84 => Some((0x8008_2594, [position, (word1 >> 8) & 0xf, 0, 0])),
            0x8007_faa0 => Some((0x8008_2d80, [position, byte, 0, 0])),
            0x8007_fabc => Some((0x8008_9bb4, [position, byte, 0, 0])),
            0x8007_fad8 => {
                spawn_variant(m, position, byte);
                None
            }
            0x8007_faf4 => Some((0x8008_3c3c, [position, byte, (word1 >> 8) & 0xf, 0])),
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

/// 8007FB24: debris record `kind` (jump table 80024834) with the position
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
            0x8007_fbac => debris_burst(m, position, velocity),
            0x8007_fbc0 | 0x8007_fbd8 | 0x8007_fc44 => {
                let routine = match target {
                    0x8007_fbc0 => 0x8008_0f70,
                    0x8007_fbd8 => 0x8008_1bc8,
                    _ => 0x8008_27e0,
                };
                let byte = u32::from(m.lbu(record.wrapping_add(2)));
                call(m, routine, [position, velocity, byte, a3]);
            }
            0x8007_fbf0 => {
                call(m, 0x8008_1d4c, [position, velocity, a2, a3]);
            }
            0x8007_fc04 => {
                let header = m.lw(record);
                call(
                    m,
                    0x8008_1fdc,
                    [position, velocity, (header >> 9) & 0x7f, a3],
                );
            }
            0x8007_fc24 => {
                let header = m.lw(record);
                debris_explosion(m, position, velocity, (header >> 9) & 0x7f);
            }
            0x8007_fc5c => {
                call(m, 0x8008_2a3c, [position, velocity, a2, a3]);
            }
            0x8007_fc70 => {
                call(m, 0x8008_2c1c, [position, velocity, a2, a3]);
            }
            0x8007_fc84 => debris_column(m, position, velocity),
            _ => m.raise(Fault::Untranslated { address: target }),
        }
    }
    m.pop_frame(0x38);
}

/// 8007FCA0: smoke record `kind` 0 (800807A4, size packed from word +0x10)
/// or 1 (80083054 with the velocity scaled by `1 << (word+0x10 & 0xFFF)`
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
        call(m, 0x8008_3054, [position, velocity, scale, byte]);
    }
    m.pop_frame(0x38);
}

/// 80080EB4: call 80080A94 (`variant` even) or 80080C08 (odd) with the
/// three position words.
pub fn spawn_variant(m: &mut Machine, position: u32, variant: u32) {
    let routine = if variant & 1 == 0 {
        0x8008_0a94
    } else {
        0x8008_0c08
    };
    let x = m.lw(position);
    let y = m.lw(position.wrapping_add(4));
    let z = m.lw(position.wrapping_add(8));
    call(m, routine, [x, y, z, 0]);
}

/// The flash (pool +0x600) and light (pool +0x33C0, random lifetime and
/// size; 800807A4 also stores `light`) of 80080534 and 800807A4.
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

/// 80080534: explosion burst: flash, light and eight debris pieces (pool
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

/// 800807A4: smoke trail: flash, light (`packed` size) and eight pieces
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

/// 800822A4: explosion at `position` + `velocity` + random jitter: with
/// `mode` 1 an 80082D80 blast, otherwise a flash and a fireball (pool
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
        call(m, 0x8008_2d80, [position, 0x200, mode, 0]);
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

/// 800833C8: water column at `position` + 8 x `velocity` (block
/// `[gp+0x41C]+0x3B28`), clipped against the 8009A800 hit test.
pub fn debris_column(m: &mut Machine, position: u32, velocity: u32) {
    // One implementation: effects.rs owns 800833C8.
    crate::dispatch::call(m, 0x8008_33c8, [position, velocity, 0, 0]);
}

// ---- particles -----------------------------------------------------------------

fn particle(m: &mut Machine, index: u32) -> u32 {
    let block = m.gw(PARTICLES);
    block
        .wrapping_add(index.wrapping_mul(0x3c))
        .wrapping_add(0x15d0)
}

/// 8006C99C: clear particle `index` word +4 (its lifetime).
pub fn particle_stop(m: &mut Machine, index: u32) {
    let block = m.gw(PARTICLES);
    let at = index.wrapping_mul(0x3c).wrapping_add(block);
    m.sw(at.wrapping_add(0x15d4), 0);
}

/// 8006C6B8: emitter `index` on (mask +0x1548), parameter `value` (two
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

/// 8006C740: emitter `index` off (mask +0x1548).
pub fn emitter_stop(m: &mut Machine, index: u32) {
    let block = m.gw(PARTICLES);
    let mask = m.lw(block.wrapping_add(0x1548));
    m.sw(block.wrapping_add(0x1548), mask & !(1 << (index & 31)));
}

/// 8006C760: start particle `index` of `kind` (jump table 80024618) at
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
        0x8003_0778,
        [position.wrapping_add(0x10), frame + 0x10, 0, 0],
    );
    set_rot_matrix(m, frame + 0x10);
    let particle = particle(m, index);
    if kind < 6 {
        let target = m.lw(EMITTER_CASES + kind * 4);
        let (flags, life, z) = match target {
            0x8006_c82c => (0x0002_0410, 0x25, 0x800),
            0x8006_c840 => (0x0002_0820, 0x18, 0x990),
            0x8006_c85c => (0x0004_1040, 0x3e, 0x4c0),
            0x8006_c878 => (0x0008_2080, 0x23, 0),
            0x8006_c88c => (0x0009_4103, 0x10, 0),
            0x8006_c8b0 => (0x0009_8202, 0x10, 0x800),
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

/// 8006C9B8: advance the 128 particles: lifetime, position (+8 and +0x18
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

/// 8007F298: rewind the replay: reset the frame/cursor and camera state,
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
    call(m, 0x8003_0778, [buffer + 0x1038, frame + 0x18, 0, 0]);
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
    call(m, 0x8008_49ec, [0; 4]);
    call(m, 0x8007_da1c, [0; 4]);
    call(m, 0x8006_c530, [0; 4]);
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

// JP-REVIEW(R0405): near function: PAL 8007DF54 -> JP 8007F5B4, del:2 imm:2 ins:2, 6 instructions differ (whole routine)
/// 8007F5B4: play one replay frame; returns 1 once every recorded frame
/// was played (else 0).
// JP-PATCH: replaced by `crate::jp::res_p1_replay::replay_frame` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn replay_frame(m: &mut Machine) -> u32 {
    crate::jp::res_p1_replay::replay_frame(m)
}

// JP-REVIEW(R0408): near function: PAL 8007DDCC -> JP 8007F448, del:7 imm:4 op:1, 12 instructions differ (whole routine)
/// 8007F448: start the replay (phase 0x80): clear the status bytes, block
/// pausing, queue the replay music (the stage's stream when its entry is
/// not negative, else sequence 800B4334 + 12 * (context +0x390 mod 6 + 1)),
/// reset slots, set the replay camera, save the entities, rewind, skip the
/// effects of every frame, leave bit 0x10000000 of gameplay and request
/// transition 0x100.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::replay_start` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn replay_start(m: &mut Machine) {
    crate::jp::res_p1_replay::replay_start(m)
}

// JP-REVIEW(R0409): near function: PAL 8007E1A8 -> JP 8007F808, imm:1, 1 instructions differ (whole routine)
/// 8007F808: end the replay (phase 0x200): flush the sequencer, rewind,
/// replay the effects forward, clear the buffer counters, allow pausing,
/// restore the entities and slots, then either block pausing again (replay
/// mode) or run the mission step 80037E50.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::replay_finish` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn replay_finish(m: &mut Machine) {
    crate::jp::res_p1_replay::replay_finish(m)
}

// ---- in-engine sequences (phase 0x10 / 0x20 / 0x40, update 800396E8) --------------
//
// gp+0x3EC current record, +0x3E0 fade state, +0x3E4 frame count, +0x3E8
// playing, +0x3EC skip requested, +0x3F0 frames left (halfword), +0x3F4
// stream finished, +0x3FC/+0x400 stream chunk / header, +0x404/+0x406 record
// count / index (halfwords), +0x408 wait counter. Context +0xF7C is the
// sequence mode (1..; 0 is no sequence).

/// 800B0F10: sequence command handlers, by opcode (header & 0x7F).
const SEQUENCE_COMMANDS: u32 = 0x800b_0f10;
/// 800C6610: sound source count; 800C6620: listener (4 words); 800C6630:
/// sound sources of 0x5C bytes.
const SOUND_SOURCE_COUNT: u32 = 0x800c_6610;
const SOUND_LISTENER: u32 = 0x800c_6620;
const SOUND_SOURCES: u32 = 0x800c_6630;
/// 800B03AC: music table, 20 bytes per entry.
const MUSIC_TABLE: u32 = 0x800b_03ac;

/// 8002C928: queue a type-4 (stream) event in the event ring `[gp+0x48]`
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

/// 8002CC44: select music `id` (0xFF: none) for the three channels of
/// `[gp+0x314]`: clear the channel state, then copy the entry's track count
/// and flags from the music table 800B03AC. Returns 0.
pub fn music_select(m: &mut Machine, id: u32, a1: u32, a2: u32) -> u32 {
    for offset in [0x322, 0x321, 0x320, 0x31b, 0x31a] {
        m.sb(GP + offset, 0);
    }
    let mut block = 0;
    for i in 0..3u32 {
        let channels = m.gw(0x314);
        m.sb(channels.wrapping_add(i) + 4, 0);
        let channels = m.gw(0x314);
        m.sb(channels.wrapping_add(i) + 7, 0);
        block = m.gw(0x314);
        m.sh(block.wrapping_add(i << 1) + 0xa, 0);
    }
    if id == 0xff {
        m.sb(GP + 0x318, 0);
        return 0;
    }
    let entry = id.wrapping_mul(20).wrapping_add(MUSIC_TABLE);
    m.sb(GP + 0x318, 1);
    m.sw(block, id);
    m.sw(block + 0x10, a1);
    m.sw(block + 0x14, a2);
    let tracks = u32::from(m.lbu(entry + 2));
    m.sh(block + 0x18, (tracks << 2) as u16);
    let tracks = u32::from(m.lbu(entry + 2));
    m.sh(block + 0x1a, tracks.wrapping_mul(6) as u16);
    let flags = u32::from(m.lhu(entry + 0x10));
    m.set_gw(0x31c, u32::MAX);
    m.sh(block + 0x1c, (((flags & 0x3ff) >> 6) | 0x30) as u16);
    0
}

/// 800304F4: 8002F500(2) and clear the sound listener words 800C6620..F0.
pub fn sound_listener_reset(m: &mut Machine) -> u32 {
    call(m, 0x8002_f500, [2, 0, 0, 0]);
    m.sw(SOUND_LISTENER + 8, 0);
    m.sw(SOUND_LISTENER + 4, 0);
    m.sw(SOUND_LISTENER, 0);
    0
}

/// 80030750: 8002DDE0 and 8002F620 (stop voices and sound channels).
pub fn sound_stop_all(m: &mut Machine) -> u32 {
    call(m, 0x8002_dde0, [0; 4]);
    call(m, 0x8002_f620, [0; 4]);
    0
}

/// 8002F6E4: sound source `index` word +0x24 = `value`.
pub fn sound_source_set(m: &mut Machine, index: u32, value: u32) -> u32 {
    let source = index.wrapping_mul(0x5c).wrapping_add(SOUND_SOURCES);
    m.sw(source + 0x24, value);
    source
}

/// 800798EC: camera target mode 0x1000 on the player (800793C4), then
/// 80035740(0).
pub fn camera_follow_player(m: &mut Machine) -> u32 {
    let gameplay = m.gw(GAMEPLAY);
    let entities = m.gw(ENTITIES);
    let camera = m.lw(gameplay + 0x28);
    let player = m.lw(entities);
    call(m, 0x8007_93c4, [camera, 0x1000, player, 0]);
    call(m, 0x8003_5740, [0; 4])
}

/// 8002BCA4: chunks left in the stream (`[gp+0x40]` +0x34) while the stream
/// state `[gp+0x38]` +0x20 is 0x40 (playing), else 0.
pub fn stream_pending(m: &mut Machine) -> u32 {
    let state = m.gw(0x38);
    if m.lw(state + 0x20) != 0x40 {
        return 0;
    }
    let stream = m.gw(0x40);
    m.lw(stream + 0x34)
}

/// 8002B9EC: take the next stream chunk. Returns 1 when none is ready
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

/// 8002BBE0: release the `chunk +2` consumed chunks from `chunk` (type 2 ->
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
            sdk::printf(m, [0x8001_0430, index, chunk, at]);
            let stream = m.gw(0x40);
            let position = m.lw(stream + 0x2c);
            sdk::printf(m, [0x8001_0464, position, 0, 0]);
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

/// 80038E48: clear the emitter mask (particle block +0x1548).
pub fn emitters_clear(m: &mut Machine) -> u32 {
    let block = m.gw(PARTICLES);
    m.sw(block + 0x1548, 0);
    block
}

/// 80038E58: clear bit 0x10000 of word 0 of all 64 entities.
pub fn entities_clear_hidden(m: &mut Machine) {
    for i in 0..0x40 {
        let object = entity(m, i);
        let flags = m.lw(object);
        m.sw(object, flags & 0xfffe_ffff);
    }
}

/// 80092AB8: release the used objects of the three effect pools of
/// `[gp+0x41C]` (+0x108C used masks cleared from the +0x600 bitmaps) and
/// clear its +0x33C0 / +0x3964 / +0x3B28 counters.
pub fn effect_pools_release(m: &mut Machine) -> u32 {
    let base = m.gw(0x41c);
    for i in 0..3u32 {
        let at = base.wrapping_add(4 * i);
        let used = m.lw(at + 0x108c);
        let bitmap = m.lw(at + 0x600);
        m.sw(at + 0x108c, 0);
        m.sw(at + 0x600, bitmap & !used);
    }
    let base = m.gw(0x41c);
    m.sw(base + 0x33c0, 0);
    m.sw(base + 0x3964, 0);
    m.sw(base + 0x3b28, 0);
    base
}

/// 800393B8: run the sequence commands from `record` through the handler
/// table 800B0F10 until opcode 0; returns the end of the list.
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

/// 800393B0 / 80039380: commands 0 and 6, one word (no effect).
pub fn command_skip(record: u32) -> u32 {
    record.wrapping_add(4)
}

/// 80038E98: command 1, camera pose: angles +4..+8 into camera +0x14..+0x18,
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

/// 80038F00: command 7, camera lens and target: a nonzero, changed +2
/// (projection) applies 80077768; a changed +4 / +6 (near / far) applies
/// 800777B0; then the camera targets entity (header >> 10) & 0x3F in mode
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
    call(m, 0x8007_93c4, [camera, mode, target, 0]);
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

/// 80039028: command 2, entity pose; header bit 14 sets (else clears) bit 3
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

/// 800390E4: command 3, entity pose without the flag.
pub fn command_entity_place(m: &mut Machine, record: u32) -> u32 {
    command_entity(m, record);
    record.wrapping_add(0x18)
}

/// 8003916C: command 4, emitter (header >> 11) & 0x1F: when header bits
/// 8..10 are 0, bit 21 starts it (8006C6B8 with 0x1801 and the position /
/// angles of the record) or else stops it (8006C740).
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

/// 80039230: command 5, effect: sub-command (header >> 8) & 0x1F through the
/// jump table 800239E4 to the 8009xxxx effect routines.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::command_effect` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn command_effect(m: &mut Machine, record: u32) -> u32 {
    crate::jp::res_p1_replay::command_effect(m, record)
}

/// 80039388: command 8, 80093944(record); eight words.
pub fn command_overlay(m: &mut Machine, record: u32) -> u32 {
    call(m, 0x8009_3944, [record, 0, 0, 0]);
    record.wrapping_add(0x20)
}

/// 80077768: camera +0x24 = projection `h`, SetGeomScreen(h), and the depth
/// cue for the camera's near/far (8005E7EC).
pub fn camera_set_projection(m: &mut Machine, camera: u32, h: u32) -> u32 {
    m.sh(camera + 0x24, h as u16);
    sdk::set_geom_screen(m, h);
    let near = m.lh(camera + 0x28) as i32 as u32;
    let far = m.lh(camera + 0x2a) as i32 as u32;
    call(m, 0x8005_e7ec, [near, far, h, 0])
}

/// 800777B0: camera near/far clamped to 1000..0x7C17 (0 if far < near):
/// depth buckets 8005EAB8(near, far, `depth`), camera +0x28/+0x2A, then the
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
    call(m, 0x8005_eab8, [near as u32, far as u32, depth, 0]);
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

/// 80030528: sound listener and sources for one sequence frame: listener
/// pose from the record (80083DFC into 800C6620), source ids (8002F770),
/// source positions / angles / volumes, 8002F9AC, then the one-shot sounds
/// from +0x78 until id -1 (8002F3C0).
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
    call(m, 0x8008_3dfc, [frame + 0x10, SOUND_LISTENER, 0, 0]);
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
    call(m, 0x8002_f770, [frame + 0x38, frame + 0x48, 0, 0]);
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
    call(m, 0x8002_f9ac, [frame + 0x10, 0, 0, 0]);
    let sounds = sources.wrapping_add(0x60);
    while m.lh(sounds) != -1 {
        let position = [8, 0xc, 0x10].map(|offset| m.lw(sounds + offset) << 12);
        for (i, word) in position.into_iter().enumerate() {
            m.sw(frame + 0x28 + 4 * i as u32, word);
        }
        let id = m.lh(sounds) as i32 as u32;
        call(m, 0x8002_f3c0, [id, frame + 0x28, 4, 0]);
    }
    m.pop_frame(0x70);
    0
}

/// 80039420: stream callback, the sequence stream ended (gp+0x404 = 1).
pub fn sequence_stream_done(m: &mut Machine) {
    m.set_gw(0x404, 1);
}

// JP-REVIEW(R0411): near function: PAL 80039138 -> JP 80039430, call~:1 imm:2 ins:28 op:1, 32 instructions differ (whole routine)
/// 80039430: start the sequence of the current mode: frame count from its
/// object, camera on the player, gameplay bit 0 and context bit 0x10000000,
/// event queue reset, a 0x48C0-byte stream buffer for its stream file
/// (8002C928 with callback 80039420), sound and effect resets, music 1 and
/// transition 0x20.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::sequence_start` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn sequence_start(m: &mut Machine) -> u32 {
    crate::jp::res_p1_replay::sequence_start(m)
}

// JP-REVIEW(R0413): changed function: PAL 8003926C -> JP 800395D4, del:1 imm:1 ins:26 op:3, 31 instructions differ (whole routine)
/// 800395D4: end the sequence: event queue reset, its final-state commands
/// (object +4), music off, sounds stopped, context bit 0x10000000 cleared and
/// 80038D1C(0).
// JP-PATCH: replaced by `crate::jp::res_p1_replay::sequence_end` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn sequence_end(m: &mut Machine) -> u32 {
    crate::jp::res_p1_replay::sequence_end(m)
}

/// 80038D1C: restore the camera (0x63 words) and the 64 entity snapshots
/// (8 words each; `flags_only` restores word 0 only and re-places the
/// player with 80072780) saved at `[gp+0x3E4]`, reset the arena to
/// `[gp+0x3E8]`, clear context bit 0x10000000, reset the effect lists and
/// choose the next event (80037E50).
pub fn sequence_restore(m: &mut Machine, flags_only: u32) -> u32 {
    let frame = m.push_frame(0x48);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay + 0x28);
    let saved = m.gw(0x3e4);
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
        let saved = m.gw(0x3e4).wrapping_add(offset);
        if flags_only != 0 {
            copy_words(m, frame + 0x18, saved, 8);
            let object = entity(m, i);
            let word = m.lw(frame + 0x18);
            m.sw(object, word);
            if i == 0 {
                call(m, 0x8007_2780, [object.wrapping_add(8), 0, 0, 0]);
            }
        } else {
            let object = entity(m, i);
            copy_words(m, object, saved, 8);
        }
        offset += 0x20;
    }
    let pointer = m.gw(0x3e8);
    crate::arena::reset(m, pointer);
    call(m, 0x8008_49ec, [0; 4]);
    let context = m.gw(CONTEXT);
    let flags = m.lw(context);
    m.sw(context, flags & 0xefff_ffff);
    call(m, 0x8006_c530, [0; 4]);
    let v0 = call(m, 0x8003_7e50, [0; 4]);
    m.pop_frame(0x48);
    v0
}

// JP-REVIEW(R0414): changed function: PAL 80039598 -> JP 800399D4, del:59 imm:3 ins:148 op:20 reg:1, 231 instructions differ (whole routine)
/// 800399D4: sequence fade and letterbox. gp+0x3F0 is the fade state (1/3
/// fading in, 2 shown, 4 fading out, 5 done), gp+0x9C the fade level and
/// gp+0xA0 the letterbox level. Pad bit 0x40 skips (state 1 -> 3, 2 -> 4);
/// fading out ends with transition 0x40. Then the fade (80093BC4) and the
/// letterbox (PAL 80092164) packets, and for modes 0x17/0x18/0x19/0x1B the
/// title overlay 8008FC9C over its frame window (the original's second,
/// 80084474 overlay needs bit 0 of a mask that is never set).
// JP-PATCH: replaced by `crate::jp::res_p1_replay::sequence_fade` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn sequence_fade(m: &mut Machine) {
    crate::jp::res_p1_replay::sequence_fade(m)
}

// JP-REVIEW(R0417): near function: PAL 800923E8 -> JP 80093BC4, imm:1, 1 instructions differ (whole routine)
/// 80093BC4: fade (`level` < 0x1000): a full-screen rectangle of strength
/// 0x1000 - `level` (negated when `additive` is 0) drawn by 80093B2C, then a
/// mask-bit packet (E6000001); returns the next free packet.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::screen_fade` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn screen_fade(m: &mut Machine, ot: u32, packets: u32, additive: u32, level: u32) -> u32 {
    crate::jp::res_p1_replay::screen_fade(m, ot, packets, additive, level)
}

// JP-REVIEW(R0418): near function: PAL 800924B4 -> JP 80093C90, imm:2, 2 instructions differ (whole routine)
/// 80093C90: red tint (far colour 0x30/0/0, RGB 0x6203135F, IR0 =
/// 0x1000 - `level`, DPCS) as a full-screen semi-transparent rectangle at
/// ordering-table entry `ot`; returns the next free packet (nothing when
/// `level` is 0).
// JP-PATCH: replaced by `crate::jp::res_p1_replay::tint_rectangle` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn tint_rectangle(m: &mut Machine, ot: u32, packets: u32, level: u32) -> u32 {
    crate::jp::res_p1_replay::tint_rectangle(m, ot, packets, level)
}

// JP-REVIEW(R0420): near function: PAL 80092540 -> JP 80093D1C, imm:3, 3 instructions differ (whole routine)
/// 80093D1C: sparks at (`x`, `y`, `z`): on odd `phase` a spark of the pool
/// at effects +0x600 (3 masks, 7 words), then a random-chance ember and a
/// smoke puff of the pool at +0x1098 (10 masks, 7 words), all with random
/// spread, life and size.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::sequence_sparks` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn sequence_sparks(m: &mut Machine, x: u32, y: u32, z: u32, phase: u32) {
    crate::jp::res_p1_replay::sequence_sparks(m, x, y, z, phase)
}

/// 80093FC0: the burning-sky effect of sequence mode 0x16 at sequence frame
/// `frame` (0xB..0x2DF): a pulsing red tint (rsin, 80093C90), then until
/// frame 0x186 a random camera shake (gameplay +0x290/+0x292) and on odd
/// frames sparks (80093D1C) along four fixed paths.
pub fn sequence_burn(m: &mut Machine, frame: u32) {
    let frame = frame as i32;
    if !(0xb..0x2e0).contains(&frame) {
        return;
    }
    let sine = call(m, 0x800a_60f8, [(frame << 6) as u32, 0, 0, 0]) as i32;
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
    let effects = m.gw(0x41c);
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

/// 800C6FD0: per-entity session flags (one word per entity).
const ENTITY_SESSION_FLAGS: u32 = 0x800c_6fd0;
/// 800C70D0: per-entity session bytes.
const ENTITY_SESSION_BYTES: u32 = 0x800c_70d0;

/// 8004596C: at the end of a session before a non-mission event: 80038320
/// for entities 2..0x1F whose session flags have bit 1, then for entities
/// 2..0x3F with bit 2: clear their session byte, the bit, and entity word 0
/// bit 0x4000.
pub fn session_release_entities(m: &mut Machine) {
    for index in 2..0x20u32 {
        let flags = m.lw(ENTITY_SESSION_FLAGS + 4 * index);
        if flags & 2 != 0 {
            call(m, 0x8003_8320, [index, 0, 0, 0]);
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

// ---- phase-0x400 courses (update 8005C270) --------------------------------------
//
// gp+0x334 is the course index (800B1FC4 + 4 x index: kind byte, two target
// slots at +2/+3), gp+0x4E4 the course flags set by 8005B654 (0x300 / 0x400
// / 0x800 / 0x1000 / 0x2000 / 0x4000 / 0x8000 select the lesson).

/// 800B1FC4: course table (4 bytes per course).
const COURSES: u32 = 0x800b_1fc4;
/// 80024730: jump table of 80076F04, by camera view gp+0x2A0.
const VIEW_CASES: u32 = 0x8002_4730;
/// 800C5830: sound channel table (20 bytes per channel, 0x18 channels).
const SOUND_CHANNELS: u32 = 0x800c_5830;

/// 800304EC: no-op music hook; returns 0.
pub fn music_none() -> u32 {
    0
}

/// 8002DF40: channel `index` (< 0x18) volume byte +0x10 = `volume` * 128 /
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

/// 8004E060: reset the (up to two) target objects of the current course:
/// object `slot` of `[gp+0x34C]` (0xB8 bytes) gains 0x200000, its +0x1C
/// takes the model byte of `[gp+0x384]` slot +0x30 and +0x1D/+0x1E/+0x38/
/// +0x3C/+0x3D are reset.
pub fn course_targets_reset(m: &mut Machine) {
    for i in 0..2u32 {
        let course = (m.gw(0x334) << 2).wrapping_add(COURSES);
        let slot = u32::from(m.lbu(course.wrapping_add(i) + 2));
        if slot == 0 {
            continue;
        }
        let models = m.gw(0x384);
        let model = m.lw((slot << 6).wrapping_add(models) + 0x30);
        let object = m.gw(0x34c).wrapping_add(slot.wrapping_mul(0xb8));
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

/// 80076F04: apply camera view gp+0x2A0 (0..6) through the jump table
/// 80024730 (80076B18 / 80076C54 / 80076360 / 80076DEC, second argument 0).
pub fn camera_view_apply(m: &mut Machine) {
    let view = m.gw(0x2a0);
    if view >= 7 {
        return;
    }
    let target = m.lw((view << 2).wrapping_add(VIEW_CASES));
    match target {
        0x8007_6f38 => call(m, 0x8007_6b18, [0, 0, 0, 0]),
        0x8007_6f40 => call(m, 0x8007_6b18, [1, 0, 0, 0]),
        0x8007_6f54 => call(m, 0x8007_6c54, [0, 0, 0, 0]),
        0x8007_6f5c => call(m, 0x8007_6c54, [1, 0, 0, 0]),
        0x8007_6f70 => call(m, 0x8007_6360, [0, 0, 0, 0]),
        0x8007_6f80 => call(m, 0x8007_6dec, [0, 0, 0, 0]),
        0x8007_6f88 => call(m, 0x8007_6dec, [1, 0, 0, 0]),
        _ => {
            m.raise(Fault::Untranslated { address: target });
            0
        }
    };
}

/// 80073A30: clear the player flight state +0x10 and `[gp+0x524]` +0x24,
/// then re-enter the camera mode `[gp+0x520]` +4 (8 / 0x100 / 0x4000 /
/// 0x8000 / 0x4000000) with its first-frame flag.
pub fn camera_mode_restart(m: &mut Machine) {
    let state = m.gw(0x520);
    let other = m.gw(0x524);
    m.sw(state + 0x10, 0);
    m.sw(other + 0x24, 0);
    let mode = m.lw(state + 4);
    match mode {
        0x4000 => {
            call(m, 0x8007_6dec, [0, 1, 0, 0]);
        }
        8 => {
            let near = u32::from(m.gw(0x2a0) != 0);
            call(m, 0x8007_6b18, [near, 1, 0, 0]);
        }
        0x100 => {
            let near = u32::from(m.gw(0x2a0) != 2);
            call(m, 0x8007_6c54, [near, 1, 0, 0]);
        }
        0x8000 => {
            call(m, 0x8007_6dec, [1, 1, 0, 0]);
        }
        0x0400_0000 => {
            call(m, 0x8007_6360, [1, 0, 0, 0]);
        }
        _ => {}
    }
}

/// 8005BC64: phase 0x400 (course intro): a fade-in for lessons 0x6000 with
/// progress +0x3F = 1; after 0x3D frames and no radio message, the course
/// camera (800762D0 for 0x8000, else 80076F04), the lesson's first radio
/// message (8004CEB0) and transition 0x800. Any of pad 0x7000 skips the
/// course (transition 0x2000).
// JP-PATCH: replaced by `crate::jp::res_p1_replay::course_start` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn course_start(m: &mut Machine) {
    crate::jp::res_p1_replay::course_start(m)
}

/// 8005BE0C: phase 0x2000 (course end) by result gameplay +8: 2 (cleared)
/// marks the lesson at its end frame and leaves after the lesson's window
/// (lesson 0x8000 always waits for pad 0x200); 0x20 (failed) leaves from
/// frame 0x78; another nonzero result restarts the course (transition
/// 0x1000) from frame 0x78; 0 leaves from frame 0x96. Pad 0x7000 skips.
/// The lesson's result message (8004CEB0) shows on frame 0.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::course_end` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn course_end(m: &mut Machine) {
    crate::jp::res_p1_replay::course_end(m)
}

// ---- movie (phase 0x4000) and picture (phase 0x20000) events --------------------
//
// Context +0x3AC selects the entry of `[gp+0x220]` (8 bytes: +0 file, +2
// music flag); `[gp+0x218]` / `[gp+0x22C]` +4 is the music-on flag of the
// movie / picture screens. ClearImage, LoadImage and StoreImage are libgpu
// calls (800A8318 / 800A8440 / 800A84A0) made through `dispatch`.

const CLEAR_IMAGE: u32 = 0x800a_8318;
const LOAD_IMAGE: u32 = 0x800a_8440;

// JP-REVIEW(R0427): near function: PAL 80055AF0 -> JP 800573E8, del:1 imm:7 ins:24 op:4 reg:3, 39 instructions differ (whole routine)
/// 800573E8: phase 0x4000 (movie start): entry 2 of `[gp+0x220]` goes to
/// the picture screen (transition 0x20000). Otherwise music flag, context
/// bit 0x10000000, arena reset to gameplay +0x2B8, the 0x44-byte player
/// block `[gp+0x4D0]` with two 0x400-byte buffers and a 320x256 frame, both
/// VRAM halves cleared, music 7, transition 0x8000.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::movie_start` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn movie_start(m: &mut Machine) {
    crate::jp::res_p1_replay::movie_start(m)
}

// JP-REVIEW(R0428): changed function: PAL 80055CEC -> JP 80057640, del:3 imm:4 ins:3 op:1 reg:4, 15 instructions differ (whole routine)
/// 80057640: phase 0x10000 (movie end): context bit 0x10000000 cleared,
/// status +0x14 = 2, music off, arena and resident reload (800323F8) from
/// gameplay +0x2B8, both VRAM halves cleared, next event (80037E50).
// JP-PATCH: replaced by `crate::jp::res_p1_replay::movie_end` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn movie_end(m: &mut Machine) {
    crate::jp::res_p1_replay::movie_end(m)
}

// JP-REVIEW(R0429): near function: PAL 800594E4 -> JP 8005AE8C, imm:4 ins:24 op:1, 29 instructions differ (whole routine)
/// 8005AE8C: phase 0x20000 (picture start): context bit 0x10000000, status
/// +0x14 = 0, arena reset, status flags (+4 = 1, +0x10..+0x12 = 0, word 0
/// | 9), the 0x64-byte block `[gp+0x4DC]` with two 0x2000-byte buffers and
/// a 320x480 frame, VRAM saved (rectangle 800BCDAC into 0x2800 bytes and
/// 768,0 256x284 into 0x23800 bytes), the screen cleared, music 10 and
/// transition 0x40000.
// JP-PATCH: replaced by `crate::jp::res_p1_replay::picture_start` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn picture_start(m: &mut Machine) {
    crate::jp::res_p1_replay::picture_start(m)
}

/// 8005B234: phase 0x80000 (picture end): event queue reset, arena and
/// resident reload from gameplay +0x2B8, the screen cleared, the two saved
/// VRAM areas restored (rectangles gp+0x240 / gp+0x248), music off and the
/// next event (80037E50).
pub fn picture_end(m: &mut Machine) {
    let frame = m.push_frame(0x20);
    call(m, 0x8002_c6a8, [0; 4]);
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    crate::arena::reset(m, base);
    let gameplay = m.gw(GAMEPLAY);
    let base = m.lw(gameplay + 0x2b8);
    call(m, 0x8003_23f8, [base, 0, 0, 0]);
    let clear = frame + 0x10;
    m.sh(clear + 4, 0x140);
    m.sh(clear, 0);
    m.sh(clear + 2, 0);
    m.sh(clear + 6, 0x1e0);
    call(m, CLEAR_IMAGE, [clear, 0, 0, 0]);
    let block = m.gw(0x4dc);
    let pixels = m.lw(block + 0x58);
    call(m, LOAD_IMAGE, [GP + 0x240, pixels, 0, 0]);
    let block = m.gw(0x4dc);
    let pixels = m.lw(block + 0x5c);
    call(m, LOAD_IMAGE, [GP + 0x248, pixels, 0, 0]);
    let screen = m.gw(0x22c);
    if m.lw(screen + 4) != 0 {
        music_select(m, 0xff, 0, 0);
    }
    call(m, 0x8003_7e50, [0; 4]);
    m.pop_frame(0x20);
}

/// Native targets owned by this module, by original address.
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    let v0 = match target {
        0x8004_596c => {
            session_release_entities(m);
            0
        }
        0x8003_04ec => music_none(),
        0x8002_df40 => sound_channel_volume(m, a0, a1, a2),
        0x8004_e060 => {
            course_targets_reset(m);
            0
        }
        0x8007_6f04 => {
            camera_view_apply(m);
            0
        }
        0x8007_3a30 => {
            camera_mode_restart(m);
            0
        }
        0x8005_bc64 => {
            course_start(m);
            0
        }
        0x8005_be0c => {
            course_end(m);
            0
        }
        0x8005_73e8 => {
            movie_start(m);
            0
        }
        0x8005_7640 => {
            movie_end(m);
            0
        }
        0x8005_ae8c => {
            picture_start(m);
            0
        }
        0x8005_b234 => {
            picture_end(m);
            0
        }
        0x8002_b504 => stream_state(m),
        0x8002_cb58 => stream_entry_negative(m, a0),
        0x8007_9680 => {
            camera_replay_view(m);
            0
        }
        0x8007_96b8 => {
            camera_set_eye(m, a0, a1);
            0
        }
        0x8007_9730 => {
            camera_set_target(m, a0, a1, a2);
            0
        }
        0x8007_eb50 => {
            record_effect_start(m, a0);
            0
        }
        0x8007_eb80 => {
            record_effect_rewind(m, a0);
            0
        }
        0x8007_ebb4 => record_skip(m, a0, a1, a2),
        0x8007_ec54 => {
            replay_skip_frames(m, a0);
            0
        }
        0x8007_f1d4 => record_play(m, a0),
        0x8007_e998 => {
            record_particles(m, a0);
            0
        }
        0x8007_f9cc => {
            spawn_request(m, a0, a1);
            0
        }
        0x8007_f048 => {
            record_spawn(m, a0);
            0
        }
        0x8006_c99c => {
            particle_stop(m, a0);
            0
        }
        0x8006_c6b8 => {
            emitter_set(m, a0, a1, a2);
            0
        }
        0x8006_c740 => {
            emitter_stop(m, a0);
            0
        }
        0x8006_c760 => {
            emitter_start(m, a0, a1, a2, a3);
            0
        }
        0x8006_c9b8 => {
            particles_update(m);
            0
        }
        0x8007_f298 => {
            replay_rewind(m);
            0
        }
        0x8007_f5b4 => replay_frame(m),
        0x8007_f448 => {
            replay_start(m);
            0
        }
        0x8007_f808 => {
            replay_finish(m);
            0
        }
        0x8007_fdb4 => pool_alloc(m, a0, a1, a2),
        0x8007_f9e8 => {
            spawn_object(m, a0, a1);
            0
        }
        0x8007_fb24 => {
            spawn_debris(m, a0, a1);
            0
        }
        0x8007_fca0 => {
            spawn_smoke(m, a0, a1);
            0
        }
        0x8008_0eb4 => {
            spawn_variant(m, a0, a1);
            0
        }
        0x8008_0534 => {
            debris_burst(m, a0, a1);
            0
        }
        0x8008_07a4 => {
            smoke_trail(m, a0, a1, a2);
            0
        }
        0x8008_22a4 => {
            debris_explosion(m, a0, a1, a2);
            0
        }
        0x8002_c928 => {
            let callback = m.lw(m.sp.wrapping_add(0x10));
            stream_queue(m, a0, a1, a2, a3, callback)
        }
        0x8002_cc44 => music_select(m, a0, a1, a2),
        0x8003_04f4 => sound_listener_reset(m),
        0x8003_0750 => sound_stop_all(m),
        0x8002_f6e4 => sound_source_set(m, a0, a1),
        0x8007_98ec => camera_follow_player(m),
        0x8002_bca4 => stream_pending(m),
        0x8002_b9ec => stream_next(m, a0, a1),
        0x8002_bbe0 => stream_release(m, a0),
        0x8003_8e48 => emitters_clear(m),
        0x8003_8e58 => {
            entities_clear_hidden(m);
            0
        }
        0x8009_2ab8 => effect_pools_release(m),
        0x8003_93b8 => sequence_commands(m, a0),
        0x8003_93b0 | 0x8003_9380 => command_skip(a0),
        0x8003_8e98 => command_camera_pose(m, a0),
        0x8003_8f00 => command_camera_lens(m, a0),
        0x8003_9028 => command_entity_pose(m, a0),
        0x8003_90e4 => command_entity_place(m, a0),
        0x8003_916c => command_emitter(m, a0),
        0x8003_9230 => command_effect(m, a0),
        0x8003_9388 => command_overlay(m, a0),
        0x8007_7768 => camera_set_projection(m, a0, a1),
        0x8007_77b0 => {
            let depth = m.lw(m.sp.wrapping_add(0x10));
            camera_set_range(m, a0, a1, a2, a3, depth)
        }
        0x8003_0528 => sequence_sound(m, a0),
        0x8003_9420 => {
            sequence_stream_done(m);
            1
        }
        0x8003_9430 => sequence_start(m),
        0x8003_95d4 => sequence_end(m),
        0x8003_8d1c => sequence_restore(m, a0),
        0x8003_99d4 => {
            sequence_fade(m);
            0
        }
        0x8009_3c90 => tint_rectangle(m, a0, a1, a2),
        0x8009_3d1c => {
            sequence_sparks(m, a0, a1, a2, a3);
            0
        }
        0x8009_3fc0 => {
            sequence_burn(m, a0);
            0
        }
        0x8009_3bc4 => screen_fade(m, a0, a1, a2, a3),
        _ => return None,
    };
    Some(v0)
}
