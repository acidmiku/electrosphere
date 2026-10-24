//! JP overlay 2, part 2: the hub's cell animations (list 0 of the visual
//! block's cell records), translated from the JP instructions.
//!
//! A cell record is 12 bytes: +4/+6 x/y, +8 velocity, +0xA mode (0 enter,
//! 1 idle, 2 leave, 3 gone). [`animate_cell`] (800E08D4) steps cell `n`
//! by the original's jump table (0x800CC8D4): cells 1, 5 and 7 have
//! routines of their own (some timed by the visual block's counter
//! +0x1A4, with sound 0x5C), cells 11..15 slide in and out inline.
use super::ov2_p2::{MODULE, less, lh, lhu, visual};
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch;
use crate::machine::{Machine, SmallData};

/// 8002DB4C (resident, `script`): start sound effect `a0` (it reads no other argument).
const SOUND: u32 = 0x8002_db4c;

/// The cell list 0 slot `cell` (record pointer = list + 12 * cell).
fn cell_record(m: &mut Machine, block: u32, offset: u32) -> u32 {
    offset.wrapping_add(m.lw(block.wrapping_add(0x16c)))
}

/// Halfword `+at` of the record += `delta` (as the original: `lhu`, add,
/// `sh`). Returns the sum (the original's `v0`, not truncated).
fn add(m: &mut Machine, record: u32, at: u32, delta: u32) -> u32 {
    let value = lhu(m, record.wrapping_add(at)).wrapping_add(delta);
    m.sh(record.wrapping_add(at), value as u16);
    value
}

/// The record's coordinate `+at` += its velocity.
fn advance(m: &mut Machine, record: u32, at: u32) {
    let velocity = lhu(m, record.wrapping_add(8));
    add(m, record, at, velocity);
}

/// The `v0` of a mode other than 0, 1 and 2 (the original's mode tests:
/// `slti v0, mode, 2` leaves 1 for a negative mode, then `v0 = 2` for the
/// others).
fn other_mode(mode: u32) -> u32 {
    if (mode as i32) < 0 { 1 } else { 2 }
}

/// 800E03CC: return (cell 0 has no animation; v0 is the caller's).
pub fn no_animation() {}

/// 800E03D4: cell `cell`: entering, timed by counter +0x1A4: up to 0x1A
/// slide right decelerating, 0x1B..0x1C step 3 right, 0x1D..0x1E step
/// back (sound 0x5C at 0x1D), then stop and idle; leaving: slide left
/// accelerating until x < -0x13F. Returns the original's last `v0`.
pub fn animate_timed_x(m: &mut Machine, cell: u32) -> u32 {
    super::ov2_p2::framed(m, 0x20, |m, _| timed_x(m, cell))
}
fn timed_x(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let offset = cell.wrapping_mul(12);
    let record = cell_record(m, block, offset);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            let time = m.lw(block.wrapping_add(0x1a4));
            if (time as i32) <= 0 {
                return 1;
            }
            if less(time, 0x1b) {
                advance(m, record, 4);
                let record = cell_record(m, block, offset);
                let velocity = lh(m, record.wrapping_add(8));
                let raw = lhu(m, record.wrapping_add(8));
                if (velocity as i32) <= 0 {
                    return velocity;
                }
                let value = raw.wrapping_sub(1);
                m.sh(record.wrapping_add(8), value as u16);
                value
            } else if less(time, 0x1d) {
                add(m, record, 4, 3)
            } else if less(time, 0x1f) {
                if time == 0x1d {
                    dispatch::call(m, SOUND, [0x5c, 0, 0, 0]);
                }
                let block = visual(m);
                let record = cell_record(m, block, offset);
                add(m, record, 4, u32::MAX)
            } else {
                m.sh(record.wrapping_add(8), 0);
                let record = cell_record(m, block, offset);
                m.sh(record.wrapping_add(0xa), 1);
                record
            }
        }
        1 => cell.wrapping_mul(3),
        2 => {
            let velocity = lhu(m, record.wrapping_add(8));
            let x = lhu(m, record.wrapping_add(4)).wrapping_sub(velocity);
            m.sh(record.wrapping_add(4), x as u16);
            let record = cell_record(m, block, offset);
            if less(lh(m, record.wrapping_add(4)), (-0x13f_i32) as u32) {
                m.sh(record.wrapping_add(0xa), 3);
                3
            } else {
                add(m, record, 8, 2)
            }
        }
        _ => other_mode(mode),
    }
}

