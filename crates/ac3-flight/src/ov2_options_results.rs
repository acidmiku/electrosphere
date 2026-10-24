//! Mission state 11: the DEBRIEFING screen (title "DEBRIEFING",
//! "Nachbesprechung", "RAPPORT", "INSTRUCCIONES", "RELAZIONE"), entered at the overlay's
//! initialization after a flight returned (progress flag 8). Step 0
//! (800D2C98) records the results into the career statistics (800D191C),
//! picks the next mission and moves the map pictures in VRAM; step 1
//! (800D2E18) runs ov2core's loader for the next mission (callbacks of table
//! 0x800F39F4) while it draws the results: the destroyed targets counted
//! in time with the flight's track (a list of destroyed unit types and one
//! of target categories, with counts), the
//! flight time, the map with the player's trail and the kill markers, and,
//! once loading is done, the result texts (helps 0xCC/0xCD, the rank's
//! style and the story lines paged two at a time); CROSS or TRIANGLE ends
//! it. Step 2 (800D31FC) fades slot 2 over 10 frames and returns 0x100
//! (next state 12).
//!
//! The screen's block `[0x800F5D50]`: +0 flags (1 text done, 2 between
//! pages), +4 page timer, +8 story line count (-1 none), +0xC line, +0x10 /
//! +0x12 loader phase and substate, +0x14 the display clock (frames, up to
//! the flight time / 30), +0x18 the fade timer, +0x1C the next event,
//! +0x20 / +0x22 the unit types / target categories listed, +0x24 + 4k the
//! kill markers {x, y, side, shown}, +0x124 + 0x2C k the unit types {name,
//! +0x28 kind, +0x29 count}, +0x284 + 0x2C k the categories.
//!
//! The flight's result record `[0x800F942C]` (= context +0x3BC): +2 the
//! flight time, +8 the track length, +0xA the event count, +0xB the rank,
//! +0xC + 2i the track (x, y bytes), +0x390 + 8k the events {kind, side,
//! time halfword, +4 x, +5 y}.
use super::{CLEAR_TEXT, SET_VISUAL_PARAMETER, SET_VISUAL_STYLE, SHOW_TEXT, call_pointer, context};
use crate::{
    machine::Machine,
    ov2_briefing::{
        DRAW_TEXT, SPRINTF, STRLEN, WORK_HIGH, add_prim, div_pow2, div25, external, half, lbu, lh,
        lhu, mulhi, sb, sext16, sh, sound, sra, text_line, text_line_count, work,
    },
};

/// `[0x800F5D50]`: the results block.
pub const RESULTS: u32 = 0x800f_5d50;
/// `[0x800F942C]`: the flight's result record (set by 800D2C98).
pub const RECORD: u32 = 0x800f_942c;
/// 800F5D54: unit type names by event kind < 0x40 ("NON", "EF2000E",
/// "MiG-33", ...); 800F5E54: category names by kind 0x70..0x74 ("AIR TGT",
/// "GROUND TGT", "GUN", "MSSL", "NON TGT").
pub const UNIT_NAMES: u32 = 0x800f_5d54;
pub const CATEGORY_NAMES: u32 = 0x800f_5e54;
/// 800F3A08: the screen title by language.
pub const TITLES: u32 = 0x800f_3a08;
/// 800F39F4: ov2core's loader callbacks (phase lists).
pub const CALLBACKS: u32 = 0x800f_39f4;
/// 800CB10C / 800CB114: the count ("%2d\n") and time ("%2d:%2d.%2d")
/// formats.
const COUNT_FORMAT: u32 = 0x800c_b10c;
const TIME_FORMAT: u32 = 0x800c_b114;
/// 800CB120 / 800CB128: the two VRAM rectangles moved by 800D2C98.
const MOVED_RECTS: u32 = 0x800c_b120;

// Routines owned elsewhere, called by original address.
/// ov2core: select loader page (800CEB80).
const SET_PAGE: u32 = 0x800c_eb80;
/// ov2air: the category of a mission (800D17D0).
const MISSION_CATEGORY: u32 = 0x800d_17d0;
/// SDK: MoveImage, DrawSync, SetDrawArea, SetDrawMode, strcpy, rand.
const MOVE_IMAGE: u32 = 0x800a_69d0;
const DRAW_SYNC: u32 = 0x800a_6664;
const SET_DRAW_AREA: u32 = 0x800a_73f0;
const SET_DRAW_MODE: u32 = 0x800a_ac38;
const STRCPY: u32 = 0x800a_c2d8;
const RAND: u32 = 0x800a_c178;

