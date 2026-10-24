//! JP overlay 2, part 5: the routines 800F5834..800FF95C (121 functions,
//! `jpwave/ov2-part5.tsv`), translated from the JP instructions.
//!
//! This file holds the helpers shared by the part's files and the first
//! story-flow substates of state 3 (the callback table 0x801097A8):
//!
//! | Substate | Callback | Rust |
//! | ---: | --- | --- |
//! | 0 | `800F60F0` | [`debrief_initialize`]: hub block, ordering tables, the debriefing set-up |
//! | 1 | `800F6848` | [`debrief_update`]: the debriefing screen (phase table 0x80111710) |
//!
//! The other files of the part hold the primitive builders and the stream
//! block (`ov2_p5_util`), the aircraft and weapon selection of the block
//! at 0x80112E10 (`ov2_p5_net`: drawing and input; `ov2_p5_net2`: its
//! phase routines, table 0x8010ECF8), substates 2..4 and the hub scene
//! (`ov2_p5_lost`), the movie player of the block at 0x80112E18 and
//! substates 5/6 (`ov2_p5_movie`), the disc prompts, the mission item
//! tables and the disc check (`ov2_p5_disc`) and the map routines shared
//! with PAL overlay 2 (`ov2_p5_map`).
//!
//! Every routine is claimed by a `dispatch` gated by
//! `common::loaded_module(m) == 2`; routines of the other parts and the
//! resident are called through [`dispatch::call`] by JP address.
//!
//! Memory helpers take `(base, offset)` like the instructions do and
//! compute the address with wrapping arithmetic, as the CPU does.
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch;
use crate::machine::{Fault, Machine, SmallData};

/// The module number (context +0x38 while it is loaded).
pub const MODULE: u32 = 2;

// ---- globals ----------------------------------------------------------------------

/// `[gp+0x2D4]` (0x800BCE48): the resident sound/stream state block.
pub const SOUND: u32 = 0x800b_ce48;
/// `[0x80112DC0]`: a block whose +0x5C points at the current record.
pub const RECORD: u32 = 0x8011_2dc0;
/// `[0x80112DC4]`: the hub block (0x284 bytes from the arena): +0/+4
/// ordering-table pairs, +8 the current ordering table, +0xC two packet
/// areas, +0x38 two 0x129C-byte areas, +0x80, +0x16C three 32-entry tables,
/// +0x1AC, +0x1C6 the debriefing phase, +0x268 its stream step, +0x274 the
/// packet pointer, +0x278 two 0x8000-byte packet buffers.
pub const HUB: u32 = 0x8011_2dc4;
/// `[0x80112E04]`: the debriefing block (0x3D8 bytes).
pub const DEBRIEF: u32 = 0x8011_2e04;
/// `[0x80112E08]`: context + 0x3B8 (the mission result record).
pub const RESULT: u32 = 0x8011_2e08;

// ---- helpers ----------------------------------------------------------------------

