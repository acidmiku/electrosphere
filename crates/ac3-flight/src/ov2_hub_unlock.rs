//! Inner state 12, the aircraft unlock screen after a mission (state 11,
//! the post-mission load, leads here; with option bit 8 it continues to
//! the save screen, state 9): initialization 800DC7B0, the per-frame tick
//! 800DC868 with its three phases at 0x800F604C (800DC470 collect the new
//! aircraft and load the first one's picture, 800DC5BC fade the page in
//! for ten frames, 800DC638 browse with next/previous and leave with
//! confirm/cancel), and the unlock rules 800DB980..800DC2D4.
//!
//! State `[0x800F6048]` (0x800F96F0): +0 phase, +4 flags (1 no mission
//! data, set from option bit 0x10; 2 picture sliding out; 4 picture
//! sliding in), +8 fade-in timer, +0xC slide timer, +0x10 count and +0x12
//! selection (halfwords), +0x14 up to 8 unlocked aircraft table indices,
//! +0x1C the picture RECT (x 0x3C0, y 0x100, 0x20 x 0x90), +0x24/+0x28 the
//! selected aircraft's two text lines.
//!
//! The aircraft table at 0x800F53A0 (21 entries of 0x30 bytes): +0 the
//! aircraft id (signed halfword, the index of the progress flags), +0x24
//! the mission whose completion unlocks it, +0x25 1 = no second-flag
//! update, +0x26 the score category (mission record +3), +0x27 the score
//! the category needs, +0x28/+0x2C the two text lines.
//!
//! Progress (context +0x960, `P`): +0x18+id aircraft owned, +0x58+id
//! aircraft also owned in the second set (missions without data, bit 1 of
//! state +4 clear), +0x2B/+0x2C/+0x2D the three special unlocks (aircraft
//! 0x13/0x14/0x15), +0x300+n/+0x33C+n/+0x378+n the per-mission ranks of the
//! three passes, +0x3F0+n the rank of a mission without data, +0xA3 every
//! mission ranked 1 in the third pass, +0xA5 mission 0x12 completed.
use super::{
    CLEAR_TEXT, CONTEXT, MISSION, MISSION_RECORDS, NEXT_PRIMITIVE, SELECT_PAGE, SET_OFFSET,
    SET_PARAMETER, SET_TEXT, call,
};
use crate::machine::{Machine, SmallData};

/// `[0x800F6048]`: the unlock screen state.
pub const STATE: u32 = 0x800f_6048;
/// 800F604C: the three phase callbacks.
pub const PHASES: u32 = 0x800f_604c;
/// 800F53A0: 21 aircraft unlock entries of 0x30 bytes.
pub const AIRCRAFT: u32 = 0x800f_53a0;
const AIRCRAFT_COUNT: u32 = 21;
/// 800CE978: load an aircraft resource (`ov2air`; `bank`, `index`, `vram`,
/// `rect`, with a mode word at `sp+0x10`).
pub const LOAD_RESOURCE: u32 = 0x800c_e978;
/// 800DC074: grant an aircraft's loadout (`ov2core`).
pub const GRANT_LOADOUT: u32 = 0x800d_c074;
/// 800CEB90: bind the common scripts (`ov2core`).
pub const BIND_SCRIPTS: u32 = 0x800c_eb90;
/// 800D04C8: draw a string (`ov2core`).
pub const DRAW_STRING: u32 = 0x800d_04c8;

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}

fn progress(m: &mut Machine) -> u32 {
    m.gw(CONTEXT) + 0x960
}

fn entry(index: u32) -> u32 {
    AIRCRAFT.wrapping_add(index.wrapping_mul(0x30))
}

fn sext16(value: u16) -> u32 {
    i32::from(value as i16) as u32
}