fn results(m: &mut Machine) -> u32 {
    m.lw(RESULTS)
}
fn record(m: &mut Machine) -> u32 {
    m.lw(RECORD)
}
/// The work block's ordering-table slot `[+0x90]` and primitive cursor
/// `[+0x20094]`.
fn ot_slot(m: &mut Machine) -> u32 {
    let block = work(m);
    m.lw(block + 0x90)
}
fn cursor(m: &mut Machine) -> u32 {
    let high = work(m).wrapping_add(WORK_HIGH);
    m.lw(high + 0x94)
}
/// The story text table `[work +0x200B8]`.
fn story_texts(m: &mut Machine) -> u32 {
    let high = work(m).wrapping_add(WORK_HIGH);
    m.lw(high + 0xb8)
}
fn set_cursor(m: &mut Machine, value: u32) {
    let high = work(m).wrapping_add(WORK_HIGH);
    m.sw(high + 0x94, value);
}
/// The compiled unsigned `x / 30` of a halfword (`multu` by 0x88888889,
/// `mfhi`, `srl 4`, masked to 16 bits).
fn div30(value: u32) -> u32 {
    (((u64::from(value) * 0x8888_8889) >> 32) as u32 >> 4) & 0xffff
}

/// 800CF358: a flat line (LINE_F2, code 0x40, 0x42 when `[sp+0x18]` is
/// set) from (`x0`, `y0`) to the stack's (`[sp+0x10]`, `[sp+0x14]`) in the
/// color at `[sp+0x1C]`, linked at `slot`. Returns the next primitive.
pub fn line(m: &mut Machine, prim: u32, slot: u32, x0: u32, y0: u32) -> u32 {
    let sp = m.sp;
    let semi = m.lw(sp + 0x18);
    let color = m.lw(sp + 0x1c);
    let x1 = lhu(m, sp + 0x10);
    let y1 = lhu(m, sp + 0x14);
    sb(m, prim + 3, 3);
    sb(m, prim + 7, 0x40);
    sb(m, prim + 7, if semi != 0 { 0x42 } else { 0x40 });
    let r = lbu(m, color);
    sb(m, prim + 4, r);
    let g = lbu(m, color + 1);
    sb(m, prim + 5, g);
    let b = lbu(m, color + 2);
    sh(m, prim + 8, x0);
    sh(m, prim + 0xa, y0);
    sh(m, prim + 0xc, x1);
    sh(m, prim + 0xe, y1);
    sb(m, prim + 6, b);
    add_prim(m, slot, prim);
    prim.wrapping_add(0x10)
}

/// 800D1828 (owned by `ov2_hub`): the mission (+0) of the first of the 41
/// mission records (0x800F5958) whose key (+2) is `key`, else the first
/// record's.
pub fn mission_of(m: &mut Machine, key: u32) -> u32 {
    // One implementation: ov2_hub (state 13) owns 800D1828.
    crate::dispatch::call(m, 0x800d_1828, [key, 0, 0, 0])
}

/// 800D191C: record the flight into the career statistics (context +0x960
/// on): the time (+0x3BE) as the category's last time and, when better or
/// first, its best for the difficulty (+0x398), the time totals; the rank
/// (+0x3C7) as the last rank and, when better or first, the best; the kill
/// count (+0x3C6) by category and in total; the bonus +0x95C (cleared).
pub fn record_results(m: &mut Machine) {
    m.push_frame(0x28);
    let ctx = context(m);
    let stats = ctx + 0x960;
    let time = lhu(m, ctx + 0x3be);
    let difficulty = m.lw(ctx + 0x398);
    let mission = lbu(m, ctx + 0x3c0);
    let rank = lbu(m, ctx + 0x3c7);
    let flight = ctx + 0x3bc;
    let category = external(m, MISSION_CATEGORY, [mission, 0, 0, 0]) & 0xff;
    if time != 0 {
        let slot = stats.wrapping_add(category << 1);
        sh(m, slot + 0x288, time);
        let best = match difficulty {
            0 => Some(0xa8),
            1 => Some(0x120),
            2 => Some(0x198),
            3 => Some(0x210),
            _ => None,
        };
        if let Some(offset) = best {
            let old = lhu(m, slot + offset);
            if time < old || old == 0 {
                sh(m, slot + offset, time);
            }
        }
        let first = m.lw(stats + 0x60c);
        let second = m.lw(stats + 0x618);
        m.sw(stats + 0x60c, first.wrapping_add(time));
        m.sw(stats + 0x618, second.wrapping_add(time));
    }
    let slot = stats.wrapping_add(category);
    sb(m, slot + 0x3f0, rank);
    let best = match difficulty {
        0 => Some(0x300),
        1 => Some(0x33c),
        2 => Some(0x378),
        3 => Some(0x3b4),
        _ => None,
    };
    if let Some(offset) = best {
        let old = lbu(m, slot + offset);
        if old == 0 || rank < old {
            sb(m, slot + offset, rank);
        }
    }
    let ctx = context(m);
    let kills = lbu(m, flight + 0xa);
    let entry = ctx.wrapping_add(category << 2);
    m.sw(entry + 0xd8c, kills);
    let kills = lbu(m, flight + 0xa);
    let total = m.lw(ctx + 0xf70).wrapping_add(kills);
    let bonus = lh(m, ctx + 0x95c);
    m.sw(ctx + 0xf70, total);
    let value = m.lw(entry + 0xe7c).wrapping_add(bonus);
    m.sw(entry + 0xe7c, value);
    let bonus = lh(m, ctx + 0x95c);
    let value = m.lw(ctx + 0xf74);
    sh(m, ctx + 0x95c, 0);
    m.sw(ctx + 0xf74, value.wrapping_add(bonus));
    m.pop_frame(0x28);
}

