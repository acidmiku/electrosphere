//! JP overlay 2, part 2: the hub's stick and sound configuration pages
//! (hub update modes 15 and 19..23), translated from the JP instructions.
//!
//! Sound settings live in the context: +0x398 effect level, +0x399 music
//! level, +0x39A bit 0 the output mode. The sound page keeps the cursor
//! at state +0x100 (0 mode, 1 effects, 2 music) and the levels on entry at
//! +0x101..+0x103; its bars are drawn from the vertex record at state
//! +0x12A (800F7180 of part 5). Leaving restarts sounds 0x5D..0x61 and
//! keeps their channels in visual +0x130..+0x140 (stopped by mode 22).
use super::ov2_p2::{MODULE, framed, lbu, lh, visual};
use super::ov2_p2_hub::STATE;
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch;
use crate::machine::{Machine, SmallData};

/// 8002DB4C (resident, `script`): start sound effect `a0`; returns the
/// channel.
const SOUND: u32 = 0x8002_db4c;
/// 8002DDA8 (resident, `shared`): stop sound channel `a0`.
const STOP: u32 = 0x8002_dda8;
/// 8002BD28 / 8002BD5C (resident, `mainloop`): output mode `a0`, apply.
const OUTPUT_MODE: u32 = 0x8002_bd28;
const APPLY_MODE: u32 = 0x8002_bd5c;
/// 800ECDA8 (overlay 2, part 4): release image `a0`.
const RELEASE: u32 = 0x800e_cda8;
/// Overlay 2, part 5: bar (vertex record, packet, ordering table + 8),
/// output-mode indicator value, page title (a0, a1), help text (a0).
const BAR: u32 = 0x800f_7180;
const MODE_VALUE: u32 = 0x800f_77a4;
const TITLE: u32 = 0x800f_7524;
const HELP: u32 = 0x800f_7514;

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}
fn sound(m: &mut Machine, id: u32) -> u32 {
    dispatch::call(m, SOUND, [id, 0, 0, 0])
}
fn pad(m: &mut Machine, offset: u32) -> u32 {
    let context = m.gw(CONTEXT);
    m.lw(context.wrapping_add(offset))
}
fn press(m: &mut Machine, bit: u32, held: u32) -> bool {
    let context = m.gw(CONTEXT);
    m.lw(context.wrapping_add(0x12c)) & bit != 0 && m.lw(context.wrapping_add(0x128)) & held == 0
}

/// 800DA444 (hub update mode 15): the stick screens (selection 1:
/// [`stick_position`](super::ov2_p2_screen::stick_position), 3:
/// [`stick_calibration`](super::ov2_p2_screen::stick_calibration)); a pad
/// change (context +0x156 = 2, or +0x124 differing from state +0xE8)
/// returns to page 1. Returns the state record (re-read after the stores)
/// or, with no change, state word +0xE8.
pub fn stick_page(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let record = state(m);
        if lh(m, record.wrapping_add(0xc)) == 1 {
            super::ov2_p2_screen::stick_position(m);
        }
        let record = state(m);
        if lh(m, record.wrapping_add(0xc)) == 3 {
            super::ov2_p2_screen::stick_calibration(m);
        }
        let context = m.gw(CONTEXT);
        if lh(m, context.wrapping_add(0x156)) != 2 {
            let record = state(m);
            let option = m.lw(context.wrapping_add(0x124));
            let saved = m.lw(record.wrapping_add(0xe8));
            if option == saved {
                return saved;
            }
        }
        let record = state(m);
        m.sb(record, 1);
        let record = state(m);
        m.sb(record.wrapping_add(1), 0);
        record
    })
}

/// 800DA4F4 (hub update mode 19): open the sound page: release image
/// [visual +0xA0], step to 1, reset cell 0xC of list 2, words 1 / 0x1000
/// into slots 0 / 2, cursor 0, the current levels into state +0x101..,
/// flags 5 (state +6) and 6, fade slot 1 in and help text 0x8010AC60.
/// Returns the help routine's value.
pub fn open_sound(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = visual(m);
        let image = m.lw(block.wrapping_add(0xa0));
        dispatch::call(m, RELEASE, [image, 0, 0, 0]);
        let record = state(m);
        m.sb(record.wrapping_add(1), 1);
        super::ov2_p2::reset_cell(m, 2, 0xc);
        super::ov2_p2::set_word_178(m, 0, 1);
        super::ov2_p2::set_word_178(m, 2, 0x1000);
        let record = state(m);
        m.sb(record.wrapping_add(0x100), 0);
        for (from, to, mask) in [
            (0x39a, 0x101, 1),
            (0x398, 0x102, 0xff),
            (0x399, 0x103, 0xff),
        ] {
            let context = m.gw(CONTEXT);
            let value = lbu(m, context.wrapping_add(from)) & mask;
            let record = state(m);
            m.sb(record.wrapping_add(to), value as u8);
        }
        let record = state(m);
        m.sb(record.wrapping_add(3), 0);
        let record = state(m);
        let value = lbu(m, record.wrapping_add(6));
        super::ov2_p2::set_flag(m, 5, value);
        super::ov2_p2::set_flag(m, 6, 0);
        super::ov2_p2::set_fade(m, 1, 1, 0, 0x1e);
        dispatch::call(m, HELP, [0x8010_ac60, 0, 0, 0])
    })
}