/// A call by JP address (the resident, or any overlay-2 part).
pub(crate) fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}
/// Run `body` inside the original stack frame of `size` bytes (its base is
/// the routine's `sp`), so callee frames and stack arguments sit at the
/// original addresses.
pub(crate) fn framed<R>(
    m: &mut Machine,
    size: u32,
    body: impl FnOnce(&mut Machine, u32) -> R,
) -> R {
    let sp = m.push_frame(size);
    let result = body(m, sp);
    m.pop_frame(size);
    result
}
/// The runtime context (`[gp+0x2EC]`, 0x800BCE60).
pub(crate) fn ctx(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}
/// `[0x80112DC4]`.
pub(crate) fn hub(m: &mut Machine) -> u32 {
    m.lw(HUB)
}
/// The arena allocator (80027410).
pub(crate) fn alloc(m: &mut Machine, size: u32) -> u32 {
    call(m, 0x8002_7410, [size, 0, 0, 0])
}
/// `sra`.
pub(crate) fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}
/// A sign-extended halfword (`sll 16; sra 16`).
pub(crate) fn sx16(value: u32) -> u32 {
    value as u16 as i16 as u32
}
/// `slt`.
pub(crate) fn slt(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `mult a, b; mfhi`.
pub(crate) fn hi_s(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}
/// `multu a, b; mfhi`.
pub(crate) fn hi_u(a: u32, b: u32) -> u32 {
    ((u64::from(a) * u64::from(b)) >> 32) as u32
}
/// `div` (`mflo`, `mfhi`) behind the compiler's checks: `break 7` at
/// `zero_break` for a zero divisor and `break 6` at `overflow_break` for
/// `0x80000000 / -1` (0 when the code has no such check). Reaching a trap
/// records `Fault::Trap` and yields the R3000 result.
pub(crate) fn div(
    m: &mut Machine,
    numerator: u32,
    divisor: u32,
    zero_break: u32,
    overflow_break: u32,
) -> (u32, u32) {
    let (n, d) = (numerator as i32, divisor as i32);
    if d == 0 {
        if zero_break != 0 {
            m.raise(Fault::Trap {
                address: zero_break,
                code: 7,
            });
        }
        return (if n < 0 { 1 } else { u32::MAX }, numerator);
    }
    if n == i32::MIN && d == -1 {
        if overflow_break != 0 {
            m.raise(Fault::Trap {
                address: overflow_break,
                code: 6,
            });
        }
        return (numerator, 0);
    }
    (n.wrapping_div(d) as u32, n.wrapping_rem(d) as u32)
}
/// `lw rt, offset(base)`.
pub(crate) fn lw(m: &mut Machine, base: u32, offset: u32) -> u32 {
    m.lw(base.wrapping_add(offset))
}
/// `lh rt, offset(base)`.
pub(crate) fn lh(m: &mut Machine, base: u32, offset: u32) -> u32 {
    m.lh(base.wrapping_add(offset)) as u32
}
/// `lhu rt, offset(base)`.
pub(crate) fn lhu(m: &mut Machine, base: u32, offset: u32) -> u32 {
    u32::from(m.lhu(base.wrapping_add(offset)))
}
/// `lbu rt, offset(base)`.
pub(crate) fn lbu(m: &mut Machine, base: u32, offset: u32) -> u32 {
    u32::from(m.lbu(base.wrapping_add(offset)))
}
/// `sw rt, offset(base)`.
pub(crate) fn sw(m: &mut Machine, base: u32, offset: u32, value: u32) {
    m.sw(base.wrapping_add(offset), value);
}
/// `sh rt, offset(base)`.
pub(crate) fn sh(m: &mut Machine, base: u32, offset: u32, value: u32) {
    m.sh(base.wrapping_add(offset), value as u16);
}
/// `sb rt, offset(base)`.
pub(crate) fn sb(m: &mut Machine, base: u32, offset: u32, value: u32) {
    m.sb(base.wrapping_add(offset), value as u8);
}
/// Link `prim` into the ordering-table word at `ot` (the libgpu `addPrim`
/// sequence: the packet's link first, then the table word, re-read).
pub(crate) fn add_prim(m: &mut Machine, ot: u32, prim: u32) {
    let link = (m.lw(prim) & 0xff00_0000) | (m.lw(ot) & 0x00ff_ffff);
    m.sw(prim, link);
    let head = (m.lw(ot) & 0xff00_0000) | (prim & 0x00ff_ffff);
    m.sw(ot, head);
}

// ---- the debriefing (800F5834..800F6DA4) ------------------------------------------

/// 800F5834: print the mission time `"%2d:%2d@%2d"` at (0x1E, 0x153):
/// seconds and fields of the timer `[DEBRIEF]+2`, and a hundredths figure
/// (a random 30..59 while the timer runs, else from context +0x3BA).
pub fn print_time(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        const M30: u32 = 0x8888_8889;
        let block = m.lw(DEBRIEF);
        let context = ctx(m);
        let running = lbu(m, block, 0);
        let a1 = lhu(m, context, 0x3ba);
        let s0 = if running == 0 {
            0
        } else {
            let timer = lh(m, block, 2);
            let q = sra(hi_s(a1, M30).wrapping_add(a1), 4).wrapping_sub(sra(a1, 31));
            if slt(timer, q) {
                let r = call(m, 0x800a_dd48, [0; 4]);
                let q = sra(hi_s(r, M30).wrapping_add(r), 4).wrapping_sub(sra(r, 31));
                let rem = r.wrapping_sub((q << 4).wrapping_sub(q) << 1);
                rem.wrapping_add(0x1e)
            } else {
                let rem = a1.wrapping_sub((q << 4).wrapping_sub(q) << 1);
                let v = ((rem << 1).wrapping_add(rem) << 3).wrapping_add(rem) << 2;
                sra(hi_s(v, M30).wrapping_add(v), 4).wrapping_sub(sra(v, 31))
            }
        };
        call(m, 0x800f_33a8, [0x1e, 0x153, 0, 0]);
        let block = m.lw(DEBRIEF);
        let t = lhu(m, block, 2) << 16;
        let a2 = sra(t, 16);
        let a3 = sra(hi_s(a2, M30).wrapping_add(a2), 5).wrapping_sub(sra(t, 31));
        let t = a3 << 16;
        let a1 = sra(t, 16);
        let q = sra(hi_s(a1, M30).wrapping_add(a1), 5).wrapping_sub(sra(t, 31));
        let a1 = sx16(a1.wrapping_sub((q << 4).wrapping_sub(q) << 2));
        let a2 = sx16(a2.wrapping_sub((a3 << 4).wrapping_sub(a3) << 2));
        call(m, 0x800f_4b40, [0x800c_d154, a1, a2, s0 & 0xff])
    })
}

