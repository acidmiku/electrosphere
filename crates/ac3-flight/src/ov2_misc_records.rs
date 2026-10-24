//! Overlay 2 inner states 7 and 8 (hub choice 1 / free-mission hub choice
//! 1): the mission records screen. Block `[0x800F6034]`: +0 flags (1: free
//! mission, all 0x24 missions; 2: image reload pending), +4 difficulty,
//! +6 mission, +8 last mission, +0xC/+0xE cleared counts, +0x10 panel
//! timer. Records live in the context from +0x960. See
//! [`crate::ov2_misc`] for conventions.
use crate::machine::Machine;
use crate::ov2_aircraft::div_magic;
use crate::ov2_aircraft::{CLEAR_LABELS, Calls, LABEL, OFFSET, PAGE, SCENE, SOUND, STYLE, TEXT};
use crate::ov2_misc::{
    LOAD_RESOURCE, MISSION_CATEGORY, MISSION_RECORD, SPRINTF, STRCAT, STRLEN, context,
};

/// 0x800F6034: pointer to the records block.
pub const RECORDS: u32 = 0x800f_6034;
/// 800F17C0 (`ov2_hub`): draw a text string pair.
pub const TEXT_PAIR: u32 = 0x800f_17c0;
/// 800E983C / 800E9824 (`ov2_core`): background object phases.
pub const BACKGROUND_PHASE: u32 = 0x800e_983c;
pub const BACKGROUND_RESET: u32 = 0x800e_9824;
/// 800AC148 (resident SDK): memset.
pub const MEMSET: u32 = 0x800a_c148;
/// Overlay rodata formats: "%10d", "%2d/%2d", "--:--.--", "%2d:%2d.%2d",
/// "%3d:", "%2d:%2d", and the 4-byte string at 0x800CB1D8.
const FORMAT_SCORE: u32 = 0x800c_b1a0;
const FORMAT_COUNT: u32 = 0x800c_b1a8;
const NO_TIME: u32 = 0x800c_b1b0;
const FORMAT_TIME: u32 = 0x800c_b1bc;
const FORMAT_HOURS: u32 = 0x800c_b1c8;
const FORMAT_CLOCK: u32 = 0x800c_b1d0;
const PROMPT: u32 = 0x800c_b1d8;
const GREY: u32 = 0x0080_8080;
const MISSIONS: u32 = 0x800f_5958;

fn block(m: &mut Machine) -> u32 {
    m.lw(RECORDS)
}

fn style(m: &mut Machine, c: &mut dyn Calls, slot: u32, value: u32) {
    c.call(m, STYLE, [slot, value, 0, 0]);
}

fn label(m: &mut Machine, c: &mut dyn Calls, index: u32) {
    c.call(m, LABEL, [index, 1, 0, 0]);
}

fn offset(m: &mut Machine, c: &mut dyn Calls, slot: u32, x: u32, y: u32) {
    c.call(m, OFFSET, [slot, x, y, 0]);
}

/// 800D04C8 into the scene's current ordering table entry and primitive
/// area (stack arguments at `frame+0x10..`); the new primitive pointer goes
/// back to scene +0x20094.
pub(crate) fn text(
    m: &mut Machine,
    c: &mut dyn Calls,
    frame: u32,
    font: u32,
    string: u32,
    x: u32,
    y: u32,
) {
    let scene = m.lw(SCENE);
    m.sw(frame + 0x10, x);
    m.sw(frame + 0x14, y);
    m.sw(frame + 0x18, GREY);
    let ot = m.lw(scene + 0x90);
    let prim = m.lw(scene + 0x2_0094);
    let next = c.call(m, TEXT, [font, string, ot, prim]);
    let scene = m.lw(SCENE);
    m.sw(scene + 0x2_0094, next);
}

/// Replace every blank of the `length`-byte string at `at` by '0'.
fn zero_fill(m: &mut Machine, at: u32, length: u32) {
    let mut i = 0u32;
    while (i as i32) < (length as i32) {
        if m.lbu(at + i) == b' ' {
            m.sb(at + i, b'0');
        }
        i += 1;
    }
}