/// Event `index` of the record, its marker in the results block and its
/// class (1 a unit type < 0x40, 2 a category 0x70..0x74, else 0).
fn event_at(m: &mut Machine, record: u32, block: u32, index: u32) -> (u32, u32, u32) {
    let event = record.wrapping_add((index << 3).wrapping_add(0x390));
    let marker = block.wrapping_add((index << 2).wrapping_add(0x24));
    let kind = lbu(m, event);
    let class = if kind < 0x40 {
        1
    } else {
        u32::from(kind.wrapping_sub(0x70) < 5) << 1
    };
    (event, marker, class)
}

/// One kind list of 800D1BA4 (`count` at +0x20 or +0x22, entries from
/// `base` = 0 or 0x160, names from `names` by kind less `first`): the
/// event's kind counts once more, or is appended with its name and count 1.
fn count_kind(m: &mut Machine, event: u32, count: u32, base: u32, names: u32, first: u32) {
    let block = results(m);
    let kinds = lh(m, block + count);
    let mut index = 0u32;
    if (kinds as i32) > 0 {
        let kind = lbu(m, event);
        let mut entry = block;
        loop {
            if lbu(m, entry + base + 0x14c) == kind {
                let value = lbu(m, entry + base + 0x14d) + 1;
                sb(m, entry + base + 0x14d, value);
                return;
            }
            index = index.wrapping_add(1);
            if (index as i32) >= (kinds as i32) {
                break;
            }
            entry = entry.wrapping_add(0x2c);
        }
    }
    let offset = index.wrapping_mul(0x2c).wrapping_add(base);
    let block = results(m);
    let kind = lbu(m, event);
    sb(m, block.wrapping_add(offset) + 0x14c, kind);
    let kind = lbu(m, event).wrapping_sub(first);
    let name = m.lw(names.wrapping_add(kind << 2));
    let block = results(m);
    external(m, STRCPY, [block.wrapping_add(offset) + 0x124, name, 0, 0]);
    let block = results(m);
    sb(m, block.wrapping_add(offset) + 0x14d, 1);
    let block = results(m);
    let kinds = lhu(m, block + count).wrapping_add(1);
    sh(m, block + count, kinds);
}

/// 800D1BA4: count the events up to the display clock: each event whose
/// time (+2) the clock (+0x14, limited by the track length +0x3C4 at a
/// quarter of its rate) has reached shows its marker and counts its kind
/// (sound 0x2E for a unit type, 0x2F for a category). Returns 0 when all
/// events were counted
/// before, else 1.
pub fn tally(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let result = tally_body(m);
    m.pop_frame(0x28);
    result
}

fn tally_body(m: &mut Machine) -> u32 {
    let flight = record(m);
    let block = results(m);
    let index = m.lw(block + 0x1c);
    if (index as i32) >= (lbu(m, flight + 0xa) as i32) {
        return 0;
    }
    let mut clock = m.lw(block + 0x14);
    let ctx = context(m);
    let length = lh(m, ctx + 0x3c4);
    if (div_pow2(clock, 2) as i32) >= (length as i32) {
        clock = length;
    }
    if (clock as i32) < 0 {
        clock = 0;
    }
    let (mut event, mut marker, mut class) = event_at(m, flight, block, index);
    loop {
        if (clock as i32) < (lh(m, event + 2) as i32) {
            return 1;
        }
        let flight = record(m);
        let block = results(m);
        if (m.lw(block + 0x1c) as i32) >= (lbu(m, flight + 0xa) as i32) {
            return 1;
        }
        sb(m, marker + 3, 1);
        let x = lbu(m, event + 4);
        sb(m, marker, x);
        let y = lbu(m, event + 5);
        sb(m, marker + 1, y);
        let side = lbu(m, event + 1);
        sb(m, marker + 2, side);
        let block = results(m);
        let next = m.lw(block + 0x1c).wrapping_add(1);
        m.sw(block + 0x1c, next);
        match class {
            1 => {
                count_kind(m, event, 0x20, 0, UNIT_NAMES, 0);
                sound(m, 0x2e);
            }
            2 => {
                count_kind(m, event, 0x22, 0x160, CATEGORY_NAMES, 0x70);
                sound(m, 0x2f);
            }
            _ => {}
        }
        let flight = record(m);
        let block = results(m);
        let index = m.lw(block + 0x1c);
        if index == lbu(m, flight + 0xa) {
            return 1;
        }
        (event, marker, class) = event_at(m, flight, block, index);
    }
}