/// 800F59B4: advance the debriefing step timer `[DEBRIEF]+0x3D4` against
/// the step lengths at 0x8010E634; at the end of step 0 go to step 1, at
/// the end of a later step set +0x3D2. Returns 1 while the step runs, else
/// the block pointer.
pub fn step_timer(m: &mut Machine) -> u32 {
    let block = m.lw(DEBRIEF);
    let t = lhu(m, block, 0x3d4).wrapping_add(1);
    let step = lbu(m, block, 0);
    sh(m, block, 0x3d4, t);
    let length = lh(m, 0x8010_e634, step << 1);
    if slt(sx16(t), length) {
        return 1;
    }
    if step == 0 {
        sb(m, block, 0, step.wrapping_add(1));
    } else {
        sb(m, block, 0x3d2, 1);
    }
    let block = m.lw(DEBRIEF);
    sh(m, block, 0x3d4, 0);
    block
}

/// 800F5A24: while the debriefing runs, count the timer `[DEBRIEF]+2` up to
/// context +0x3BA / 30. Returns the new timer, or 0 when it did not move.
pub fn count_time(m: &mut Machine) -> u32 {
    let block = m.lw(DEBRIEF);
    if lbu(m, block, 0) == 0 {
        return 0;
    }
    let context = ctx(m);
    let total = lhu(m, context, 0x3ba);
    let limit = (hi_u(total, 0x8888_8889) >> 4) & 0xffff;
    let timer = lh(m, block, 2);
    let next = lhu(m, block, 2).wrapping_add(1);
    if slt(timer, limit) {
        sh(m, block, 2, next);
        return next;
    }
    0
}

/// 800F5A88 (JP; PAL 800CA154 in overlay 5, changed): print
/// `"RMODE : %d"` with the debriefing step at (10, 0x14A).
pub fn print_mode(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        call(m, 0x800f_33a8, [0xa, 0x14a, 0, 0]);
        let block = m.lw(DEBRIEF);
        let step = lbu(m, block, 0);
        call(m, 0x800f_4ae0, [0x800c_d160, step, 0, 0])
    })
}

/// 800F5AC4: point [`RESULT`] at context +0x3B8 and clear the 64 flags at
/// `[DEBRIEF]+0x13 + 4n`. Returns 0.
pub fn clear_flags(m: &mut Machine) -> u32 {
    let context = ctx(m);
    m.sw(RESULT, context.wrapping_add(0x3b8));
    for i in 0..0x40u32 {
        let block = m.lw(DEBRIEF);
        sb(m, block.wrapping_add(i << 2), 0x13, 0);
    }
    0
}

/// 800F5B04: whether mission `a0` unlocks something in the result record
/// (context +0x95C): four record groups per checked mission (0x22, 0x27,
/// 0x12, 0x35, 0x30). Returns 0 or 1.
pub fn result_unlocks(m: &mut Machine, a0: u32) -> u32 {
    let context = ctx(m);
    let a1 = context.wrapping_add(0x95c);
    if lh(m, context, 0x954) != 1 {
        return 0;
    }
    let any = |m: &mut Machine, offsets: [u32; 4]| offsets.into_iter().any(|o| lbu(m, a1, o) != 0);
    if !any(m, [0x2e2, 0x31e, 0x35a, 0x396]) && a0 != 0x22 {
        return 0;
    }
    if !any(m, [0x2e7, 0x323, 0x35f, 0x39b]) && a0 != 0x27 {
        return 0;
    }
    if !any(m, [0x2d2, 0x30e, 0x34a, 0x386]) && a0 != 0x12 {
        return 0;
    }
    if !any(m, [0x2f5, 0x331, 0x36d, 0x3a9]) && a0 != 0x35 {
        return 0;
    }
    if any(m, [0x2f0, 0x32c, 0x368, 0x3a4]) {
        return 1;
    }
    u32::from(a0 == 0x30)
}