/// Append table index `index` to the unlock list (`[S + 0x14 + count]`,
/// count + 1) and return the new count (the routines' tail).
fn append(m: &mut Machine, index: u32) -> u32 {
    let s = state(m);
    let count = sext16(m.lhu(s + 0x10));
    m.sb(s.wrapping_add(count) + 0x14, index as u8);
    let s = state(m);
    let count = u32::from(m.lhu(s + 0x10)) + 1;
    m.sh(s + 0x10, count as u16);
    count
}

/// 800D17AC: the mission number (+2) of mission record `index`.
pub fn number_d17ac(m: &mut Machine, index: u32) -> u32 {
    let record = MISSION_RECORDS.wrapping_add(index.wrapping_mul(12));
    u32::from(m.lbu(record.wrapping_add(2)))
}

/// Whether any of the three pass ranks of mission `n` is 1.
fn ranked_first(m: &mut Machine, p: u32, n: u32, passes: &[u32]) -> bool {
    passes
        .iter()
        .any(|&pass| m.lbu(p.wrapping_add(n) + pass) == 1)
}

/// 800DB980: unless the state has no mission data, unlock every aircraft
/// whose entry +0x24 is `mission`. Returns 1 (no data) or 0.
pub fn by_mission_db980(m: &mut Machine, mission: u32) -> u32 {
    let s = state(m);
    if m.lw(s + 4) & 1 != 0 {
        return 1;
    }
    for index in 0..AIRCRAFT_COUNT {
        if u32::from(m.lbu(entry(index) + 0x24)) == mission {
            append(m, index);
        }
    }
    0
}

/// 800DBA04: the score of the category of mission number `number` (the
/// first of the 40 records with +2 = `number`, else record 0): every
/// distinct mission number of the category scores 3/2/1 for rank 1/2/3
/// (with mission data, the best nonzero rank of the three passes; without,
/// the +0x3F0 rank). Then unlock every aircraft of the category whose
/// needed score is reached and that is not owned yet. A 37-word array at
/// `sp` marks the counted mission numbers. Returns 0.
pub fn by_score_dba04(m: &mut Machine, number: u32) -> u32 {
    let frame = m.push_frame(0x98);
    let mut score = 0u32;
    let mut found = 0u32;
    let p = progress(m);
    for k in (0..=0x24u32).rev() {
        m.sw(frame + (k << 2), 0);
    }
    for index in 0..40u32 {
        let record = MISSION_RECORDS + index * 12;
        if u32::from(m.lbu(record + 2)) == number {
            found = index;
            break;
        }
    }
    let category = m.lbu(MISSION_RECORDS.wrapping_add(found.wrapping_mul(12)) + 3);
    let s = state(m);
    let mut record = MISSION_RECORDS;
    while record < MISSION_RECORDS + 0x1e0 {
        if m.lbu(record + 3) == category {
            let n = u32::from(m.lbu(record + 2));
            let mark = frame.wrapping_add(n << 2);
            if m.lw(mark) != 1 {
                m.sw(mark, 1);
                let rank = if m.lw(s + 4) & 1 != 0 {
                    let n = u32::from(m.lbu(record + 2));
                    let second = u32::from(m.lbu(p.wrapping_add(n) + 0x33c));
                    let mut rank = u32::from(m.lbu(p.wrapping_add(n) + 0x300));
                    if second != 0 && (second as i32) < (rank as i32) {
                        rank = second;
                    }
                    let n = u32::from(m.lbu(record + 2));
                    let third = u32::from(m.lbu(p.wrapping_add(n) + 0x378));
                    if third != 0 && (third as i32) < (rank as i32) {
                        rank = third;
                    }
                    rank
                } else {
                    let n = u32::from(m.lbu(record + 2));
                    u32::from(m.lbu(p.wrapping_add(n) + 0x3f0))
                };
                score += match rank {
                    1 => 3,
                    2 => 2,
                    3 => 1,
                    _ => 0,
                };
            }
        }
        record += 12;
    }
    for index in 0..AIRCRAFT_COUNT {
        let at = entry(index);
        if m.lbu(at + 0x26) != category {
            continue;
        }
        if (score as i32) < i32::from(m.lbu(at + 0x27)) {
            continue;
        }
        let s = state(m);
        let id = sext16(m.lhu(at));
        let owned = if m.lw(s + 4) & 1 != 0 {
            m.lbu(p.wrapping_add(id) + 0x18)
        } else {
            m.lbu(p.wrapping_add(id) + 0x58)
        };
        if owned != 1 {
            append(m, index);
        }
    }
    m.pop_frame(0x98);
    0
}