/// 800E056C: cell `cell`: entering, timed by counter +0x1A4 (sound 0x5C
/// at 0x2B and 0x21): up to 0x1E slide down decelerating, 0x1F..0x20
/// step 8 down, 0x21..0x22 step 4 up, then idle with cell 7's velocity
/// -0x2C; the counter counts up while entering (and while idle, to
/// 0x190), down while leaving (slide up accelerating until y < -0x1DF).
/// Returns the original's last `v0`.
pub fn animate_timed_y(m: &mut Machine, cell: u32) -> u32 {
    super::ov2_p2::framed(m, 0x20, |m, _| timed_y(m, cell))
}
fn timed_y(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let offset = cell.wrapping_mul(12);
    let record = cell_record(m, block, offset);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            if m.lw(block.wrapping_add(0x1a4)) == 0x2b {
                dispatch::call(m, SOUND, [0x5c, 0, 0, 0]);
            }
            let block = visual(m);
            let time = m.lw(block.wrapping_add(0x1a4));
            if less(time, 0x1f) {
                let record = cell_record(m, block, offset);
                advance(m, record, 6);
                let record = cell_record(m, block, offset);
                let velocity = lh(m, record.wrapping_add(8));
                let raw = lhu(m, record.wrapping_add(8));
                if (velocity as i32) > 0 {
                    m.sh(record.wrapping_add(8), raw.wrapping_sub(1) as u16);
                }
            } else if less(time, 0x21) {
                let record = cell_record(m, block, offset);
                add(m, record, 6, 8);
            } else if less(time, 0x23) {
                let record = cell_record(m, block, offset);
                add(m, record, 6, (-4_i32) as u32);
            } else {
                let record = cell_record(m, block, offset);
                m.sh(record.wrapping_add(0xa), 1);
                let records = m.lw(block.wrapping_add(0x16c));
                m.sh(records.wrapping_add(0x5c), (-0x2c_i16) as u16);
            }
            let block = visual(m);
            if m.lw(block.wrapping_add(0x1a4)) == 0x21 {
                dispatch::call(m, SOUND, [0x5c, 0, 0, 0]);
            }
            let block = visual(m);
            let time = m.lw(block.wrapping_add(0x1a4)).wrapping_add(1);
            m.sw(block.wrapping_add(0x1a4), time);
            time
        }
        1 => {
            let time = m.lw(block.wrapping_add(0x1a4));
            if !less(time, 0x190) {
                return 0;
            }
            let time = time.wrapping_add(1);
            m.sw(block.wrapping_add(0x1a4), time);
            time
        }
        2 => {
            advance(m, record, 6);
            let record = cell_record(m, block, offset);
            if less(lh(m, record.wrapping_add(6)), (-0x1df_i32) as u32) {
                m.sh(record.wrapping_add(0xa), 3);
            } else {
                add(m, record, 8, (-2_i32) as u32);
            }
            count_down(m)
        }
        _ if (mode as i32) < 0 => 0x2b,
        _ => 2,
    }
}

/// Counter +0x1A4 -= 1 while positive; returns the counter as last read
/// or written (the original's `v0`).
fn count_down(m: &mut Machine) -> u32 {
    let block = visual(m);
    let time = m.lw(block.wrapping_add(0x1a4));
    if (time as i32) <= 0 {
        return time;
    }
    let time = time.wrapping_sub(1);
    m.sw(block.wrapping_add(0x1a4), time);
    time
}

/// 800E0798: cell `cell`: entering once cell 5 idles: slide (y clamped at
/// 0) accelerating while the velocity is negative, then idle; idle: y
/// clamped at 0; leaving: slide down accelerating until y >= 0x1E0.
/// Returns the original's last `v0`.
pub fn animate_follow(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let records = m.lw(block.wrapping_add(0x16c));
    let offset = cell.wrapping_mul(12);
    let record = offset.wrapping_add(records);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            let leader = lh(m, records.wrapping_add(0x46));
            if leader != 1 {
                return 1;
            }
            advance(m, record, 6);
            let record = cell_record(m, block, offset);
            if less(lh(m, record.wrapping_add(6)), 0) {
                m.sh(record.wrapping_add(6), 0);
            }
            let block = visual(m);
            let record = cell_record(m, block, offset);
            let velocity = lh(m, record.wrapping_add(8));
            let value = lhu(m, record.wrapping_add(8)).wrapping_add(2);
            if less(velocity, 0) {
                m.sh(record.wrapping_add(8), value as u16);
            } else {
                m.sh(record.wrapping_add(0xa), leader as u16);
            }
            value
        }
        1 => {
            let y = lh(m, record.wrapping_add(6));
            if less(y, 0) {
                m.sh(record.wrapping_add(6), 0);
            }
            y
        }
        2 => {
            advance(m, record, 6);
            let record = cell_record(m, block, offset);
            if less(lh(m, record.wrapping_add(6)), 0x1e0) {
                add(m, record, 8, 4)
            } else {
                m.sh(record.wrapping_add(0xa), 3);
                3
            }
        }
        _ => other_mode(mode),
    }
}