/// 800F5CA4: record the finished mission's result: the best times and
/// ranks per difficulty (context +0x95C tables), the totals at context
/// +0xE4C/+0xF3C/+0x1030/+0x1034, and the rank in `[[RECORD]+0x5C]+1`.
/// Returns the new total at context +0x1034.
pub fn record_result(m: &mut Machine) -> u32 {
    framed(m, 0x30, |m, _| {
        let context = ctx(m);
        let s2 = context.wrapping_add(0x95c);
        let s4 = lbu(m, context, 0x3bc);
        let s3 = lw(m, context, 0x394);
        let s1 = lbu(m, context, 0x3c3);
        let a1 = lh(m, context, 0x3ba);
        let a0 = lhu(m, context, 0x3ba);
        let s5 = context.wrapping_add(0x3b8);
        if a1 != 0 {
            let v1 = s2.wrapping_add(s4 << 1);
            sh(m, v1, 0x248, a0);
            if s3 == 0 && slt(a1, lhu(m, v1, 0x68)) {
                sh(m, v1, 0x68, a0);
            }
            for (difficulty, offset) in [(1u32, 0xe0u32), (2, 0x158), (3, 0x1d0)] {
                if s3 == difficulty {
                    let at = s2.wrapping_add((s4 & 0xff) << 1);
                    if slt(sx16(a0), lhu(m, at, offset)) {
                        sh(m, at, offset, a0);
                    }
                }
            }
            let total = lw(m, s2, 0x6d0).wrapping_add(sx16(a0));
            sw(m, s2, 0x6d0, total);
        }
        let context = ctx(m);
        let mission = lh(m, context, 0x38c);
        call(m, 0x800f_e950, [mission, 0, 0, 0]);
        let context = ctx(m);
        let mission = lh(m, context, 0x38c);
        call(m, 0x800f_ec84, [mission, 0, 0, 0]);
        let v1 = s2.wrapping_add(s4 & 0xff);
        sb(m, v1, 0x3b0, s1);
        for (difficulty, offset) in [(0u32, 0x2c0u32), (1, 0x2fc), (2, 0x338), (3, 0x374)] {
            if s3 == difficulty {
                let at = s2.wrapping_add(s4 & 0xff);
                let rank = lbu(m, at, offset);
                if rank == 0 || (s1 & 0xff) < rank {
                    sb(m, at, offset, s1);
                }
            }
        }
        // Jump table 0x800CD16C: rank 0..5 stores that rank.
        let rank = s1 & 0xff;
        if rank < 6 {
            let record = m.lw(RECORD);
            let current = lw(m, record, 0x5c);
            sb(m, current, 1, rank);
        }
        let record = m.lw(RECORD);
        let current = lw(m, record, 0x5c);
        sb(m, current, 1, s1);
        let a0 = ctx(m);
        let kills = lbu(m, s5, 0xa);
        let v1 = a0.wrapping_add((s4 & 0xff) << 2);
        sw(m, v1, 0xe4c, kills);
        let kills = lbu(m, s5, 0xa);
        let sum = lw(m, a0, 0x1030).wrapping_add(kills);
        let a1 = lh(m, a0, 0x958);
        sw(m, a0, 0x1030, sum);
        let sum = lw(m, v1, 0xf3c).wrapping_add(a1);
        sw(m, v1, 0xf3c, sum);
        let v1 = lh(m, a0, 0x958);
        let sum = lw(m, a0, 0x1034).wrapping_add(v1);
        sw(m, a0, 0x1034, sum);
        sum
    })
}

/// 800F5FF8: clear the 8 entries (0x2C bytes) of the two lists at
/// `[DEBRIEF]+0x110` and `+0x270`, and the bytes +0x3D0/+0x3D1. Returns the
/// block pointer.
pub fn clear_lists(m: &mut Machine) -> u32 {
    for base in [0x110u32, 0x270] {
        let mut v1 = 0u32;
        for _ in 0..8 {
            for offset in [base + 0x29, base + 0x28, base] {
                let block = m.lw(DEBRIEF);
                sb(m, block.wrapping_add(v1), offset, 0);
            }
            v1 = v1.wrapping_add(0x2c);
        }
    }
    let block = m.lw(DEBRIEF);
    sb(m, block, 0x3d0, 0);
    let block = m.lw(DEBRIEF);
    sb(m, block, 0x3d1, 0);
    block
}

/// 800F60A4: clear the result flags at context +0xD98..+0xE4B and
/// +0xD48..+0xD97 (each run from its top down). Returns context + 0x95B.
pub fn clear_records(m: &mut Machine) -> u32 {
    let context = ctx(m);
    let a0 = context.wrapping_add(0x95c);
    let mut v0 = context.wrapping_add(0xa0f);
    for _ in 0..0xb4 {
        sb(m, v0, 0x43c, 0);
        v0 = v0.wrapping_sub(1);
    }
    let mut v1 = 0x4fu32;
    let mut v0 = a0.wrapping_add(v1);
    loop {
        sb(m, v0, 0x3ec, 0);
        v1 = v1.wrapping_sub(1);
        v0 = v0.wrapping_sub(1);
        if (v1 as i32) < 0 {
            break;
        }
    }
    v0
}