/// The tail of 800DBC54/800DBD1C/800DBE2C: append the first table entry
/// whose aircraft id is `id` (when `gated`, only with option bits 0x18).
/// Returns the new count, 0 when gated out or not found.
fn unlock_aircraft(m: &mut Machine, id: u32, gated: bool) -> u32 {
    for index in 0..AIRCRAFT_COUNT {
        if sext16(m.lhu(entry(index))) == id {
            if gated {
                let ctx = m.gw(CONTEXT);
                if m.lw(ctx + 0x970) & 0x18 == 0 {
                    return 0;
                }
            }
            return append(m, index);
        }
    }
    0
}

/// 800DBC54: after mission number 0x24 ranked 1 in any pass, unlock
/// aircraft 0x13 once (flag +0x2B). Returns the routine's v0.
pub fn final_mission_dbc54(m: &mut Machine, number: u32) -> u32 {
    let p = progress(m);
    if number != 0x24 {
        return 0x24;
    }
    let ranks = p + 0x24;
    let mut last = 0;
    let mut first = false;
    for pass in [0x300, 0x33c, 0x378] {
        last = u32::from(m.lbu(ranks + pass));
        if last == 1 {
            first = true;
            break;
        }
    }
    if !first {
        return last;
    }
    if m.lbu(p + 0x2b) == 1 {
        return 1;
    }
    m.sb(p + 0x2b, 1);
    unlock_aircraft(m, 0x13, false)
}

/// 800DBD1C: once every mission 1..=36 is ranked 1 in some pass, unlock
/// aircraft 0x15 once (flag +0x2D; the aircraft only with option bits
/// 0x18). Returns the routine's v0.
pub fn all_first_dbd1c(m: &mut Machine) -> u32 {
    let p = progress(m);
    if m.lbu(p + 0x2d) == 1 {
        return 1;
    }
    for n in 1..0x25 {
        if !ranked_first(m, p, n, &[0x300, 0x33c, 0x378]) {
            return 1;
        }
    }
    m.sb(p + 0x2d, 1);
    unlock_aircraft(m, 0x15, true)
}

/// 800DBE2C: once every mission 1..=36 is ranked 1 in the second or third
/// pass, unlock aircraft 0x14 once (flag +0x2C; the aircraft only with
/// option bits 0x18). Returns the routine's v0.
pub fn later_first_dbe2c(m: &mut Machine) -> u32 {
    let p = progress(m);
    if m.lbu(p + 0x2c) == 1 {
        return 1;
    }
    for n in 1..0x25 {
        if !ranked_first(m, p, n, &[0x33c, 0x378]) {
            return 1;
        }
    }
    m.sb(p + 0x2c, 1);
    unlock_aircraft(m, 0x14, true)
}