/// The kind list of 800D1F28 at `count` (+0x20 / +0x22) with entries from
/// `base`: per kind its name at (0xA5, `y`) and its count ("%2d\n", a leading
/// zero below 10) at (0x112, `count_y`), 0x16 apart.
fn kind_list(m: &mut Machine, frame: u32, cursor: u32, ot: u32, rows: (u32, u32, u32)) -> u32 {
    let (count, base, (y, count_y)) = (rows.0, rows.1, rows_y(rows.2));
    let mut cursor = cursor;
    let block = results(m);
    if (lh(m, block + count) as i32) <= 0 {
        return cursor;
    }
    let mut index = 0u32;
    loop {
        let offset = index.wrapping_mul(0x2c).wrapping_add(base);
        let step = index.wrapping_mul(0x16);
        let block = results(m);
        m.sw(frame + 0x10, 0xa5);
        m.sw(frame + 0x14, y.wrapping_add(step));
        m.sw(frame + 0x18, 0x0080_8080);
        let name = block.wrapping_add(offset) + 0x124;
        cursor = external(m, DRAW_TEXT, [0x12, name, ot, cursor]);
        let block = results(m);
        let kills = lbu(m, block.wrapping_add(offset) + 0x14d);
        external(m, SPRINTF, [frame + 0x20, COUNT_FORMAT, kills, 0]);
        let block = results(m);
        if lbu(m, block.wrapping_add(offset) + 0x14d) < 0xa {
            sb(m, frame + 0x20, 0x30);
        }
        m.sw(frame + 0x10, 0x112);
        m.sw(frame + 0x14, count_y.wrapping_add(step));
        m.sw(frame + 0x18, 0x0080_8080);
        cursor = external(m, DRAW_TEXT, [0x12, frame + 0x20, ot, cursor]);
        index = index.wrapping_add(1);
        let block = results(m);
        if (index as i32) >= (lh(m, block + count) as i32) {
            break;
        }
    }
    cursor
}

/// The name / count rows' y of the unit (0) and category (1) lists.
fn rows_y(list: u32) -> (u32, u32) {
    if list == 0 {
        (0x31, 0x38)
    } else {
        (0xd4, 0xdc)
    }
}

/// 800D1F28: the unit type and category lists. Returns the next primitive.
pub fn draw_kinds(m: &mut Machine, cursor: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x50);
    let cursor = kind_list(m, frame, cursor, ot, (0x20, 0, 0));
    let cursor = kind_list(m, frame, cursor, ot, (0x22, 0x160, 1));
    m.pop_frame(0x50);
    cursor
}

/// 800D2174: the flight time "%2d:%2d.%2d" (spaces as zeros) at
/// (0x1E, 0x14F): minutes and seconds of the display clock (30/25 s per
/// tick), and random hundredths while it runs, else the time's
/// (time % 25 * 4). Returns the next primitive.
pub fn draw_time(m: &mut Machine, cursor: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x58);
    let block = results(m);
    let clock = m.lw(block + 0x14);
    let seconds = div25(clock.wrapping_mul(30));
    let ctx = context(m);
    let time = lhu(m, ctx + 0x3be);
    let hundredths = if (clock as i32) < (div30(time) as i32) {
        let value = external(m, RAND, [0; 4]);
        let tens = sra(mulhi(value, 0x51eb_851f), 5).wrapping_sub(sra(value, 31));
        value.wrapping_sub(tens.wrapping_mul(100))
    } else {
        let quarter = (((u64::from(time) * 0x51eb_851f) >> 32) as u32) >> 3;
        (time.wrapping_sub(quarter.wrapping_mul(25)) & 0xffff) << 2
    };
    m.sw(frame + 0x10, hundredths);
    let minutes =
        sra(mulhi(seconds, 0x8888_8889).wrapping_add(seconds), 5).wrapping_sub(sra(seconds, 31));
    let rest = seconds.wrapping_sub(minutes.wrapping_mul(60));
    let text = frame + 0x20;
    external(m, SPRINTF, [text, TIME_FORMAT, minutes, rest]);
    let mut k = 0u32;
    loop {
        let length = external(m, STRLEN, [text, 0, 0, 0]);
        if (k as i32) >= (length as i32) {
            break;
        }
        if lbu(m, text + k) == 0x20 {
            sb(m, text + k, 0x30);
        }
        k = k.wrapping_add(1);
    }
    m.sw(frame + 0x10, 0x1e);
    m.sw(frame + 0x14, 0x14f);
    m.sw(frame + 0x18, 0x0080_8080);
    let next = external(m, DRAW_TEXT, [0x12, text, ot, cursor]);
    m.pop_frame(0x58);
    next
}