/// 800F60F0, state 3 substate 0: allocate the hub block and its buffers,
/// map the mission codes, start the subsystems and the stream, and, after a
/// finished mission (context +0x954 == 1), set up the debriefing. Returns
/// NEXT_SUBSTATE (0x04000000).
pub fn debrief_initialize(m: &mut Machine) -> u32 {
    framed(m, 0x40, |m, sp| {
        let sound = m.lw(SOUND);
        sb(m, sound, 0x44, 1);
        let sound = m.lw(SOUND);
        let flags = lw(m, sound, 0) | 9;
        sw(m, sound, 4, 1);
        sw(m, sound, 0x14, 0);
        sw(m, sound, 0, flags);
        call(m, 0x8002_c6a8, [0, 0, 0, 0]);
        for offset in [0x10, 0x11, 0x12] {
            let sound = m.lw(SOUND);
            sb(m, sound, offset, 0);
        }
        call(m, 0x8002_cbcc, [0; 4]);
        let block = alloc(m, 0x284);
        m.sw(HUB, block);
        for i in 0..2u32 {
            let buffer = alloc(m, 0x8000);
            let hub = hub(m);
            sw(m, hub.wrapping_add(i << 2), 0x278, buffer);
        }
        // the JP mission codes 0x36/0x3A and 0x38/0x39 map to 0x22/0x27
        // and 0x2A/0x25
        let context = ctx(m);
        let mut code = lbu(m, context, 0x3bc);
        if code == 0x36 {
            sb(m, context, 0x3bc, 0x22);
            let context = ctx(m);
            code = lbu(m, context, 0x3bc);
        }
        if code == 0x3a {
            let context = ctx(m);
            sb(m, context, 0x3bc, 0x27);
        }
        let context = ctx(m);
        let mut code = lbu(m, context, 0x3bd);
        if code == 0x38 {
            sb(m, context, 0x3bd, 0x2a);
            let context = ctx(m);
            code = lbu(m, context, 0x3bd);
        }
        if code == 0x39 {
            let context = ctx(m);
            sb(m, context, 0x3bd, 0x25);
        }
        let context = ctx(m);
        if lbu(m, context, 0x957) == 1 {
            let mission = lh(m, context, 0x38c);
            let mapped = lbu(m, mission, 0x8010_e638);
            sh(m, context, 0x954, 0);
            sh(m, context, 0x38c, mapped);
        }
        let context = ctx(m);
        let mission = lh(m, context, 0x38c);
        let v0 = call(m, 0x800f_b2a0, [mission, 0, 0, 0]);
        let hub = self::hub(m);
        sw(m, hub, 0x80, v0);
        for s2 in 0..3u32 {
            let table = alloc(m, 0x180);
            let hub = self::hub(m);
            sw(m, hub.wrapping_add(s2 << 2), 0x16c, table);
            for s1 in 0..0x20u32 {
                let hub = self::hub(m);
                let slot = hub.wrapping_add(s2 << 2);
                let table = lw(m, slot, 0x16c);
                sh(m, table, s1 * 12, 0);
                let table = lw(m, slot, 0x16c);
                sh(m, table.wrapping_add(s1 * 12), 2, 0);
                call(m, 0x800d_eca8, [s2, s1, 0, 0]);
            }
        }
        let v0 = alloc(m, 0x10);
        let hub = self::hub(m);
        sw(m, hub, 0x1ac, v0);
        sh(m, v0, 0xa, 0xffff);
        for s1 in 0..2u32 {
            let v0 = alloc(m, 0x40);
            let hub = self::hub(m);
            sw(m, hub, s1 << 2, v0);
            let v0 = alloc(m, 0x400);
            let hub = self::hub(m);
            sw(m, hub.wrapping_add(s1 << 2), 0xc, v0);
        }
        for s1 in 0..2u32 {
            let v0 = alloc(m, 0x129c);
            let hub = self::hub(m);
            sw(m, hub.wrapping_add(s1 << 2), 0x38, v0);
        }
        call(m, 0x800d_e924, [0; 4]);
        let v0 = call(m, 0x8002_7338, [0x8010_bbf0, 0, 0, 0]);
        call(m, 0x800d_e908, [v0, 1, 2, 0]);
        call(m, 0x800d_edf0, [0, 0, 0, 0]);
        call(m, 0x800d_edf0, [1, 0, 0, 0]);
        call(m, 0x800d_edf0, [2, 1, 0, 0]);
        call(m, 0x800f_3c14, [0; 4]);
        let v0 = alloc(m, 0x2bc);
        m.set_gw(crate::common::GAMEPLAY, v0);
        call(m, 0x800e_37f8, [0; 4]);
        call(m, 0x800f_74c8, [0; 4]);
        call(m, 0x800f_7514, [0x8010_e854, 0, 0, 0]);
        let hub = self::hub(m);
        sh(m, hub, 0x1c6, 0);
        call(m, 0x8002_da94, [0; 4]);
        let a0 = ctx(m);
        if lw(m, a0, 0x96c) & 0x10 != 0 {
            let hub = self::hub(m);
            sw(m, hub, 0xc4, 0);
            if lh(m, a0, 0x954) == 1 {
                sh(m, a0, 0x954, 3);
            }
            let v1 = ctx(m);
            if lw(m, v1, 0x96c) & 0x10 != 0 {
                sh(m, v1, 0x38c, 0x3b);
            }
        }
        let a0 = ctx(m);
        if lh(m, a0, 0x954) == 1 {
            let case = lbu(m, a0, 0x3bc).wrapping_sub(0x12);
            // Jump table 0x800CD194: codes 0x12, 0x22, 0x27, 0x30 and 0x35
            // go to 800F64A0, the others to 800F6514.
            if case < 0x24 && matches!(case, 0 | 16 | 21 | 30 | 35) {
                let v1 = ctx(m);
                let flags = lw(m, v1, 0x96c);
                let hub = self::hub(m);
                sw(m, v1, 0x96c, flags | 8);
                sw(m, hub, 0xe0, 2);
                let code = lbu(m, v1, 0x3bc);
                if result_unlocks(m, code) != 0 {
                    let a0 = ctx(m);
                    let v1 = lw(m, a0, 0x96c);
                    if v1 & 0x20 != 0 {
                        let hub = self::hub(m);
                        sw(m, a0, 0x96c, v1 & !0x20);
                        sw(m, hub, 0xe0, 3);
                    } else {
                        sw(m, a0, 0x96c, v1 | 0x20);
                    }
                }
            } else {
                let a0 = ctx(m);
                let flags = lw(m, a0, 0x96c) & !8;
                sw(m, a0, 0x96c, flags);
            }
            let context = ctx(m);
            if lw(m, context, 0x96c) & 8 != 0 {
                record_result(m);
                return 0x0400_0000;
            }
        }
        let a0 = ctx(m);
        if lbu(m, a0, 0x95f) == 1 || lw(m, a0, 0x96c) & 1 != 0 || lbu(m, a0, 0x957) == 1 {
            return 0x0400_0000;
        }
        match lh(m, a0, 0x954) {
            1 => {
                let flags = lw(m, a0, 0x96c);
                let code = lbu(m, a0, 0x3bd);
                sh(m, a0, 0x38c, code);
                if flags & 0x10 != 0 {
                    sh(m, a0, 0x38c, 0x3b);
                }
            }
            0 | 2 => {
                let v1 = ctx(m);
                if lw(m, v1, 0x96c) & 0x10 != 0 {
                    sh(m, v1, 0x38c, 0x3b);
                }
                return 0x0400_0000;
            }
            3 => sh(m, a0, 0x38c, 0x3b),
            _ => {}
        }
        record_result(m);
        let v0 = alloc(m, 0x3d8);
        m.sw(DEBRIEF, v0);
        sb(m, v0, 0, 0);
        let v0 = m.lw(DEBRIEF);
        sb(m, v0, 1, 0);
        let a0 = m.lw(DEBRIEF);
        sh(m, v0, 2, 0);
        sb(m, a0, 0, 0);
        let a0 = m.lw(DEBRIEF);
        let hub = self::hub(m);
        sh(m, a0, 0x3d4, 0);
        sw(m, hub, 0x268, 0);
        for s1 in 0..2u32 {
            let v0 = alloc(m, 0x1000);
            let block = m.lw(DEBRIEF);
            sw(m, block.wrapping_add(s1 << 2), 4, v0);
        }
        clear_flags(m);
        clear_lists(m);
        call(m, 0x800f_35b8, [7, 0, 0, 0]);
        call(m, 0x800f_33f8, [1, 0, 0, 0]);
        call(m, 0x800f_34c8, [1, 1, 1, 0]);
        call(m, 0x800f_34b0, [1, 0, 0, 0]);
        // the two display rectangles at 0x800CD184 and 0x800CD18C
        for (at, source) in [(0x10u32, 0x800c_d184u32), (0x18, 0x800c_d18c)] {
            let (w0, w1) = (m.lw(source), m.lw(source + 4));
            sw(m, sp, at, w0);
            sw(m, sp, at + 4, w1);
        }
        call(m, 0x800a_8500, [sp + 0x10, 0x3c0, 0x100, 0]);
        call(m, 0x800a_8194, [0, 0, 0, 0]);
        call(m, 0x800a_8500, [sp + 0x18, 0x3c0, 0x180, 0]);
        call(m, 0x800a_8194, [0, 0, 0, 0]);
        0x0400_0000
    })
}