/// 800DBF2C: mark every listed aircraft owned (+0x18), and with mission
/// data also in the second set (+0x58) unless its entry +0x25 is 1.
/// Returns the routine's v0 (the context pointer for an empty list).
pub fn apply_dbf2c(m: &mut Machine) -> u32 {
    let ctx = m.gw(CONTEXT);
    let p = ctx + 0x960;
    let s = state(m);
    if sext16(m.lhu(s + 0x10)) as i32 <= 0 {
        return ctx;
    }
    let mut i = 0u32;
    loop {
        let s = state(m);
        let index = u32::from(m.lbu(s.wrapping_add(i) + 0x14));
        let at = entry(index);
        let id = sext16(m.lhu(at));
        m.sb(p.wrapping_add(id) + 0x18, 1);
        let s = state(m);
        if m.lw(s + 4) & 1 == 0 && m.lbu(at + 0x25) != 1 {
            let id = sext16(m.lhu(at));
            m.sb(p.wrapping_add(id) + 0x58, 1);
        }
        let s = state(m);
        i += 1;
        if (i as i32) >= sext16(m.lhu(s + 0x10)) as i32 {
            return 0;
        }
    }
}

/// 800DBFF8: collect the unlocks of the finished mission (`mission` the
/// current mission number for 800DB980, `previous` the number used by the
/// score and final-mission rules), then with option bit 8 set context
/// +0xA04 = 1 and +0x974 |= 3, and apply the list (800DBF2C, whose v0 is
/// returned).
pub fn collect_dbff8(m: &mut Machine, mission: u32, previous: u32) -> u32 {
    m.push_frame(0x18);
    by_mission_db980(m, mission);
    by_score_dba04(m, previous);
    final_mission_dbc54(m, previous);
    all_first_dbd1c(m);
    later_first_dbe2c(m);
    let ctx = m.gw(CONTEXT);
    if m.lw(ctx + 0x970) & 8 != 0 {
        let flags = m.lw(ctx + 0x974);
        m.sb(ctx + 0xa04, 1);
        m.sw(ctx + 0x974, flags | 3);
    }
    let result = apply_dbf2c(m);
    m.pop_frame(0x18);
    result
}

/// 800DC138: grant every listed aircraft's loadout (800DC074). Returns
/// the routine's v0.
pub fn grant_dc138(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let s = state(m);
    let count = sext16(m.lhu(s + 0x10));
    let result = if count as i32 <= 0 {
        count
    } else {
        let mut i = 0u32;
        loop {
            let s = state(m);
            let index = u32::from(m.lbu(s.wrapping_add(i) + 0x14));
            let id = sext16(m.lhu(entry(index)));
            i += 1;
            call(m, GRANT_LOADOUT, [id, 0, 0, 0]);
            let s = state(m);
            if (i as i32) >= sext16(m.lhu(s + 0x10)) as i32 {
                break 0;
            }
        }
    };
    m.pop_frame(0x20);
    result
}

/// 800DC1D4: progress +0xA3 = 1 once every mission 1..=36 is ranked 1 in
/// the third pass. Returns 1.
pub fn third_pass_dc1d4(m: &mut Machine) -> u32 {
    let p = progress(m);
    let all = (1..0x25).all(|n| m.lbu(p + n + 0x378) == 1);
    if all {
        m.sb(p + 0xa3, 1);
    }
    1
}

/// 800DC22C: progress +0xA5 = 1 after mission number 0x12. Returns 1.
pub fn mission_12_dc22c(m: &mut Machine, number: u32) -> u32 {
    let p = progress(m);
    if number == 0x12 {
        m.sb(p + 0xa5, 1);
    }
    1
}

/// 800DC254: 800DC138, 800DC1D4 and 800DC22C(`number`). Returns 1.
pub fn finish_dc254(m: &mut Machine, number: u32) -> u32 {
    m.push_frame(0x18);
    grant_dc138(m);
    third_pass_dc1d4(m);
    let result = mission_12_dc22c(m, number);
    m.pop_frame(0x18);
    result
}

/// 800DC288: start sliding the picture out (flag 2; from rest the slide
/// timer restarts at 0, while sliding in the flag 4 is replaced). Returns
/// the routine's v0.
pub fn slide_out_dc288(m: &mut Machine) -> u32 {
    let s = state(m);
    let flags = m.lw(s + 4);
    if flags & 2 != 0 {
        return flags & 4;
    }
    if flags & 4 == 0 {
        m.sw(s + 4, flags | 2);
        m.sw(s + 0xc, 0);
        return flags | 2;
    }
    let value = (flags & !4) | 2;
    m.sw(s + 4, value);
    value
}