/// "%2d:%2d.%2d" of `time` (25 Hz frames: minutes, seconds, hundredths),
/// with blanks zero-filled.
fn format_time(m: &mut Machine, c: &mut dyn Calls, frame: u32, buffer: u32, time: u32) {
    let seconds = div_magic(time, 0x51eb_851f, 3, false);
    let fraction = time.wrapping_sub(seconds.wrapping_mul(25)) << 2;
    m.sw(frame + 0x10, fraction);
    let minutes = div_magic(time, 0x0576_19f1, 5, false);
    let seconds = seconds.wrapping_sub(minutes.wrapping_mul(60));
    c.call(m, SPRINTF, [buffer, FORMAT_TIME, minutes, seconds]);
    let length = c.call(m, STRLEN, [buffer, 0, 0, 0]);
    zero_fill(m, buffer, length);
}

/// 800DA9B8: draw the records page for panel offset `t` (+-t*t): totals,
/// clear counts, the selected mission's score, time and grade, and the
/// total play time.
pub fn draw_records_800da9b8(m: &mut Machine, c: &mut dyn Calls, t: u32) {
    let frame = m.push_frame(0x100);
    let b = block(m);
    let square = t.wrapping_mul(t);
    let mission = m.lh(b + 6) as i32 as u32;
    let records = context(m) + 0x960;
    let left = i32::from(square.wrapping_neg() as u16 as i16) as u32;
    let right = i32::from(square as u16 as i16) as u32;
    let index = c.call(m, MISSION_RECORD, [mission, 0, 0, 0]);
    c.call(m, CLEAR_LABELS, [0; 4]);
    let value = u32::from(m.lbu(records + 1));
    style(m, c, 1, value.wrapping_add(1));
    let b = block(m);
    let mut grade = 0u32;
    let mut time = 0u32;
    if m.lw(b) & 1 != 0 {
        let difficulty = m.lh(b + 4) as i32 as u32;
        let column = (difficulty << 2).wrapping_add(difficulty) << 3;
        let selected = m.lh(b + 6) as i32 as u32;
        style(m, c, 3, selected.wrapping_add(column));
        for i in 1..0x25u32 {
            let at = records + i;
            let value = m.lbu(at + 0x300);
            style(m, c, i + 0xa, u32::from(value));
            let value = m.lbu(at + 0x33c);
            style(m, c, i + 0x32, u32::from(value));
            let value = m.lbu(at + 0x378);
            style(m, c, i + 0x5a, u32::from(value));
        }
    } else {
        let selected = m.lh(b + 6) as i32 as u32;
        style(m, c, 3, selected);
        for i in 1..0x25u32 {
            let value = m.lbu(records + i + 0x3f0);
            style(m, c, i + 0xa, u32::from(value));
        }
    }
    let b = block(m);
    let difficulty = m.lh(b + 4) as i32 as u32;
    style(m, c, 2, difficulty.wrapping_add(1));
    label(m, c, 0xc6);
    for i in 0..3u32 {
        let b = block(m);
        if i == m.lh(b + 4) as i32 as u32 {
            label(m, c, 0xc2 + i);
        }
    }
    label(m, c, 0xbd);
    let buffer = frame + 0x70;
    let total = m.lw(records + 0x610);
    c.call(m, SPRINTF, [buffer, FORMAT_SCORE, total, 0]);
    let column = left.wrapping_add(0x82);
    text(m, c, frame, 6, buffer, column, 0x78);
    label(m, c, 0xbf);
    let total = m.lw(records + 0x614);
    c.call(m, SPRINTF, [buffer, FORMAT_SCORE, total, 0]);
    text(m, c, frame, 6, buffer, column, 0x8a);
    label(m, c, 0xbe);
    let b = block(m);
    let cleared = m.lh(b + 0xc) as i32 as u32;
    c.call(m, SPRINTF, [buffer, FORMAT_COUNT, cleared, 0x15]);
    let column = left.wrapping_add(0x9b);
    text(m, c, frame, 6, buffer, column, 0x9c);
    label(m, c, 0xc0);
    let b = block(m);
    let cleared = m.lh(b + 0xe) as i32 as u32;
    c.call(m, SPRINTF, [buffer, FORMAT_COUNT, cleared, 0xe]);
    text(m, c, frame, 6, buffer, column, 0xae);
    let b = block(m);
    if m.lw(b) & 1 != 0 {
        let difficulty = m.lh(b + 4) as i32;
        let selected = m.lh(b + 6) as i32 as u32;
        let marks = match difficulty {
            0 => Some(0x300),
            1 => Some(0x33c),
            2 => Some(0x378),
            _ => None,
        };
        if let Some(marks) = marks {
            grade = u32::from(m.lbu(records.wrapping_add(selected) + marks));
        }
    } else {
        label(m, c, 0xc8);
        let b = block(m);
        let selected = m.lh(b + 6) as i32 as u32;
        let score = m.lw(records.wrapping_add(selected << 2) + 0x42c);
        c.call(m, SPRINTF, [buffer, FORMAT_SCORE, score, 0]);
        let column = right.wrapping_add(0xa8);
        text(m, c, frame, 8, buffer, column, 0x112);
        label(m, c, 0xc9);
        let b = block(m);
        let selected = m.lh(b + 6) as i32 as u32;
        let score = m.lw(records.wrapping_add(selected << 2) + 0x51c);
        c.call(m, SPRINTF, [buffer, FORMAT_SCORE, score, 0]);
        text(m, c, frame, 8, buffer, column, 0x124);
        let b = block(m);
        let selected = m.lh(b + 6) as i32 as u32;
        time = u32::from(m.lhu(records.wrapping_add(selected << 1) + 0x288));
        label(m, c, 0xc7);
        let b = block(m);
        let selected = m.lh(b + 6) as i32 as u32;
        if m.lbu(records.wrapping_add(selected) + 0x3f0) == 0 {
            text(m, c, frame, 8, NO_TIME, right.wrapping_add(0xfe), 0xfa);
        } else {
            format_time(m, c, frame, buffer, time);
            text(m, c, frame, 8, buffer, right.wrapping_add(0xfe), 0xfa);
        }
        let b = block(m);
        let selected = m.lh(b + 6) as i32 as u32;
        grade = u32::from(m.lbu(records.wrapping_add(selected) + 0x3f0));
    }
    label(m, c, 0xc5);
    let name = m.lw(MISSIONS + index.wrapping_mul(12) + 8);
    text(m, c, frame, 7, name, right.wrapping_add(0xaa), 0xd6);
    label(m, c, 0xca);
    style(m, c, 4, grade);
    let b = block(m);
    let difficulty = m.lh(b + 4) as i32;
    if let Some((times, marks)) = match difficulty {
        0 => Some((0xa8, 0x300)),
        1 => Some((0x120, 0x33c)),
        2 => Some((0x198, 0x378)),
        _ => None,
    } {
        let selected = m.lh(b + 6) as i32 as u32;
        time = u32::from(m.lhu(records.wrapping_add(selected << 1) + times));
        grade = u32::from(m.lbu(records.wrapping_add(selected) + marks));
    }
    label(m, c, 0xc1);
    let string = if grade == 0 || grade == 5 {
        NO_TIME
    } else {
        format_time(m, c, frame, buffer, time);
        buffer
    };
    text(m, c, frame, 0xb, string, left.wrapping_add(0xf0), 0x9a);
    label(m, c, 0xcb);
    let b = block(m);
    let play = if m.lw(b) & 1 != 0 {
        m.lw(records + 0x618)
    } else {
        m.lw(records + 0x60c)
    };
    let hours = div_magic(play, 0x5d34_edef, 15, false);
    let line = frame + 0x20;
    c.call(m, SPRINTF, [line, FORMAT_HOURS, hours, 0]);
    let minutes_total = div_magic(play, 0x0576_19f1, 5, false);
    let minutes = minutes_total.wrapping_sub(hours.wrapping_mul(60));
    let seconds =
        div_magic(play, 0x51eb_851f, 3, false).wrapping_sub(minutes_total.wrapping_mul(60));
    c.call(m, SPRINTF, [buffer, FORMAT_CLOCK, minutes, seconds]);
    let length = c.call(m, STRLEN, [buffer, 0, 0, 0]);
    zero_fill(m, buffer, length);
    c.call(m, STRCAT, [line, buffer, 0, 0]);
    text(m, c, frame, 8, line, right.wrapping_add(0xa0), 0x168);
    m.pop_frame(0x100);
}