/// 800F6774 (JP; PAL 800D233C, near): set the drawing area `a1` (0: the
/// whole 320x480 frame, 1: 0x14,0x46 128x240; else the frame's stale
/// rectangle) into the DR_AREA packet `a0` (SetDrawArea) and link it into
/// the hub's ordering table. Returns the next packet (`a0 + 0xC`).
pub fn draw_area(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        let packet = a0;
        if a1 == 0 {
            sh(m, sp, 0x14, 0x140);
            sh(m, sp, 0x10, 0);
            sh(m, sp, 0x12, 0);
            sh(m, sp, 0x16, 0x1e0);
        } else if a1 == 1 {
            sh(m, sp, 0x10, 0x14);
            sh(m, sp, 0x12, 0x46);
            sh(m, sp, 0x14, 0x80);
            sh(m, sp, 0x16, 0xf0);
        }
        call(m, 0x800a_8f20, [packet, sp + 0x10, 0, 0]);
        let hub = hub(m);
        let ot = lw(m, hub, 8);
        add_prim(m, ot, packet);
        packet.wrapping_add(0xc)
    })
}

/// 800F6848, state 3 substate 1: the debriefing screen. Clears the
/// ordering table, links the context's display list, runs the phase routine
/// `[0x80111710 + 4 * [HUB]+0x1C6]` and the stream steps, draws the result
/// pages and the titles "DEBRIEFING" and "PERFORMANCE RATING". Returns
/// NEXT_SUBSTATE when it is over (or not wanted), else 0.
pub fn debrief_update(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        let context = ctx(m);
        let buffer = lw(m, context, 4);
        let hub = hub(m);
        let ot = lw(m, hub, buffer << 2);
        sw(m, hub, 8, ot);
        call(m, 0x800a_8680, [ot, 0x10, 0, 0]);
        let a0 = self::hub(m);
        let t0 = ctx(m);
        let a2 = lw(m, a0, 8);
        let v0 = lw(m, t0, 8);
        let v1 = lw(m, a2, 0);
        let v0 = lw(m, v0, 0);
        sw(m, a2, 0, (v1 & 0xff00_0000) | (v0 & 0x00ff_ffff));
        let v0 = lw(m, a0, 8);
        let a2 = lw(m, t0, 8);
        let v1 = lw(m, a2, 0);
        let link = (v1 & 0xff00_0000) | (v0.wrapping_add(0x3c) & 0x00ff_ffff);
        sw(m, a2, 0, link);
        let buffer = lw(m, t0, 4);
        let packets = lw(m, a0.wrapping_add(buffer << 2), 0x278);
        sw(m, a0, 0x274, packets);
        let flags = lw(m, t0, 0x96c);
        if flags & 1 != 0 || flags & 8 != 0 || lbu(m, t0, 0x95f) == 1 {
            return 0x0400_0000;
        }
        let state = lh(m, t0, 0x954);
        if state != 1 && state != 3 {
            return 0x0400_0000;
        }
        let context = ctx(m);
        if lbu(m, context, 0x957) == 1 {
            return 0x0400_0000;
        }
        call(m, 0x800f_33d0, [5, 0x32, 0x35, 0]);
        let hub = self::hub(m);
        let ot = lw(m, hub, 8);
        call(m, 0x800f_3db4, [ot, 0, 0, 0]);
        let hub = self::hub(m);
        let phase = lh(m, hub, 0x1c6);
        let routine = lw(m, phase << 2, 0x8011_1710);
        let v0 = call(m, routine, [0; 4]);
        const STREAMS: u32 = 0x8010_e674;
        let entry = |m: &mut Machine| {
            let context = ctx(m);
            let mission = lh(m, context, 0x38c);
            let column = lbu(m, context, 0x956);
            ((mission << 2).wrapping_add(column) << 1).wrapping_add(STREAMS)
        };
        let at = entry(m);
        let a0 = lbu(m, at, 0);
        if v0.wrapping_sub(1) < 2 {
            let hub = self::hub(m);
            let phase = lhu(m, hub, 0x1c6).wrapping_add(1);
            sh(m, hub, 0x1c6, phase);
            if sx16(phase) == 7 && a0 != 0 {
                let at = entry(m);
                let resource = lbu(m, at, 0);
                call(m, 0x800f_7524, [1, resource, 0, 0]);
                let hub = self::hub(m);
                sw(m, hub, 0x268, 1);
                let at = entry(m);
                if lbu(m, at, 1) == 0 {
                    let list = lw(m, hub, 0x20);
                    let a0 = lw(m, list, 0x10);
                    call(m, 0x800e_cec8, [a0, 0, 0, 0]);
                }
                let at = entry(m);
                if lbu(m, at, 1) == 1 {
                    let hub = self::hub(m);
                    let list = lw(m, hub, 0x20);
                    let a0 = lw(m, list, 0x14);
                    call(m, 0x800e_cec8, [a0, 0, 0, 0]);
                }
            }
        }
        let hub = self::hub(m);
        match lw(m, hub, 0x268) {
            1 => {
                if call(m, 0x8002_b59c, [0; 4]) != 0 {
                    call(m, 0x8002_cc44, [3, 0, 0, 0]);
                    let hub = self::hub(m);
                    sw(m, hub, 0x268, 2);
                }
            }
            2 => {
                let ot = lw(m, hub, 8);
                let packets = lw(m, hub, 0x274);
                let v0 = call(m, 0x8002_d3f4, [ot, packets, 0, 0]);
                let hub = self::hub(m);
                sw(m, hub, 0x274, v0);
            }
            _ => {}
        }
        let hub = self::hub(m);
        let packets = lw(m, hub, 0x274);
        let v0 = call(m, 0x800e_2494, [packets, 0, 0, 0]);
        let hub = self::hub(m);
        sw(m, hub, 0x274, v0);
        call(m, 0x800f_5018, [0; 4]);
        call(m, 0x800f_56d8, [0; 4]);
        print_time(m);
        let hub = self::hub(m);
        let packets = lw(m, hub, 0x274);
        let mut a0 = draw_area(m, packets, 0);
        for routine in [0x800f_5388u32, 0x800f_4df0, 0x800f_4c04] {
            let hub = self::hub(m);
            sw(m, hub, 0x274, a0);
            a0 = call(m, routine, [a0, 0, 0, 0]);
        }
        let hub = self::hub(m);
        sw(m, hub, 0x274, a0);
        let v0 = draw_area(m, a0, 1);
        let hub = self::hub(m);
        sw(m, hub, 0x274, v0);
        call(m, 0x800f_33a8, [0x14, 0x29, 0, 0]);
        call(m, 0x800f_4ae0, [0x800c_d224, 0, 0, 0]);
        call(m, 0x800f_33a8, [0x8b, 0x157, 0, 0]);
        call(m, 0x800f_4ae0, [0x800c_d230, 0, 0, 0]);
        step_timer(m);
        let v1 = ctx(m);
        if lw(m, v1, 0x12c) != 0 && lw(m, v1, 0x128) & 0xf == 0 {
            let hub = self::hub(m);
            if lh(m, hub, 0x1c6) == 7 {
                call(m, 0x800d_edf0, [2, 0, 0, 0]);
                call(m, 0x800d_ee08, [1, 0, 0, 0]);
                call(m, 0x8002_c6a8, [0, 0, 0, 0]);
                return 0x0400_0000;
            }
        }
        count_time(m);
        0
    })
}