/// 800DA5EC (hub update mode 20): confirm (pad 0x10) fades slot 1 out;
/// once it has finished, step to 2. Returns 2, or the state record.
pub fn wait_sound_fade(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if press(m, 0x10, 0x2f) {
            super::ov2_p2::set_fade(m, 1, 2, 0x1e, 0x1e);
        }
        let mode = super::ov2_p2::fade_mode(m, 1);
        if mode != 2 {
            return 2;
        }
        let record = state(m);
        m.sb(record.wrapping_add(1), mode as u8);
        record
    })
}

/// 800DA66C (hub update mode 21): the page choice (state +6, 0..3) with
/// the repeated pad bits 2 / 1 (sound 0x3D); confirm fades slot 1 and
/// steps to 3 (choice made, sounds 0x3C and 0x5B), cancel (0x20) to 3
/// with +3 = 2 (sound 0x3B); flags 5 (the choice) and 6 (held bits 1 / 2).
/// Returns the cancel test's value (0, the held bits & 0x5F) or, after a
/// cancel, the context pointer.
pub fn choose_page(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        if pad(m, 0x130) & 2 != 0 {
            let record = state(m);
            let choice = lbu(m, record.wrapping_add(6));
            let next = if choice < 3 { choice + 1 } else { 0 };
            m.sb(record.wrapping_add(6), next as u8);
            sound(m, 0x3d);
        }
        if pad(m, 0x130) & 1 != 0 {
            let record = state(m);
            let choice = lbu(m, record.wrapping_add(6));
            let next = if choice != 0 { choice - 1 } else { 3 };
            m.sb(record.wrapping_add(6), next as u8);
            sound(m, 0x3d);
        }
        if press(m, 0x10, 0x2f) {
            super::ov2_p2::set_fade(m, 1, 3, u32::MAX, u32::MAX);
            let record = state(m);
            m.sb(record.wrapping_add(1), 3);
            let record = state(m);
            m.sb(record.wrapping_add(3), 1);
            sound(m, 0x3c);
            sound(m, 0x5b);
        }
        let record = state(m);
        let choice = lbu(m, record.wrapping_add(6));
        super::ov2_p2::set_flag(m, 5, choice);
        super::ov2_p2::set_flag(m, 6, 0);
        if pad(m, 0x128) & 1 != 0 {
            super::ov2_p2::set_flag(m, 6, 1);
        }
        if pad(m, 0x128) & 2 != 0 {
            super::ov2_p2::set_flag(m, 6, 2);
        }
        let pressed = pad(m, 0x12c) & 0x20;
        if pressed == 0 {
            return 0;
        }
        let held = pad(m, 0x128) & 0x5f;
        if held != 0 {
            return held;
        }
        super::ov2_p2::set_fade(m, 1, 3, u32::MAX, u32::MAX);
        let record = state(m);
        m.sb(record.wrapping_add(1), 3);
        let record = state(m);
        m.sb(record.wrapping_add(3), 2);
        sound(m, 0x3b);
        let context = m.gw(CONTEXT);
        m.sw(context.wrapping_add(0x12c), 0);
        context
    })
}