/// Side panels 2..4 at (0, -t*t), (t*t, 0), (-t*t, 0).
fn panels(m: &mut Machine, c: &mut dyn Calls, t: u32) {
    let negative = t.wrapping_neg().wrapping_mul(t);
    offset(m, c, 2, 0, negative);
    offset(m, c, 3, t.wrapping_mul(t), 0);
    offset(m, c, 4, negative, 0);
}

/// 800DB454: records entry. Panels off-screen (timer 25), sound 0x5A; in
/// free-mission mode (option 0x10) all 0x24 missions from the first at
/// difficulty 0 (page 9), otherwise the context mission's category and
/// difficulty (page 8); the two clear counts; the mission image (resource
/// +0x200A4). Returns 1.
pub fn records_init_800db454(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let frame = m.push_frame(0x20);
    let b = block(m);
    m.sw(b + 0x10, 25);
    offset(m, c, 2, 0, (-0x271i32) as u32);
    offset(m, c, 3, 0x271, 0);
    offset(m, c, 4, (-0x271i32) as u32, 0);
    c.call(m, SOUND, [0x5a, 0, 0, 0]);
    let ctx = context(m);
    let page = if m.lw(ctx + 0x970) & 0x10 != 0 {
        let b = block(m);
        m.sh(b + 6, 1);
        m.sw(b + 8, 0x24);
        m.sh(b + 4, 0);
        m.sw(b, 1);
        9
    } else {
        let b = block(m);
        let difficulty = m.lhu(ctx + 0x398);
        m.sh(b + 4, difficulty);
        let mission = m.lh(ctx + 0x390) as i32 as u32;
        let category = c.call(m, MISSION_CATEGORY, [mission, 0, 0, 0]);
        let b = block(m);
        m.sh(b + 6, category as u16);
        m.sw(b + 8, i32::from(category as u16 as i16) as u32);
        m.sw(b, 0);
        8
    };
    c.call(m, PAGE, [page, 0, 0, 0]);
    let ctx = context(m);
    let b = block(m);
    let progress = ctx + 0x960;
    m.sh(b + 0xc, 0);
    for i in 0..0x40 {
        if m.lbu(progress + i + 0x18) == 1 {
            let count = m.lhu(b + 0xc);
            m.sh(b + 0xc, count.wrapping_add(1));
        }
    }
    let b = block(m);
    m.sh(b + 0xe, 0);
    for i in 0..0x10 {
        if m.lbu(progress + i + 0x98) == 1 {
            let count = m.lhu(b + 0xe);
            m.sh(b + 0xe, count.wrapping_add(1));
        }
    }
    load_mission_image(m, c, frame);
    m.pop_frame(0x20);
    1
}