/// 800F6CBC: set up the DR_MODE packet `a1` from the 8-byte mode record
/// `a0` (+0 dither, +1 draw-to-display, +2 semi-transparency, +3 colour
/// mode, +4 x, +6 y: the tpage computed inline) with SetDrawMode, and link
/// it into the ordering-table word `a2`. Returns the next packet (`a1 +
/// 0xC`).
pub fn draw_mode(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    framed(m, 0x28, |m, sp| {
        let packet = a1;
        let dither = lbu(m, a0, 0);
        let display = lbu(m, a0, 1);
        let abr = lbu(m, a0, 2);
        let tp = lbu(m, a0, 3);
        let y = lhu(m, a0, 6);
        let x = lhu(m, a0, 4);
        sw(m, sp, 0x10, 0);
        let mut tpage = ((abr & 3) << 7) | ((tp & 3) << 5);
        tpage |= sx16(sra((y & 0x100) << 16, 20));
        tpage |= (x & 0x3ff) >> 6;
        tpage |= sx16(y & 0x200) << 2;
        call(m, 0x800a_c7f8, [packet, dither, display, tpage]);
        add_prim(m, a2, packet);
        packet.wrapping_add(0xc)
    })
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x800f_5834 => print_time(m),
        0x800f_59b4 => step_timer(m),
        0x800f_5a24 => count_time(m),
        0x800f_5a88 => print_mode(m),
        0x800f_5ac4 => clear_flags(m),
        0x800f_5b04 => result_unlocks(m, a0),
        0x800f_5ca4 => record_result(m),
        0x800f_5ff8 => clear_lists(m),
        0x800f_60a4 => clear_records(m),
        0x800f_60f0 => debrief_initialize(m),
        0x800f_6774 => draw_area(m, a0, a1),
        0x800f_6848 => debrief_update(m),
        0x800f_6cbc => draw_mode(m, a0, a1, a2),
        _ => return None,
    })
}