/// 800DA870 (hub update mode 22): after slot 1 has faded out, a made
/// choice (+3 = 1) opens its page: 0 sound settings (step 4, cell 0xD,
/// slot 2 = 0x2000, stop the five channels), 1 step 5 (cell 0xE, 0x4000),
/// 2 step 6 (cell 0xF, slots 0 / 2 = 0 / 0x8000, [gp+0x2D4] +8/+0xC into
/// state +0x106/+0x108), 3 step 7; a cancel (+3 = 2) returns to page 0.
/// Returns the original's last `v0`: the fade mode while fading, the last
/// call's value or stored halfword of an opened page, 7 for choices 3 and
/// up, the state record after a cancel, else 2.
pub fn open_choice(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        if press(m, 0x10, 0x2f) {
            super::ov2_p2::set_fade(m, 1, 0, 0, 0);
        }
        let mode = super::ov2_p2::fade_mode(m, 1);
        if mode != 0 {
            return mode;
        }
        let record = state(m);
        match lbu(m, record.wrapping_add(3)) {
            1 => match lbu(m, record.wrapping_add(6)) {
                0 => {
                    m.sb(record.wrapping_add(1), 4);
                    super::ov2_p2::reset_cell(m, 2, 0xd);
                    super::ov2_p2::set_word_178(m, 2, 0x2000);
                    let mut result = 0;
                    for offset in [0x130, 0x134, 0x138, 0x13c, 0x140] {
                        let block = visual(m);
                        let channel = m.lw(block.wrapping_add(offset));
                        result = dispatch::call(m, STOP, [channel, 0, 0, 0]);
                    }
                    result
                }
                1 => {
                    m.sb(record.wrapping_add(1), 5);
                    super::ov2_p2::reset_cell(m, 2, 0xe);
                    super::ov2_p2::set_word_178(m, 2, 0x4000)
                }
                2 => {
                    m.sb(record.wrapping_add(1), 6);
                    super::ov2_p2::reset_cell(m, 2, 0xf);
                    super::ov2_p2::set_word_178(m, 0, 0);
                    super::ov2_p2::set_word_178(m, 2, 0x8000);
                    let source = m.gw(0x2d4);
                    let record = state(m);
                    let x = m.lhu(source.wrapping_add(8));
                    m.sh(record.wrapping_add(0x106), x);
                    let y = m.lhu(source.wrapping_add(0xc));
                    m.sh(record.wrapping_add(0x108), y);
                    u32::from(y)
                }
                choice => {
                    if choice == 3 {
                        m.sb(record.wrapping_add(1), 7);
                    }
                    7
                }
            },
            2 => {
                super::ov2_p2::set_cell_preset(m, 0, 1);
                let record = state(m);
                m.sb(record, 0);
                let record = state(m);
                m.sb(record.wrapping_add(1), 0);
                record
            }
            _ => 2,
        }
    })
}

/// `value * 221 / 255` as compiled (signed magic 0x80808081, shift 7).
fn level_width(value: u32) -> u32 {
    let n = value.wrapping_mul(221);
    let high = ((i64::from(n as i32) * i64::from(0x8080_8081_u32 as i32)) >> 32) as u32;
    ((high.wrapping_add(n) as i32 >> 7) as u32).wrapping_sub((n as i32 >> 31) as u32)
}

/// Draw the bar record at state +0x12A (800F7180) into ordering table
/// [visual +8] + `slot`; the new packet goes to state +8.
fn draw_bar(m: &mut Machine, slot: u32) {
    let record = state(m);
    let vis = visual(m);
    let bar = record.wrapping_add(0x12a);
    let ot = m.lw(vis.wrapping_add(8)).wrapping_add(slot);
    let packet = m.lw(record.wrapping_add(8));
    let next = dispatch::call(m, BAR, [bar, packet, ot, 0]);
    let record = state(m);
    m.sw(record.wrapping_add(8), next);
}

/// The two parts of a level bar: the frame at `y` (cursor on it when the
/// cursor is `item`: flag 0x32 = `item`, highlighted colors) and the fill
/// of `level`.
fn level_bar(m: &mut Machine, y: u32, item: u32, level: u32) {
    let width = level_width(lbu(m, level));
    let record = state(m);
    let current = if item == 1 {
        m.sh(record.wrapping_add(0x12c), y as u16);
        for offset in [0x134, 0x133, 0x132] {
            m.sb(record.wrapping_add(offset), 0);
        }
        state(m)
    } else {
        for offset in [0x134, 0x133, 0x132] {
            m.sb(record.wrapping_add(offset), 0);
        }
        let current = state(m);
        m.sh(record.wrapping_add(0x12c), y as u16);
        current
    };
    m.sh(record.wrapping_add(0x12a), width.wrapping_add(0x37) as u16);
    m.sh(current.wrapping_add(0x12e), 2);
    m.sh(current.wrapping_add(0x130), 0x40);
    if item == 1 {
        m.sb(current.wrapping_add(0x135), 0);
    }
    draw_bar(m, 8);
    let record = state(m);
    let cursor = lbu(m, record.wrapping_add(0x100));
    m.sh(record.wrapping_add(0x12a), 0x38);
    m.sh(record.wrapping_add(0x12c), y as u16);
    if cursor == item {
        m.sb(record.wrapping_add(0x132), 0xcf);
        let record = state(m);
        m.sb(record.wrapping_add(0x133), 0x8b);
        let record = state(m);
        m.sb(record.wrapping_add(0x134), 0x17);
        super::ov2_p2::set_flag(m, 0x32, item);
    } else {
        m.sb(record.wrapping_add(0x132), 0xe5);
        let record = state(m);
        m.sb(record.wrapping_add(0x133), 0xe5);
        let record = state(m);
        m.sb(record.wrapping_add(0x134), 0xd5);
    }
    let value = lbu(m, level).wrapping_mul(221);
    let width = if item == 1 {
        let high = ((i64::from(value as i32) * i64::from(0x8080_8081_u32 as i32)) >> 32) as u32;
        ((high.wrapping_add(value) as i32 >> 7) as u32).wrapping_sub((value as i32 >> 31) as u32)
    } else {
        ((u64::from(value) * 0x8080_8081) >> 32) as u32 >> 7
    };
    let record = state(m);
    m.sh(record.wrapping_add(0x130), 0x40);
    m.sh(record.wrapping_add(0x12e), width as u16);
    draw_bar(m, 8);
}