/// 800D233C: a drawing area (DR_AREA, SetDrawArea): `which` 0 the screen
/// (0, 0, 320, 480), 1 the map window (20, 70, 128, 240); linked at the
/// work block's slot. Returns the next primitive.
pub fn draw_area(m: &mut Machine, prim: u32, which: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let rect = frame + 0x10;
    if which == 0 {
        sh(m, rect + 4, 0x140);
        sh(m, rect, 0);
        sh(m, rect + 2, 0);
        sh(m, rect + 6, 0x1e0);
    } else if which == 1 {
        sh(m, rect, 0x14);
        sh(m, rect + 2, 0x46);
        sh(m, rect + 4, 0x80);
        sh(m, rect + 6, 0xf0);
    }
    external(m, SET_DRAW_AREA, [prim, rect, 0, 0]);
    let slot = ot_slot(m);
    add_prim(m, slot, prim);
    m.pop_frame(0x28);
    prim.wrapping_add(0xc)
}

/// The track sample of the display clock: +0x14 / 4, limited to the track
/// length (context +0x3C4) and to 0.
fn track_index(m: &mut Machine) -> u32 {
    let block = results(m);
    let clock = div_pow2(m.lw(block + 0x14), 2);
    let ctx = context(m);
    let length = lh(m, ctx + 0x3c4);
    let index = if (clock as i32) < (length as i32) {
        clock
    } else {
        length
    };
    if (index as i32) < 0 { 0 } else { index }
}

/// The (x, y) bytes of track sample `index`.
fn sample(m: &mut Machine, index: u32) -> (u32, u32) {
    let flight = record(m);
    let entry = flight.wrapping_add(index << 1);
    (lbu(m, entry + 0xc), lbu(m, entry + 0xd))
}

/// A DR_MODE (SetDrawMode, texture page `page`, no window) at `prim`,
/// linked at `ot`.
fn draw_mode(m: &mut Machine, frame: u32, prim: u32, ot: u32, page: u32) {
    m.sw(frame + 0x10, 0);
    external(m, SET_DRAW_MODE, [prim, 0, 0, page]);
    add_prim(m, ot, prim);
}

/// 800D2410: the kill markers on the map: per shown marker (+0x24 + 4k,
/// at most 64) relative to the track position, a DR_MODE and an X of six
/// semi-transparent lines in the side's color (0 white, 1 blue, 2 black,
/// 3 yellow, 4 red; the inner pair at double intensity), then a DR_MODE.
/// Returns the next primitive.
pub fn draw_markers(m: &mut Machine, cursor: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x70);
    let result = markers_body(m, frame, cursor, ot);
    m.pop_frame(0x70);
    result
}

fn markers_body(m: &mut Machine, frame: u32, cursor: u32, ot: u32) -> u32 {
    let index = track_index(m);
    let color = frame + 0x20;
    m.sw(frame + 0x28, 0);
    m.sw(frame + 0x40, color);
    let (x, y) = sample(m, index);
    sh(m, frame + 0x30, x);
    sh(m, frame + 0x38, 0xffu32.wrapping_sub(y));
    let mut cursor = cursor;
    loop {
        let k = m.lw(frame + 0x28);
        let block = results(m);
        let marker = block.wrapping_add((k << 2).wrapping_add(0x24));
        if lbu(m, marker + 3) == 0 {
            return cursor;
        }
        let mode = cursor;
        cursor = cursor.wrapping_add(0xc);
        let mx = lbu(m, marker)
            .wrapping_sub(lhu(m, frame + 0x30))
            .wrapping_add(0x53);
        let back = 0xffu32.wrapping_sub(lh(m, frame + 0x38));
        let my = (back.wrapping_sub(lbu(m, marker + 1)) << 1).wrapping_add(0xc0);
        draw_mode(m, frame, mode, ot, 0);
        let rgb = match lbu(m, marker + 2) {
            0 => Some(0x00ff_ffff),
            1 => Some(0x00ff_0000),
            2 => Some(0),
            3 => Some(0xffff),
            4 => Some(0xff),
            _ => None,
        };
        if let Some(rgb) = rgb {
            m.sw(color, rgb);
        }
        let center = sext16(my);
        let (top, bottom) = (center.wrapping_sub(8), center.wrapping_add(8));
        let lines = [
            (mx.wrapping_sub(3), top, mx.wrapping_add(5), bottom),
            (mx.wrapping_sub(5), top, mx.wrapping_add(3), bottom),
            (mx.wrapping_sub(3), bottom, mx.wrapping_add(5), top),
            (mx.wrapping_sub(5), bottom, mx.wrapping_add(3), top),
        ];
        for (x0, y0, x1, y1) in lines {
            cursor = marker_line(m, frame, cursor, ot, (x0, y0, x1, y1));
        }
        let doubled = (lbu(m, color) << 1) | (lbu(m, color + 1) << 9) | (lbu(m, color + 2) << 17);
        m.sw(color, doubled);
        let inner = [
            (mx.wrapping_sub(4), top, mx.wrapping_add(4), bottom),
            (mx.wrapping_sub(4), bottom, mx.wrapping_add(4), top),
        ];
        for (x0, y0, x1, y1) in inner {
            cursor = marker_line(m, frame, cursor, ot, (x0, y0, x1, y1));
        }
        let mode = cursor;
        cursor = cursor.wrapping_add(0xc);
        let k = m.lw(frame + 0x28).wrapping_add(1);
        draw_mode(m, frame, mode, ot, 0);
        m.sw(frame + 0x28, k);
        if (m.lw(frame + 0x28) as i32) >= 0x40 {
            return cursor;
        }
    }
}