/// The shared tail of cells 11 and 13 entering: the velocity rises to 0
/// while negative, then the cell idles. Returns the velocity as re-read.
fn settle(m: &mut Machine, record: u32, offset: u32) -> u32 {
    let velocity = lh(m, record.wrapping_add(8));
    let raw = lhu(m, record.wrapping_add(8));
    if less(velocity, 0) {
        m.sh(record.wrapping_add(8), raw.wrapping_add(1) as u16);
    }
    let block = visual(m);
    let record = cell_record(m, block, offset);
    let velocity = lh(m, record.wrapping_add(8));
    if velocity == 0 {
        m.sh(record.wrapping_add(0xa), 1);
    }
    velocity
}

/// The original's jump table of [`animate_cell`] (16 code addresses).
const CELL_CASES: u32 = 0x800c_c8d4;

/// 800E08D4: step cell `cell` (0..0xF; others nothing), by the original's
/// jump table. Returns the original's last `v0`: the case routine's, or
/// for the cells without one (and cell 0, whose 800E03CC keeps `v0`) the
/// jump target loaded from the table, or 0 for `cell` >= 16.
pub fn animate_cell(m: &mut Machine, cell: u32) -> u32 {
    super::ov2_p2::framed(m, 0x20, |m, _| {
        if cell >= 0x10 {
            return 0;
        }
        let target = m.lw(CELL_CASES.wrapping_add(cell << 2));
        match cell {
            1 => animate_timed_x(m, cell),
            5 => animate_timed_y(m, cell),
            7 => animate_follow(m, cell),
            11 => slide_x_back(m, cell),
            12 => slide_x_bounce(m, cell),
            13 => slide_x(m, cell),
            14 => slide_y(m, cell),
            15 => slide_y_timed(m, cell),
            0 => {
                no_animation();
                target
            }
            _ => target,
        }
    })
}

/// Cell 11 (800E0940): entering: slide in x, the velocity rising to 0;
/// leaving: slide in x accelerating until the velocity reaches 0x1A.
fn slide_x_back(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let offset = cell.wrapping_mul(12);
    let record = cell_record(m, block, offset);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            advance(m, record, 4);
            let record = cell_record(m, block, offset);
            settle(m, record, offset)
        }
        1 => cell.wrapping_mul(3),
        2 => {
            advance(m, record, 4);
            let record = cell_record(m, block, offset);
            add(m, record, 8, 1);
            let record = cell_record(m, block, offset);
            if less(lh(m, record.wrapping_add(8)), 0x1a) {
                return 1;
            }
            m.sh(record.wrapping_add(0xa), 3);
            3
        }
        _ => other_mode(mode),
    }
}

/// Cell 12 (800E0A14): entering: slide in x decelerating to 0, then idle;
/// while entering, held-and-pressed pad bit 8 or bit 4 (context +0x128
/// and +0x12C) reverses the velocity and makes it leave; leaving: slide
/// decelerating until the velocity falls below -0x18.
fn slide_x_bounce(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let offset = cell.wrapping_mul(12);
    let record = cell_record(m, block, offset);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            advance(m, record, 4);
            let record = cell_record(m, block, offset);
            let velocity = lh(m, record.wrapping_add(8));
            let raw = lhu(m, record.wrapping_add(8));
            if (velocity as i32) > 0 {
                m.sh(record.wrapping_add(8), raw.wrapping_sub(1) as u16);
            }
            let current = visual(m);
            let record = cell_record(m, current, offset);
            if lh(m, record.wrapping_add(8)) == 0 {
                m.sh(record.wrapping_add(0xa), 1);
            }
            for bit in [8, 4] {
                let context = m.gw(CONTEXT);
                if m.lw(context.wrapping_add(0x128)) & bit == 0
                    || m.lw(context.wrapping_add(0x12c)) & bit == 0
                {
                    continue;
                }
                let block = visual(m);
                let record = cell_record(m, block, offset);
                let velocity = lhu(m, record.wrapping_add(8));
                m.sh(record.wrapping_add(8), 0u32.wrapping_sub(velocity) as u16);
                let record = cell_record(m, block, offset);
                m.sh(record.wrapping_add(0xa), 2);
                if bit == 4 {
                    return 2;
                }
            }
            // The last test's value: (held or pressed) & 4 = 0.
            0
        }
        1 => cell.wrapping_mul(3),
        2 => {
            advance(m, record, 4);
            let record = cell_record(m, block, offset);
            add(m, record, 8, u32::MAX);
            let record = cell_record(m, block, offset);
            if !less(lh(m, record.wrapping_add(8)), (-0x18_i32) as u32) {
                return 0;
            }
            m.sh(record.wrapping_add(0xa), 3);
            3
        }
        _ => other_mode(mode),
    }
}