/// Leave the sound page: the effect and music levels (and on cancel the
/// output mode) restored or kept, fade slot 1 in, step 1, word 0x1000 into
/// slot 2, sound `id`, flags 5/6, and sounds 0x5D..0x61 restarted into
/// visual +0x130..+0x140.
fn leave_sound(m: &mut Machine, id: u32) {
    super::ov2_p2::set_fade(m, 1, 1, 0, 0x1e);
    let record = state(m);
    m.sb(record.wrapping_add(1), 1);
    super::ov2_p2::set_word_178(m, 2, 0x1000);
    sound(m, id);
    let record = state(m);
    let value = lbu(m, record.wrapping_add(6));
    super::ov2_p2::set_flag(m, 5, value);
    super::ov2_p2::set_flag(m, 6, 0);
    let mut channel = sound(m, 0x5d);
    for (offset, next) in [(0x130, 0x5e), (0x134, 0x5f), (0x138, 0x60), (0x13c, 0x61)] {
        let block = visual(m);
        m.sw(block.wrapping_add(offset), channel);
        channel = sound(m, next);
    }
    let block = visual(m);
    m.sw(block.wrapping_add(0x140), channel);
}

/// 800DAA6C (hub update mode 23): the sound settings page: apply the
/// output mode (context +0x39A bit 0) and flags 6/7, draw the effect and
/// music level bars and the output-mode indicator, move the cursor
/// (repeated pad 2 / 1) and change the selected setting (8 / 4: toggle
/// the mode, levels in steps of 0x10 with sound 0x2E), confirm keeps the
/// settings and cancel restores them (both leave the page); then the page
/// title (800F7524 with 1, or 3 after leaving). Returns its value.
pub fn sound_settings(m: &mut Machine) -> u32 {
    framed(m, 0x38, |m, _| {
        let context = m.gw(CONTEXT);
        let mode_at = context.wrapping_add(0x39a);
        let effects = context.wrapping_add(0x398);
        let music = context.wrapping_add(0x399);
        let mono = lbu(m, context.wrapping_add(0x39a)) & 1;
        let mut title = 1;
        for (flag, mode) in [(6, 0), (7, 1)] {
            let value = if mono == mode {
                dispatch::call(m, OUTPUT_MODE, [mode, 0, 0, 0]);
                dispatch::call(m, APPLY_MODE, [mode, 0, 0, 0]);
                let record = state(m);
                u32::from(lbu(m, record.wrapping_add(0x100)) != 0)
            } else {
                2
            };
            super::ov2_p2::set_flag(m, flag, value);
        }
        super::ov2_p2::set_flag(m, 0x32, 0);
        level_bar(m, 0xc2, 1, effects);
        level_bar(m, 0x10c, 2, music);
        let record = state(m);
        let value = dispatch::call(m, MODE_VALUE, [record, 0, 0, 0]);
        let scaled = mul_15(value);
        let record = state(m);
        for (offset, half) in [(0x12a, 0x32), (0x12c, 0xbd), (0x130, 0xc)] {
            m.sh(record.wrapping_add(offset), half);
        }
        m.sb(record.wrapping_add(0x132), 0xcf);
        m.sh(record.wrapping_add(0x12e), scaled as u16);
        let record = state(m);
        m.sb(record.wrapping_add(0x133), 0x8b);
        let record = state(m);
        m.sb(record.wrapping_add(0x134), 0x17);
        draw_bar(m, 4);
        input(m, mono, mode_at, effects, music);
        if press(m, 0x10, 0x2f) {
            let context = m.gw(CONTEXT);
            for (from, to, mask) in [
                (0x39a, 0x101, 1),
                (0x398, 0x102, 0xff),
                (0x399, 0x103, 0xff),
            ] {
                let value = lbu(m, context.wrapping_add(from)) & mask;
                let record = state(m);
                m.sb(record.wrapping_add(to), value as u8);
            }
            leave_sound(m, 0x3c);
            title = 3;
        }
        if press(m, 0x20, 0x5f) {
            let context = m.gw(CONTEXT);
            let record = state(m);
            let mode = lbu(m, context.wrapping_add(0x39a));
            let mode = if lbu(m, record.wrapping_add(0x101)) == 1 {
                mode | 1
            } else {
                mode & 0xfe
            };
            m.sb(context.wrapping_add(0x39a), mode as u8);
            for (from, to) in [(0x102, 0x398), (0x103, 0x399)] {
                let record = state(m);
                let context = m.gw(CONTEXT);
                let value = lbu(m, record.wrapping_add(from));
                m.sb(context.wrapping_add(to), value as u8);
            }
            leave_sound(m, 0x3b);
            title = 3;
        }
        dispatch::call(m, TITLE, [title, 0, 0, 0])
    })
}