/// One semi-transparent marker line through 800CF358 with its stack
/// arguments in the frame.
fn marker_line(m: &mut Machine, frame: u32, prim: u32, ot: u32, ends: (u32, u32, u32, u32)) -> u32 {
    let (x0, y0, x1, y1) = ends;
    m.sw(frame + 0x18, 1);
    let color = m.lw(frame + 0x40);
    m.sw(frame + 0x10, x1);
    m.sw(frame + 0x14, y1);
    m.sw(frame + 0x1c, color);
    line(m, prim, ot, x0, y0)
}

/// 800D27D0: the player's trail: up to 40 semi-transparent Gouraud lines
/// (LINE_G2, code 0x52) back along the track from the display clock's
/// sample, fading from 0x80 by 2 per step, relative to it; then a DR_MODE
/// (page 0x20: additive). Returns the next primitive.
pub fn draw_trail(m: &mut Machine, cursor: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let now = track_index(m);
    let (cx, cy) = sample(m, now);
    let mut cursor = cursor;
    for k in 0..0x28u32 {
        let index = now.wrapping_sub(k);
        if (index as i32) <= 0 {
            continue;
        }
        let flight = record(m);
        if (index as i32) >= (lh(m, flight + 8) as i32) {
            continue;
        }
        let near = 0x80u32.wrapping_sub(k << 1);
        let far = 0x80u32.wrapping_sub((k << 1) + 2);
        let (px, py) = sample(m, index);
        let (qx, qy) = sample(m, index.wrapping_sub(1));
        let prim = cursor;
        sb(m, prim + 3, 4);
        sb(m, prim + 7, 0x52);
        for offset in [4, 5, 6] {
            sb(m, prim + offset, near);
        }
        for offset in [0xc, 0xd, 0xe] {
            sb(m, prim + offset, far);
        }
        let x0 = px.wrapping_sub(cx).wrapping_add(0x53);
        let y0 = (cy.wrapping_sub(py) << 1).wrapping_add(0xc0);
        let x1 = qx.wrapping_sub(cx).wrapping_add(0x53);
        let y1 = (cy.wrapping_sub(qy) << 1).wrapping_add(0xc0);
        sh(m, prim + 0xa, y0);
        sh(m, prim + 8, x0);
        sh(m, prim + 0x10, x1);
        sh(m, prim + 0x12, y1);
        add_prim(m, ot, prim);
        cursor = cursor.wrapping_add(0x14);
    }
    let mode = cursor;
    draw_mode(m, frame, mode, ot, 0x20);
    m.pop_frame(0x30);
    mode.wrapping_add(0xc)
}

/// One textured map quad of 800D29D8 (POLY_FT4, code 0x2C, texture page
/// 0x1F, CLUT 0x603C, neutral color) with texture rows `v`..`v + 0x40`.
fn map_quad(m: &mut Machine, prim: u32, ot: u32, v: u32, corners: [(u32, u32); 4]) {
    sb(m, prim + 3, 9);
    sb(m, prim + 7, 0x2c);
    let uv = [(0, v), (0x80, v), (0, v + 0x40), (0x80, v + 0x40)];
    for (k, (u, v)) in uv.into_iter().enumerate() {
        sb(m, prim + 0xc + 8 * k as u32, u);
        sb(m, prim + 0xd + 8 * k as u32, v);
    }
    sh(m, prim + 0x16, 0x1f);
    sh(m, prim + 0xe, 0x603c);
    for offset in [4, 5, 6] {
        sb(m, prim + offset, 0x80);
    }
    for (k, (x, y)) in corners.into_iter().enumerate() {
        sh(m, prim + 8 + 8 * k as u32, x);
        sh(m, prim + 0xa + 8 * k as u32, y);
    }
    add_prim(m, ot, prim);
}

/// 800D29D8: the map picture (two 128x64 textured quads) scrolled to the
/// display clock's track sample. Returns the next primitive.
pub fn draw_map(m: &mut Machine, prim: u32, ot: u32) -> u32 {
    let index = track_index(m);
    let (x, y) = sample(m, index);
    let left = 0x54u32.wrapping_sub(x);
    let right = 0x154u32.wrapping_sub(x);
    let dy = 0xffu32.wrapping_sub(y) << 1;
    let top = 0xbeu32.wrapping_sub(dy);
    let middle = 0x1beu32.wrapping_sub(dy);
    let bottom = 0x2beu32.wrapping_sub(dy);
    map_quad(
        m,
        prim,
        ot,
        0,
        [(left, top), (right, top), (left, middle), (right, middle)],
    );
    map_quad(
        m,
        prim.wrapping_add(0x28),
        ot,
        0x40,
        [
            (left, middle),
            (right, middle),
            (left, bottom),
            (right, bottom),
        ],
    );
    prim.wrapping_add(0x50)
}