/// 800DC2D4: the picture slide. Visual slot 1 is offset by `-t*t` (`y` is
/// the caller's `a2`, which the original leaves unset here: the tick's
/// incoming `a2`). Sliding out counts up to 25, then loads the selected
/// aircraft's picture and palette (800CE978 banks +0x200AC / +0x200B0)
/// and its text lines, and slides in, counting down to 0. Returns the
/// routine's v0.
pub fn slide_dc2d4(m: &mut Machine, a2: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let s = state(m);
    let t = m.lw(s + 0xc);
    let offset = t.wrapping_neg().wrapping_mul(t);
    call(m, SET_OFFSET, [1, offset, a2, 0]);
    let list = s + 0x14;
    let s = state(m);
    let flags = m.lw(s + 4);
    let result = if flags & 2 != 0 {
        let rect = s + 0x1c;
        if (t as i32) < 0x19 {
            let value = m.lw(s + 0xc).wrapping_add(1);
            m.sw(s + 0xc, value);
            value
        } else {
            let selection = sext16(m.lhu(s + 0x12));
            m.sw(s + 4, (flags & !2) | 4);
            let block = m.lw(MISSION);
            let index = u32::from(m.lbu(list.wrapping_add(selection)));
            m.sw(frame + 0x10, 1);
            let bank = m.lw(block.wrapping_add(0x2_00ac));
            call(m, LOAD_RESOURCE, [bank, index, 0x3820, rect]);
            let s = state(m);
            let selection = sext16(m.lhu(s + 0x12));
            let block = m.lw(MISSION);
            let index = u32::from(m.lbu(list.wrapping_add(selection)));
            m.sw(frame + 0x10, 1);
            let bank = m.lw(block.wrapping_add(0x2_00b0));
            call(m, LOAD_RESOURCE, [bank, index, 0x828, 0]);
            let s = state(m);
            let selection = sext16(m.lhu(s + 0x12));
            let index = u32::from(m.lbu(list.wrapping_add(selection)));
            let first = m.lw(entry(index) + 0x28);
            let selection = sext16(m.lhu(s + 0x12));
            m.sw(s + 0x24, first);
            let index = u32::from(m.lbu(list.wrapping_add(selection)));
            let second = m.lw(entry(index) + 0x2c);
            m.sw(s + 0x28, second);
            second
        }
    } else if flags & 4 != 0 {
        if t as i32 > 0 {
            let value = m.lw(s + 0xc).wrapping_sub(1);
            m.sw(s + 0xc, value);
            value
        } else {
            let value = flags & !4;
            m.sw(s + 4, value);
            value
        }
    } else {
        0
    };
    m.pop_frame(0x30);
    result
}