/// The selected mission's image: 800CE978(scene +0x200A4, mission - 1,
/// 0x1480, no rectangle, no wait).
fn load_mission_image(m: &mut Machine, c: &mut dyn Calls, frame: u32) {
    let b = block(m);
    let mission = m.lh(b + 6) as i32 as u32;
    let scene = m.lw(SCENE);
    m.sw(frame + 0x10, 0);
    let table = m.lw(scene + 0x2_00a4);
    c.call(
        m,
        LOAD_RESOURCE,
        [table, mission.wrapping_sub(1), 0x1480, 0],
    );
}

/// 800DB618: panels slide in while timer +0x10 counts down. Returns 1 when
/// done.
pub fn records_intro_800db618(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let b = block(m);
    let t = m.lw(b + 0x10);
    panels(m, c, t);
    draw_records_800da9b8(m, c, t);
    c.call(m, BACKGROUND_PHASE, [0; 4]);
    let result = if (t as i32) > 0 {
        let b = block(m);
        let timer = m.lw(b + 0x10);
        m.sw(b + 0x10, timer.wrapping_sub(1));
        0
    } else {
        1
    };
    m.pop_frame(0x20);
    result
}

/// 800DB6CC: records input. Cancel (0x20) plays 0x3B, 0x5B and returns 1.
/// Repeat right/left (+0x130 bits 8/4) step the mission within 1..+8;
/// in free-mission mode pressed bits 2/1 step the difficulty within 0..2;
/// each step plays 0x3D; a mission change reloads its image.
pub fn records_input_800db6cc(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let frame = m.push_frame(0x30);
    draw_records_800da9b8(m, c, 0);
    let first = frame + 0x18;
    let second = frame + 0x20;
    let prompt = m.lw(PROMPT);
    m.sw(first, prompt);
    c.call(m, MEMSET, [second, 0, 4, 0]);
    c.call(m, TEXT_PAIR, [0x78, first, second, 0]);
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    if pressed & 0x20 != 0 {
        c.call(m, SOUND, [0x3b, 0, 0, 0]);
        c.call(m, SOUND, [0x5b, 0, 0, 0]);
        m.pop_frame(0x30);
        return 1;
    }
    let repeat = m.lw(ctx + 0x130);
    let b = block(m);
    let step = if repeat & 8 != 0 {
        let mission = m.lh(b + 6) as i32;
        let last = m.lw(b + 8) as i32;
        if mission < last {
            m.sh(b + 6, (mission as u16).wrapping_add(1));
            let flags = m.lw(b);
            m.sw(b, flags | 2);
            true
        } else {
            false
        }
    } else if repeat & 4 != 0 {
        let mission = m.lh(b + 6) as i32;
        if mission >= 2 {
            m.sh(b + 6, (mission as u16).wrapping_sub(1));
            let flags = m.lw(b);
            m.sw(b, flags | 2);
            true
        } else {
            false
        }
    } else if m.lw(b) & 1 != 0 {
        // The difficulty follows the pressed (not repeat) bits.
        let difficulty = m.lh(b + 4) as i32;
        if pressed & 2 != 0 {
            if difficulty < 2 {
                m.sh(b + 4, (difficulty as u16).wrapping_add(1));
                true
            } else {
                false
            }
        } else if pressed & 1 != 0 && difficulty > 0 {
            m.sh(b + 4, (difficulty as u16).wrapping_sub(1));
            true
        } else {
            false
        }
    } else {
        false
    };
    if step {
        c.call(m, SOUND, [0x3d, 0, 0, 0]);
    }
    let b = block(m);
    if m.lw(b) & 2 != 0 {
        load_mission_image(m, c, frame);
        let b = block(m);
        let flags = m.lw(b);
        m.sw(b, flags & !2);
    }
    m.pop_frame(0x30);
    0
}

/// 800DB8C8: panels slide out over 25 updates; then 800E9824 and 0x200.
pub fn records_exit_800db8c8(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let b = block(m);
    let t = m.lw(b + 0x10);
    panels(m, c, t);
    draw_records_800da9b8(m, c, t);
    let result = if (t as i32) >= 25 {
        c.call(m, BACKGROUND_RESET, [0; 4]);
        0x200
    } else {
        let b = block(m);
        let timer = m.lw(b + 0x10);
        m.sw(b + 0x10, timer.wrapping_add(1));
        0
    };
    m.pop_frame(0x20);
    result
}