/// 800D2BC8: the results picture: the event count (800D1BA4), the kind
/// lists, the time, then inside the map window (DR_AREA 0 … 1, linked
/// last-first) the markers, trail and map, and the title (by language,
/// font 0x12 at (0x14, 0x20)). Returns the next primitive.
pub fn draw_results(m: &mut Machine, cursor: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x30);
    tally(m);
    let next = draw_kinds(m, cursor, ot);
    let next = draw_time(m, next, ot);
    let next = draw_area(m, next, 0);
    let next = draw_markers(m, next, ot);
    let next = draw_trail(m, next, ot);
    let next = draw_map(m, next, ot);
    let next = draw_area(m, next, 1);
    let ctx = context(m);
    let language = m.lw(ctx + 0x38c);
    m.sw(frame + 0x10, 0x14);
    m.sw(frame + 0x14, 0x20);
    m.sw(frame + 0x18, 0x0080_8080);
    let title = m.lw(TITLES.wrapping_add(language << 2));
    let next = external(m, DRAW_TEXT, [0x12, title, ot, next]);
    m.pop_frame(0x30);
    next
}

/// 800D2C98 (state 11, step 0): reset the results block, slot 2 at 10 and
/// slot 1 at 0, loader page 2, styles 1 = 1 and 2 = 0; the work block +0xC
/// keeps the flown mission and context +0x390 takes the next one (+0x3C1,
/// or with context +0x970 bit 4 the first mission of the flown one's
/// category); record the results (800D191C); point `[0x800F942C]` at the
/// flight record and move the map pictures (MoveImage of the rectangles at
/// 0x800CB120/0x800CB128 to (960, 256)/(960, 384), each with DrawSync).
/// Returns 1.
pub fn results_open(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x28);
    let block = results(m);
    sh(m, block + 0x12, 0);
    sh(m, block + 0x10, 0);
    m.sw(block + 0x14, 0);
    m.sw(block, 0);
    m.sw(block + 0xc, 0);
    m.sw(block + 8, u32::MAX);
    m.sw(block + 4, 0);
    external(m, SET_VISUAL_PARAMETER, [2, 0xa, 0, 0]);
    external(m, SET_VISUAL_PARAMETER, [1, 0, 0, 0]);
    let block = results(m);
    m.sw(block + 0x18, 0);
    external(m, SET_PAGE, [2, 0, 0, 0]);
    external(m, SET_VISUAL_STYLE, [1, 1, 0, 0]);
    external(m, SET_VISUAL_STYLE, [2, 0, 0, 0]);
    let ctx = context(m);
    let hub = work(m);
    let mission = lh(m, ctx + 0x390);
    m.sw(hub + 0xc, mission);
    if m.lw(ctx + 0x970) & 0x10 == 0 {
        let next = lbu(m, ctx + 0x3c1);
        sh(m, ctx + 0x390, next);
    } else {
        let mission = lh(m, ctx + 0x390);
        let category = external(m, MISSION_CATEGORY, [mission, 0, 0, 0]);
        let next = mission_of(m, category);
        let ctx = context(m);
        sh(m, ctx + 0x390, next);
    }
    record_results(m);
    for k in 0..0x10 {
        let byte = lbu(m, MOVED_RECTS + k);
        sb(m, frame + 0x10 + k, byte);
    }
    let ctx = context(m);
    m.sw(RECORD, ctx + 0x3bc);
    external(m, MOVE_IMAGE, [frame + 0x10, 0x3c0, 0x100, 0]);
    external(m, DRAW_SYNC, [0; 4]);
    external(m, MOVE_IMAGE, [frame + 0x18, 0x3c0, 0x180, 0]);
    external(m, DRAW_SYNC, [0; 4]);
    m.pop_frame(0x28);
    1
}