/// 800DC470, phase 0: collect the unlocks (800DBFF8 with the current
/// mission's number, context +0x390, and the number of the mission block's
/// +0xC record), grant them (800DC254). With an empty list returns 2 (skip
/// the screen); else loads the first aircraft's picture and palette into
/// the RECT (0x3C0, 0x100, 0x20, 0x90), selects visual page 6, takes its
/// text lines and returns 1.
pub fn collect_dc470(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x30);
    let ctx = m.gw(CONTEXT);
    let current = sext16(m.lhu(ctx + 0x390));
    let number = number_d17ac(m, current);
    let block = m.lw(MISSION);
    let record = m.lw(block + 0xc);
    let previous = number_d17ac(m, record);
    collect_dbff8(m, number, previous);
    finish_dc254(m, number);
    let s = state(m);
    let result = if m.lhu(s + 0x10) == 0 {
        2
    } else {
        m.sh(s + 0x1c, 0x3c0);
        m.sh(s + 0x1e, 0x100);
        m.sh(s + 0x20, 0x20);
        let index = u32::from(m.lbu(s + 0x14));
        m.sh(s + 0x22, 0x90);
        let block = m.lw(MISSION);
        m.sw(frame + 0x10, 1);
        let bank = m.lw(block.wrapping_add(0x2_00ac));
        call(m, LOAD_RESOURCE, [bank, index, 0x3820, s + 0x1c]);
        let s = state(m);
        let block = m.lw(MISSION);
        let index = u32::from(m.lbu(s + 0x14));
        m.sw(frame + 0x10, 1);
        let bank = m.lw(block.wrapping_add(0x2_00b0));
        call(m, LOAD_RESOURCE, [bank, index, 0x828, 0]);
        call(m, SELECT_PAGE, [6, 0, 0, 0]);
        let s = state(m);
        let index = u32::from(m.lbu(s + 0x14));
        let first = m.lw(entry(index) + 0x28);
        m.sw(s + 0x24, first);
        let second = m.lw(entry(index) + 0x2c);
        m.sw(s + 0x28, second);
        1
    };
    m.pop_frame(0x30);
    result
}

/// 800DC5BC, phase 1: visual slot 2's parameter follows the timer (+8)
/// for ten frames; then the screen shows texts 0xCE and 0xCD and returns
/// 1.
pub fn fade_in_dc5bc(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let timer = m.lw(s + 8);
    call(m, SET_PARAMETER, [2, timer, 0, 0]);
    let s = state(m);
    let timer = m.lw(s + 8);
    let result = if (timer as i32) < 10 {
        m.sw(s + 8, timer.wrapping_add(1));
        0
    } else {
        call(m, CLEAR_TEXT, [0; 4]);
        call(m, SET_TEXT, [0xce, 1, 0, 0]);
        call(m, SET_TEXT, [0xcd, 1, 0, 0]);
        1
    };
    m.pop_frame(0x18);
    result
}

/// Draw `string` with font 0x14 at (0x96, `y`), colour 0x808080, at the
/// mission block's next primitive.
fn draw_line(m: &mut Machine, frame: u32, string_at: u32, y: u32) {
    m.sw(frame + 0x10, 0x96);
    m.sw(frame + 0x14, y);
    m.sw(frame + 0x18, 0x0080_8080);
    let block = m.lw(MISSION);
    let ot = m.lw(block + 0x90);
    let s = state(m);
    let string = m.lw(s + string_at);
    let prim = m.lw(block.wrapping_add(NEXT_PRIMITIVE));
    let next = call(m, DRAW_STRING, [0x14, string, ot, prim]);
    let block = m.lw(MISSION);
    m.sw(block.wrapping_add(NEXT_PRIMITIVE), next);
}

/// 800DC638, phase 2: confirm or cancel (0x30) returns 2 (leave); next (8)
/// and previous (4) start sliding to the neighbouring aircraft; then the
/// slide (800DC2D4, with the incoming `a2`) and the two text lines (y 0x12C
/// and 0x140). Returns 0.
pub fn browse_dc638(m: &mut Machine, a2: u32) -> u32 {
    let frame = m.push_frame(0x38);
    let ctx = m.gw(CONTEXT);
    let pressed = m.lw(ctx + 0x12c);
    if pressed & 0x30 != 0 {
        m.pop_frame(0x38);
        return 2;
    }
    if pressed & 8 != 0 {
        let s = state(m);
        let count = sext16(m.lhu(s + 0x10)) as i32;
        let selection = sext16(m.lhu(s + 0x12)) as i32;
        if selection < count - 1 {
            slide_out_dc288(m);
            let s = state(m);
            let value = m.lhu(s + 0x12).wrapping_add(1);
            m.sh(s + 0x12, value);
        }
    } else if pressed & 4 != 0 {
        let s = state(m);
        if (sext16(m.lhu(s + 0x12)) as i32) > 0 {
            slide_out_dc288(m);
            let s = state(m);
            let value = m.lhu(s + 0x12).wrapping_sub(1);
            m.sh(s + 0x12, value);
        }
    }
    slide_dc2d4(m, a2);
    draw_line(m, frame, 0x24, 0x12c);
    draw_line(m, frame, 0x28, 0x140);
    m.pop_frame(0x38);
    0
}