/// Cell 13 (800E0BFC): entering as cell 11; leaving: slide in x
/// accelerating.
fn slide_x(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let offset = cell.wrapping_mul(12);
    let record = cell_record(m, block, offset);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            advance(m, record, 4);
            let record = cell_record(m, block, offset);
            settle(m, record, offset)
        }
        1 => cell.wrapping_mul(3),
        2 => {
            advance(m, record, 4);
            let record = cell_record(m, block, offset);
            add(m, record, 8, 1)
        }
        _ => other_mode(mode),
    }
}

/// Cell 14 (800E0CEC): entering: slide in y decelerating while the
/// velocity is positive, then idle; leaving: slide in y decelerating
/// until the velocity falls below -0x18.
fn slide_y(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let offset = cell.wrapping_mul(12);
    let records = m.lw(block.wrapping_add(0x16c));
    let record = offset.wrapping_add(records);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            advance(m, record, 6);
            let record = cell_record(m, block, offset);
            let velocity = lh(m, record.wrapping_add(8));
            let value = lhu(m, record.wrapping_add(8)).wrapping_sub(1);
            if (velocity as i32) > 0 {
                m.sh(record.wrapping_add(8), value as u16);
            } else {
                m.sh(record.wrapping_add(0xa), 1);
            }
            value
        }
        // This case leaves the list pointer in v0.
        1 => records,
        2 => {
            advance(m, record, 6);
            let record = cell_record(m, block, offset);
            let velocity = lh(m, record.wrapping_add(8));
            let raw = lhu(m, record.wrapping_add(8));
            if less(velocity, (-0x18_i32) as u32) {
                m.sh(record.wrapping_add(0xa), 3);
                3
            } else {
                let value = raw.wrapping_sub(1);
                m.sh(record.wrapping_add(8), value as u16);
                value
            }
        }
        _ => other_mode(mode),
    }
}

/// Cell 15 (800E0DD8): entering, timed by counter +0x1A4 (counting up):
/// up to 0x20 slide in y with the velocity rising while negative,
/// 0x21..0x23 step 8 up, 0x24..0x25 step 4 down (sound 0x5C at 0x24),
/// then idle; leaving (counter counting down): slide down accelerating
/// until y >= 0x1E0.
fn slide_y_timed(m: &mut Machine, cell: u32) -> u32 {
    let block = visual(m);
    let offset = cell.wrapping_mul(12);
    let record = cell_record(m, block, offset);
    let mode = lh(m, record.wrapping_add(0xa));
    match mode {
        0 => {
            let time = m.lw(block.wrapping_add(0x1a4));
            if less(time, 0x21) {
                advance(m, record, 6);
                let record = cell_record(m, block, offset);
                let velocity = lh(m, record.wrapping_add(8));
                let raw = lhu(m, record.wrapping_add(8));
                if less(velocity, 0) {
                    m.sh(record.wrapping_add(8), raw.wrapping_add(1) as u16);
                }
            } else if less(time, 0x24) {
                add(m, record, 6, (-8_i32) as u32);
            } else if less(time, 0x26) {
                if time == 0x24 {
                    dispatch::call(m, SOUND, [0x5c, 0, 0, 0]);
                }
                let block = visual(m);
                let record = cell_record(m, block, offset);
                add(m, record, 6, 4);
            } else {
                m.sh(record.wrapping_add(0xa), 1);
            }
            let block = visual(m);
            let time = m.lw(block.wrapping_add(0x1a4)).wrapping_add(1);
            m.sw(block.wrapping_add(0x1a4), time);
            time
        }
        1 => cell.wrapping_mul(3),
        2 => {
            advance(m, record, 6);
            let record = cell_record(m, block, offset);
            if less(lh(m, record.wrapping_add(6)), 0x1e0) {
                add(m, record, 8, 2);
            } else {
                m.sh(record.wrapping_add(0xa), 3);
            }
            count_down(m)
        }
        _ => other_mode(mode),
    }
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let a0 = args[0];
    Some(match target {
        0x800e_03cc => {
            no_animation();
            0
        }
        0x800e_03d4 => animate_timed_x(m, a0),
        0x800e_056c => animate_timed_y(m, a0),
        0x800e_0798 => animate_follow(m, a0),
        0x800e_08d4 => animate_cell(m, a0),
        _ => return None,
    })
}