/// The result texts of 800D2E18 once loading is done: helps 0xCC/0xCD,
/// style 2 = the rank, the story section (context +0x95A) paged: a line
/// every 0x5F frames, two per page, 10 frames between pages; the page's
/// lines in font 0x11 at (0x11, 0x17C + 0x14k). CROSS or TRIANGLE ends the
/// screen (style 1 = 0, fade timer 10): `true`.
fn result_texts(m: &mut Machine, frame: u32) -> bool {
    external(m, CLEAR_TEXT, [0; 4]);
    external(m, SHOW_TEXT, [0xcc, 1, 0, 0]);
    external(m, SHOW_TEXT, [0xcd, 1, 0, 0]);
    let ctx = context(m);
    let rank = lbu(m, ctx + 0x3c7);
    external(m, SET_VISUAL_STYLE, [2, rank, 0, 0]);
    let texts = story_texts(m);
    let lines = if m.lw(texts) != 0 {
        let ctx = context(m);
        let story = lbu(m, ctx + 0x95a);
        text_line_count(m, story, texts)
    } else {
        u32::MAX
    };
    let block = results(m);
    m.sw(block + 8, lines);
    if m.lw(block + 8) == u32::MAX {
        let flags = m.lw(block) | 3;
        m.sw(block, flags);
    }
    let flags = m.lw(block);
    if flags & 1 == 0 {
        let timer = m.lw(block + 4);
        if flags & 2 != 0 {
            if (timer as i32) < 0xa {
                m.sw(block + 4, timer.wrapping_add(1));
            } else {
                let line = m.lw(block + 0xc).wrapping_add(1);
                m.sw(block + 0xc, line);
                let flags = m.lw(block);
                m.sw(block + 4, 0);
                m.sw(block, flags & !2);
            }
        } else if (timer as i32) < 0x5f {
            m.sw(block + 4, timer.wrapping_add(1));
        } else {
            let lines = m.lw(block + 8);
            let line = m.lw(block + 0xc);
            m.sw(block + 4, 0);
            if (line as i32) < (lines.wrapping_sub(1) as i32) {
                if half(line) << 1 == line.wrapping_sub(1) {
                    let flags = m.lw(block) | 2;
                    m.sw(block, flags);
                } else {
                    m.sw(block + 0xc, line.wrapping_add(1));
                }
            } else {
                let flags = m.lw(block) | 1;
                m.sw(block, flags);
            }
        }
    }
    let block = results(m);
    if m.lw(block) & 2 == 0 {
        let line = m.lw(block + 0xc);
        let odd = line.wrapping_sub(half(line) << 1);
        let count = odd.wrapping_add(1);
        let first = line.wrapping_sub(odd);
        let mut k = 0u32;
        while (k as i32) < (count as i32) {
            let ctx = context(m);
            let story = lbu(m, ctx + 0x95a);
            let texts = story_texts(m);
            let text = text_line(m, story, first.wrapping_add(k), texts);
            m.sw(frame + 0x10, 0x11);
            m.sw(frame + 0x14, 0x17c + 0x14 * k);
            m.sw(frame + 0x18, 0x0080_8080);
            let ot = ot_slot(m);
            let at = cursor(m);
            let next = external(m, DRAW_TEXT, [0x11, text, ot, at]);
            set_cursor(m, next);
            k = k.wrapping_add(1);
        }
    }
    let ctx = context(m);
    if m.lw(ctx + 0x12c) & 0x30 == 0 {
        return false;
    }
    external(m, SET_VISUAL_STYLE, [1, 0, 0, 0]);
    let block = results(m);
    m.sw(block + 0x18, 0xa);
    true
}

/// 800D2E18 (state 11, step 1): one loader callback (table 0x800F39F4 by
/// phase +0x10 and substate +0x12; 1 next substate, 2 next phase, 4 done:
/// the result texts, whose CROSS/TRIANGLE returns 1), then the results
/// picture at the work block's cursor and the display clock's tick (up to
/// the flight time / 30). Returns 0.
pub fn results_tick(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x40);
    let result = results_tick_body(m, frame);
    m.pop_frame(0x40);
    result
}

fn results_tick_body(m: &mut Machine, frame: u32) -> u32 {
    let block = results(m);
    let phase = lh(m, block + 0x10);
    let list = m.lw(CALLBACKS.wrapping_add(phase << 2));
    let step = lh(m, block + 0x12);
    let routine = m.lw(list.wrapping_add(step << 2));
    let result = call_pointer(m, routine, [block, 0, 0, 0]);
    if result & 1 != 0 {
        let block = results(m);
        let step = lhu(m, block + 0x12).wrapping_add(1);
        sh(m, block + 0x12, step);
    } else if result & 2 != 0 {
        let block = results(m);
        let phase = lhu(m, block + 0x10);
        sh(m, block + 0x12, 0);
        sh(m, block + 0x10, phase.wrapping_add(1));
    } else if result & 4 != 0 && result_texts(m, frame) {
        return 1;
    }
    let at = cursor(m);
    let ot = ot_slot(m);
    let next = draw_results(m, at, ot);
    set_cursor(m, next);
    let ctx = context(m);
    let limit = div30(lhu(m, ctx + 0x3be));
    let block = results(m);
    let clock = m.lw(block + 0x14);
    if (clock as i32) < (limit as i32) {
        m.sw(block + 0x14, clock.wrapping_add(1));
    }
    0
}

/// 800D31FC (state 11, step 2): slot 2 at the fade timer +0x18, counting
/// down; 0x100 at 0 (state 12), else 0.
pub fn results_close(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = results(m);
    let fade = m.lw(block + 0x18);
    external(m, SET_VISUAL_PARAMETER, [2, fade, 0, 0]);
    let block = results(m);
    let fade = m.lw(block + 0x18);
    let result = if (fade as i32) > 0 {
        m.sw(block + 0x18, fade.wrapping_sub(1));
        0
    } else {
        0x100
    };
    m.pop_frame(0x18);
    result
}