/// 800DC7B0: initialization (flags 1 when option bit 0x10 is set; phase,
/// timers, list and selection cleared; visual slot 2 parameter 0 and slot
/// 1 offset (0, 0); screen text cleared). Returns 1.
pub fn init_dc7b0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let ctx = m.gw(CONTEXT);
    m.sw(s + 4, 0);
    if m.lw(ctx + 0x970) & 0x10 != 0 {
        m.sw(s + 4, 1);
    }
    let s = state(m);
    m.sw(s, 0);
    m.sw(s + 8, 0);
    call(m, SET_PARAMETER, [2, 0, 0, 0]);
    let s = state(m);
    m.sw(s + 0xc, 0);
    call(m, SET_OFFSET, [1, 0, 0, 0]);
    let s = state(m);
    m.sh(s + 0x10, 0);
    for i in 0..8 {
        let s = state(m);
        m.sb(s + i + 0x14, 0);
    }
    let s = state(m);
    m.sh(s + 0x12, 0);
    call(m, CLEAR_TEXT, [0; 4]);
    m.pop_frame(0x18);
    1
}

/// 800DC868: run the current phase (the argument registers pass through);
/// result 1 advances the phase, 2 binds the common scripts (800CEB90) and
/// leaves the state (0x100). Returns 0 otherwise.
pub fn tick_dc868(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let phase = m.lw(s);
    let callback = m.lw(PHASES.wrapping_add(phase << 2));
    let result = call(m, callback, args);
    let value = if result & 1 != 0 {
        let s = state(m);
        let phase = m.lw(s).wrapping_add(1);
        m.sw(s, phase);
        0
    } else if result & 2 != 0 {
        call(m, BIND_SCRIPTS, [0; 4]);
        0x100
    } else {
        0
    };
    m.pop_frame(0x18);
    value
}

/// Claims of this part by original address, asked by `ov2_hub::dispatch`
/// (nested so the crate-wide claim scan sees them).
pub mod claims {
    use super::*;
    use crate::{common::loaded_module, ov2_hub::MODULE};

    /// The claims, only while module 2 is loaded.
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        if loaded_module(m) != MODULE {
            return None;
        }
        let [a0, a1, a2, _] = args;
        Some(match target {
            0x800d_17ac => number_d17ac(m, a0),
            0x800d_b980 => by_mission_db980(m, a0),
            0x800d_ba04 => by_score_dba04(m, a0),
            0x800d_bc54 => final_mission_dbc54(m, a0),
            0x800d_bd1c => all_first_dbd1c(m),
            0x800d_be2c => later_first_dbe2c(m),
            0x800d_bf2c => apply_dbf2c(m),
            0x800d_bff8 => collect_dbff8(m, a0, a1),
            0x800d_c138 => grant_dc138(m),
            0x800d_c1d4 => third_pass_dc1d4(m),
            0x800d_c22c => mission_12_dc22c(m, a0),
            0x800d_c254 => finish_dc254(m, a0),
            0x800d_c288 => slide_out_dc288(m),
            0x800d_c2d4 => slide_dc2d4(m, a2),
            0x800d_c470 => collect_dc470(m),
            0x800d_c5bc => fade_in_dc5bc(m),
            0x800d_c638 => browse_dc638(m, a2),
            0x800d_c7b0 => init_dc7b0(m),
            0x800d_c868 => tick_dc868(m, args),
            _ => return None,
        })
    }
}