/// 7 * (15 * `value` / 32767) as compiled (signed magic 0x80010003, shift
/// 14).
fn mul_15(value: u32) -> u32 {
    let n = value.wrapping_mul(15);
    let high = ((i64::from(n as i32) * i64::from(0x8001_0003_u32 as i32)) >> 32) as u32;
    let q = ((high.wrapping_add(n) as i32 >> 14) as u32).wrapping_sub((n as i32 >> 31) as u32);
    q.wrapping_mul(7)
}

/// The cursor (repeated pad 2 / 1) and the setting changes (8 / 4) of
/// [`sound_settings`]; flag 5 shows held pad 1 / 2.
fn input(m: &mut Machine, mono: u32, mode_at: u32, effects: u32, music: u32) {
    if pad(m, 0x130) & 2 != 0 {
        let record = state(m);
        let cursor = lbu(m, record.wrapping_add(0x100));
        let next = if cursor < 2 { cursor + 1 } else { 0 };
        m.sb(record.wrapping_add(0x100), next as u8);
        sound(m, 0x3d);
    }
    if pad(m, 0x130) & 1 != 0 {
        let record = state(m);
        let cursor = lbu(m, record.wrapping_add(0x100));
        let next = if cursor != 0 { cursor - 1 } else { 2 };
        m.sb(record.wrapping_add(0x100), next as u8);
        sound(m, 0x3d);
    }
    super::ov2_p2::set_flag(m, 5, 0);
    let held = pad(m, 0x128);
    if held & 1 != 0 {
        super::ov2_p2::set_flag(m, 5, 1);
    } else if held & 2 != 0 {
        super::ov2_p2::set_flag(m, 5, 2);
    }
    for increase in [true, false] {
        let bit = if increase { 8 } else { 4 };
        if pad(m, 0x130) & bit == 0 {
            continue;
        }
        let record = state(m);
        let id = match lbu(m, record.wrapping_add(0x100)) {
            0 => {
                if mono == 0 {
                    let mode = lbu(m, mode_at) | 1;
                    m.sb(mode_at, mode as u8);
                } else {
                    m.sb(mode_at, 0);
                }
                Some(0x3d)
            }
            cursor @ (1 | 2) => {
                let level = if cursor == 1 { effects } else { music };
                let value = lbu(m, level);
                if increase {
                    if value < 0xf0 {
                        m.sb(level, value.wrapping_add(0x10) as u8);
                        Some(0x2e)
                    } else if value == 0xff {
                        None
                    } else {
                        m.sb(level, 0xff);
                        Some(0x2e)
                    }
                } else if value >= 0x11 {
                    m.sb(level, value.wrapping_sub(0x10) as u8);
                    Some(0x2e)
                } else if value == 0 {
                    None
                } else {
                    m.sb(level, 0);
                    Some(0x2e)
                }
            }
            _ => None,
        };
        if let Some(id) = id {
            sound(m, id);
        }
    }
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    Some(match target {
        0x800d_a444 => stick_page(m),
        0x800d_a4f4 => open_sound(m),
        0x800d_a5ec => wait_sound_fade(m),
        0x800d_a66c => choose_page(m),
        0x800d_a870 => open_choice(m),
        0x800d_aa6c => sound_settings(m),
        _ => return None,
    })
}
