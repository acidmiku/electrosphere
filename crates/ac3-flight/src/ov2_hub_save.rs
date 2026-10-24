//! Inner state 9, the save screen ("Save game?", reached with hub choice 2
//! and after a mission when option bit 8 asks for a save): initialization
//! 800D75A4, the per-frame tick 800D7640 with its 20-phase table at
//! 0x800F5F7C, and the small helpers it alone uses (800D0EFC, 800D0F50,
//! 800EA008, 800EA020, 800F1954). The memory card work is in
//! [`super::card`].
//!
//! State `[0x800F5EB4]` (0x800F9568): +0 flags (1 card formatted, 2 save
//! confirmed, 4 load failed, 0x10 other session, 0x20 slot chosen), +4
//! flags (1 no mission data: options 0x10/8), +8 phase and +0xA substate
//! (halfwords), +0xC card status of port 0 (800CC554), +0x14 port, +0x18
//! save slot, +0x1C fade timer, +0x20 slide/zoom timer, +0x24 wait timer,
//! +0x28 yes/no cursor (0 yes, 1 no).
//!
//! Phase results (800D7640): 1 next substate, 0x40 skip a substate, 2 next
//! phase, 0x100 previous phase, 0x10 phase 3 (card check), 4 phase 2 (the
//! question), 0x80 phase 18 (leave), 0x20 close the card events and leave
//! the state (0x100).
use super::{
    CLEAR_TEXT, CONTEXT, MISSION, NEXT_PRIMITIVE, PLAY_SOUND, SELECT_PAGE, SET_OFFSET,
    SET_PARAMETER, SET_SCENE_COLOR, SET_STYLE, SET_TEXT, call, mulhi,
};
use crate::machine::{Machine, SmallData};

/// `[0x800F5EB4]`: the save screen state.
pub const STATE: u32 = 0x800f_5eb4;
/// 800F5F7C: per phase, the address of its substate callback list.
pub const PHASES: u32 = 0x800f_5f7c;
/// `[0x800F5398]`: the text block (+0 entry table, +0xC highlighted slot).
pub const TEXT: u32 = 0x800f_5398;
/// `[0x800F9E14]`: the visual-script block.
pub const VISUAL: u32 = 0x800f_9e14;
/// `[0x800F9890]`: the 3D map state.
pub const MAP: u32 = 0x800f_9890;
/// 800E7B50: background fade (`ov2air`); 800E983C / 800E9824: map object
/// exit / reveal (`ov2core`).
pub const BACKGROUND_FADE: u32 = 0x800e_7b50;
pub const MAP_OBJECTS_EXIT: u32 = 0x800e_983c;
pub const MAP_OBJECTS_REVEAL: u32 = 0x800e_9824;
/// 800D04C8: draw a string (`ov2core`; x, y, colour on the stack);
/// 800D0B28: a text table string (`ov2core`).
pub const DRAW_STRING: u32 = 0x800d_04c8;
pub const TEXT_STRING: u32 = 0x800d_0b28;
/// libc (resident): sprintf, strlen, strcat.
pub const SPRINTF: u32 = 0x800a_cd28;
pub const STRLEN: u32 = 0x800a_c328;
pub const STRCAT: u32 = 0x800a_c1b8;
/// 800CB134 "%2d:%2d", 800CB13C "%d:", 800CB140 / 800CB148 per-language
/// x offsets of "yes" / "no".
const TIME_FORMAT: u32 = 0x800c_b134;
const HOURS_FORMAT: u32 = 0x800c_b13c;
const YES_X: u32 = 0x800c_b140;
const NO_X: u32 = 0x800c_b148;

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn pressed(m: &mut Machine) -> u32 {
    let ctx = context(m);
    m.lw(ctx + 0x12c)
}

/// The selected port's card status (`[S + 0xC + 4*[S+0x14]]`).
fn status(m: &mut Machine) -> u32 {
    let s = state(m);
    let port = m.lw(s + 0x14);
    m.lw(s.wrapping_add(port << 2) + 0xc)
}

fn style(m: &mut Machine, slot: u32, value: u32) {
    call(m, SET_STYLE, [slot, value, 0, 0]);
}

fn text(m: &mut Machine, entry: u32) {
    call(m, SET_TEXT, [entry, 1, 0, 0]);
}

fn clear_text(m: &mut Machine) {
    call(m, CLEAR_TEXT, [0; 4]);
}

fn sound(m: &mut Machine, id: u32) {
    call(m, PLAY_SOUND, [id, 0, 0, 0]);
}

fn color(m: &mut Machine, rgb: [u32; 3]) {
    call(m, SET_SCENE_COLOR, [rgb[0], rgb[1], rgb[2], 0]);
}

/// Signed `x / 50` as compiled (multiply by 0x51EB851F, shift 4).
fn div50(x: i32) -> i32 {
    (mulhi(x, 0x51eb_851f) >> 4).wrapping_sub(x >> 31)
}

/// The fade colour `timer/50` of (143, 135, 83).
fn fade_color(m: &mut Machine, timer: i32) {
    let rgb = [143, 135, 83].map(|c: i32| div50(timer.wrapping_mul(c)) as u32);
    color(m, rgb);
}

/// 800D0EFC: text entry `entry`'s style halfword (`[[TEXT]] + 24*entry`).
/// Returns the entry address.
pub fn text_style_d0efc(m: &mut Machine, entry: u32, value: u32) -> u32 {
    let block = m.lw(TEXT);
    let table = m.lw(block);
    let at = table.wrapping_add(entry.wrapping_mul(24));
    m.sh(at, value as u16);
    at
}

/// 800D0F50: the text block's highlighted slot (+0xC). Returns the block.
pub fn text_highlight_d0f50(m: &mut Machine, slot: u32) -> u32 {
    let block = m.lw(TEXT);
    m.sw(block + 0xc, slot);
    block
}

/// 800F1954: visual-script slot `slot`'s scroll halfword (+0x41E + 8*slot).
/// Returns the slot address.
pub fn scroll_f1954(m: &mut Machine, slot: u32, value: u32) -> u32 {
    let block = m.lw(VISUAL);
    let at = block.wrapping_add(slot << 3);
    m.sh(at + 0x41e, value as u16);
    at
}

/// 800EA008: start the map's alternate backdrop (phase 1, progress 0).
pub fn backdrop_on_ea008(m: &mut Machine) -> u32 {
    let map = m.lw(MAP);
    m.sw(map + 0x1c, 1);
    m.sw(map + 0x28, 0);
    1
}

/// 800EA020: end the map's alternate backdrop (phase 3, progress 0).
pub fn backdrop_off_ea020(m: &mut Machine) -> u32 {
    let map = m.lw(MAP);
    m.sw(map + 0x1c, 3);
    m.sw(map + 0x28, 0);
    3
}

/// Show the yes/no pair with "yes" selected (cursor 0).
fn yes_selected(m: &mut Machine) {
    let s = state(m);
    m.sw(s + 0x28, 0);
    style(m, 1, 1);
    style(m, 2, 2);
    text_style_d0efc(m, 0x2a, 2);
    text_style_d0efc(m, 0x2b, 3);
}

/// Move the yes/no cursor: up (2) from yes to no, down (1) from no to
/// yes, with sound 0x3D (the shared tail of the question callbacks).
fn move_cursor(m: &mut Machine, pressed: u32) {
    let s = state(m);
    if pressed & 2 != 0 && m.lw(s + 0x28) == 0 {
        m.sw(s + 0x28, 1);
        style(m, 1, 2);
        style(m, 2, 1);
        text_style_d0efc(m, 0x2a, 3);
        text_style_d0efc(m, 0x2b, 2);
        sound(m, 0x3d);
    } else if self::pressed(m) & 1 != 0 && m.lw(s + 0x28) == 1 {
        m.sw(s + 0x28, 0);
        style(m, 1, 1);
        style(m, 2, 2);
        text_style_d0efc(m, 0x2a, 2);
        text_style_d0efc(m, 0x2b, 3);
        sound(m, 0x3d);
    }
}

/// Clear the screen text and show `entry` with the yes/no pair.
fn question(m: &mut Machine, entry: u32) {
    clear_text(m);
    text(m, entry);
    text(m, 0x2a);
    text(m, 0x2b);
}

/// The question callbacks 800D5460 / 800D5AD0 / 800D5210 / 800D6178 share
/// this form: cancel (0x20) returns to the question (4), confirm on "no"
/// returns 4, confirm on "yes" runs `yes` (its result is returned), other
/// input moves the cursor. `cancel_styles` clears the yes/no styles on
/// cancel.
fn answer(m: &mut Machine, cancel_styles: bool, yes: impl FnOnce(&mut Machine) -> u32) -> u32 {
    let input = pressed(m);
    if input & 0x20 != 0 {
        sound(m, 0x3b);
        if cancel_styles {
            style(m, 1, 0);
            style(m, 2, 0);
        }
        return 4;
    }
    if input & 0x10 != 0 {
        sound(m, 0x3c);
        let s = state(m);
        if m.lw(s + 0x28) != 0 {
            return 4;
        }
        return yes(m);
    }
    move_cursor(m, input);
    0
}

/// Card lost (status bit 2 clear): back to the card check (0x10).
fn card_lost(m: &mut Machine) -> bool {
    status(m) & 4 == 0
}

/// Cancel-only callbacks (800D594C, 800D6034, 800D7118): 0x10 when the
/// card is gone, 4 on cancel, else 0.
fn wait_cancel(m: &mut Machine) -> u32 {
    if card_lost(m) {
        return 0x10;
    }
    if pressed(m) & 0x20 != 0 {
        sound(m, 0x3b);
        return 4;
    }
    0
}

/// 800D4C8C (phase 0): start the fade (50 ticks) unless option 8 (after
/// a mission: black, skip).
pub fn fade_in_d4c8c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let ctx = context(m);
    let result = if m.lw(ctx + 0x970) & 8 != 0 {
        color(m, [0, 0, 0]);
        2
    } else {
        let s = state(m);
        m.sw(s + 0x1c, 0x32);
        call(m, BACKGROUND_FADE, [3, 0, 0, 0]);
        call(m, MAP_OBJECTS_EXIT, [0; 4]);
        color(m, [0x8f, 0x87, 0x53]);
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D4D08: fade the scene colour out over the timer.
pub fn fade_d4d08(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let timer = m.lw(s + 0x1c) as i32;
    let result = if timer <= 0 {
        color(m, [0, 0, 0]);
        2
    } else {
        fade_color(m, timer);
        let s = state(m);
        let t = m.lw(s + 0x1c);
        m.sw(s + 0x1c, t.wrapping_sub(1));
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800D4DE8 (phase 1): place the panels, page 0xC, slide timer 25, sound
/// 0x5A, the alternate map backdrop.
pub fn panels_d4de8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, SET_OFFSET, [1, 0, 0x271, 0]);
    call(m, SET_OFFSET, [2, 0x140, 0, 0]);
    call(m, SELECT_PAGE, [0xc, 0, 0, 0]);
    let s = state(m);
    m.sw(s + 0x20, 0x19);
    sound(m, 0x5a);
    backdrop_on_ea008(m);
    m.pop_frame(0x18);
    1
}

/// 800D4E48: slide panel 1 in (offset timer^2).
pub fn slide_in_d4e48(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let timer = m.lw(s + 0x20) as i32;
    let square = timer.wrapping_mul(timer) as u32;
    let result = if timer <= 0 {
        call(m, SET_OFFSET, [1, 0, 0, 0]);
        call(m, SET_OFFSET, [2, 0, 0, 0]);
        sound(m, 0x5c);
        2
    } else {
        call(m, SET_OFFSET, [1, 0, square, 0]);
        let s = state(m);
        let t = m.lw(s + 0x20);
        m.sw(s + 0x20, t.wrapping_sub(1));
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800D4ED4 (phase 2): "Save game?" with "yes" selected.
pub fn ask_save_d4ed4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, SELECT_PAGE, [0xc, 0, 0, 0]);
    yes_selected(m);
    question(m, 0x16);
    m.pop_frame(0x18);
    1
}

/// 800D4F5C: answer "Save game?": cancel or "no" leaves (0x80, cursor on
/// "no"), "yes" goes on (2).
pub fn answer_save_d4f5c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let input = pressed(m);
    let result = if input & 0x20 != 0 {
        style(m, 1, 0);
        style(m, 2, 0);
        clear_text(m);
        text(m, 0x16);
        sound(m, 0x3b);
        let s = state(m);
        m.sw(s + 0x28, 1);
        0x80
    } else if input & 0x10 != 0 {
        sound(m, 0x3c);
        let s = state(m);
        if m.lw(s + 0x28) == 0 { 2 } else { 0x80 }
    } else {
        move_cursor(m, input);
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800D50E8 (phase 3): "checking the card": wait until the card of port 0
/// is no longer busy.
pub fn check_card_d50e8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    m.sw(s, 0);
    call(m, SELECT_PAGE, [0xc, 0, 0, 0]);
    style(m, 1, 0);
    style(m, 2, 0);
    clear_text(m);
    text(m, 0x18);
    let s = state(m);
    let card = m.lw(s + 0xc);
    let result = if card & 4 != 0 {
        u32::from(card & 1 == 0) << 1
    } else {
        2
    };
    m.pop_frame(0x18);
    result
}

/// 800D5170 (phase 4): no usable card: ask whether to retry.
pub fn ask_no_card_d5170(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let result = if m.lw(s + 0xc) & 4 != 0 {
        2
    } else {
        question(m, 0x19);
        yes_selected(m);
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D5210: answer the no-card question (a card appearing restarts the
/// check; "yes" leaves with 0x80).
pub fn answer_no_card_d5210(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let result = if m.lw(s + 0xc) & 4 != 0 {
        0x10
    } else {
        answer(m, true, |m| {
            style(m, 1, 0);
            style(m, 2, 0);
            0x80
        })
    };
    m.pop_frame(0x18);
    result
}

/// 800D53A0 (phase 5): a card that needs attention (status bit 1): ask.
pub fn ask_card_d53a0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let card = status(m);
    let result = if card & 2 != 0 {
        question(m, 0x1a);
        yes_selected(m);
        1
    } else if card & 4 == 0 {
        0x10
    } else {
        2
    };
    m.pop_frame(0x18);
    result
}

/// "Yes" of 800D5460 / 800D5AD0 / 800D5D50: mark the save as confirmed
/// (flag 2) and clear the yes/no styles.
fn confirm_save(m: &mut Machine) {
    let s = state(m);
    let flags = m.lw(s);
    m.sw(s, flags | 2);
    style(m, 1, 0);
    style(m, 2, 0);
}

/// 800D5460: answer it ("yes": flag 2, leave with 0x80).
pub fn answer_card_d5460(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        answer(m, true, |m| {
            confirm_save(m);
            0x80
        })
    };
    m.pop_frame(0x18);
    result
}

/// 800D560C (phase 6): unformatted card (status bit 3 clear): ask to
/// format.
pub fn ask_format_d560c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let card = status(m);
    let result = if card & 8 != 0 {
        2
    } else if card & 4 == 0 {
        0x10
    } else {
        yes_selected(m);
        question(m, 0x1b);
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D56C4: answer the format question: cancel keeps "no" and returns
/// to the question (4); "yes" shows "formatting" (text 0x1D) and goes on.
pub fn answer_format_d56c4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        let input = pressed(m);
        if input & 0x20 != 0 {
            sound(m, 0x3b);
            let s = state(m);
            m.sw(s + 0x28, 1);
            4
        } else if input & 0x10 != 0 {
            sound(m, 0x3c);
            let s = state(m);
            match m.lw(s + 0x28) {
                0 => {
                    clear_text(m);
                    text(m, 0x1d);
                    style(m, 1, 0);
                    style(m, 2, 0);
                    1
                }
                1 => 4,
                _ => 0,
            }
        } else {
            move_cursor(m, input);
            0
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D5880: format the card: done -> flag 1, wait 100 ticks with text
/// 0x2C (1); failed -> text 0x1E, skip the wait (0x40).
pub fn format_d5880(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let port = m.lw(s + 0x14);
    let result = match super::card::format_cdb18(m, port) {
        0 => 0,
        1 => {
            let s = state(m);
            let flags = m.lw(s);
            m.sw(s + 0x24, 0x64);
            m.sw(s, flags | 1);
            clear_text(m);
            // a1 still holds 1 from 800CBCA8 (via 800CDB18).
            call(m, SET_TEXT, [0x2c, 1, 0, 0]);
            1
        }
        u32::MAX => {
            clear_text(m);
            text(m, 0x1e);
            0x40
        }
        _ => 0,
    };
    m.pop_frame(0x18);
    result
}

/// 800D5918 / 800D7384: count the wait timer (+0x24) down, then go on.
pub fn wait_d5918(m: &mut Machine) -> u32 {
    let s = state(m);
    let t = m.lw(s + 0x24) as i32;
    if t > 0 {
        m.sw(s + 0x24, t.wrapping_sub(1) as u32);
        0
    } else {
        m.sw(s + 0x24, 0);
        2
    }
}

/// 800D594C: wait for cancel after a failed format.
pub fn wait_cancel_d594c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = wait_cancel(m);
    m.pop_frame(0x18);
    result
}

/// 800D59C0 (phase 7): a save file exists (bit 4): go on; a freshly
/// formatted card counts as confirmed (flag 2).
pub fn check_file_d59c0(m: &mut Machine) -> u32 {
    let card = status(m);
    if card & 0x10 != 0 {
        return 2;
    }
    if card & 4 == 0 {
        return 0x10;
    }
    let s = state(m);
    let flags = m.lw(s);
    if flags & 1 == 0 {
        return 1;
    }
    m.sw(s, flags | 2);
    2
}

/// 800D5A30: list port 0's card: busy (-1) waits, free blocks go on (2),
/// a full card asks (text 0x22).
pub fn list_card_d5a30(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let free = super::card::directory_cdb68(m, 0);
    let result = if free != 0 {
        u32::from(free != u32::MAX) << 1
    } else {
        question(m, 0x22);
        yes_selected(m);
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D5AD0: answer the full-card question ("yes": flag 2, leave).
pub fn answer_full_d5ad0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        answer(m, true, |m| {
            confirm_save(m);
            0x80
        })
    };
    m.pop_frame(0x18);
    result
}

/// 800D5C7C (phase 8): "File not found. Save to new file?" unless the
/// file exists (2); a fresh format confirms at once.
pub fn ask_new_file_d5c7c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let card = status(m);
    let result = if card & 0x10 != 0 {
        2
    } else if card & 4 == 0 {
        0x10
    } else {
        let s = state(m);
        let flags = m.lw(s);
        if flags & 1 != 0 {
            m.sw(s, flags | 2);
            2
        } else {
            yes_selected(m);
            question(m, 0x21);
            1
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D5D50: answer it ("yes": flag 2, text 0x21 alone, go on).
pub fn answer_new_file_d5d50(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        answer(m, true, |m| {
            confirm_save(m);
            clear_text(m);
            text(m, 0x21);
            2
        })
    };
    m.pop_frame(0x18);
    result
}

/// 800D5F10 (phase 9): load the existing file first unless confirmed.
pub fn start_load_d5f10(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        let s = state(m);
        if m.lw(s) & 2 != 0 {
            2
        } else {
            clear_text(m);
            text(m, 0x18);
            1
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D5F80: load: done (2), failed (text 0x1E, 1), bad data (flag 4,
/// 2), other session (flag 0x10, 2).
pub fn load_d5f80(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let port = m.lw(s + 0x14);
    let result = match super::card::load_ce2fc(m, port, 1) {
        0 => 0,
        1 => 2,
        u32::MAX => {
            clear_text(m);
            text(m, 0x1e);
            1
        }
        2 => {
            let s = state(m);
            let flags = m.lw(s);
            m.sw(s, flags | 4);
            2
        }
        3 => {
            let s = state(m);
            let flags = m.lw(s);
            m.sw(s, flags | 0x10);
            2
        }
        _ => 0,
    };
    m.pop_frame(0x18);
    result
}

/// 800D6034: wait for cancel after a failed load.
pub fn wait_cancel_d6034(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = wait_cancel(m);
    m.pop_frame(0x18);
    result
}

/// 800D60A8 (phase 10): bad data on the card (flag 4): ask to overwrite.
pub fn ask_bad_data_d60a8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        let s = state(m);
        let flags = m.lw(s);
        if flags & 4 == 0 {
            2
        } else {
            m.sw(s, flags & !4);
            question(m, 0x20);
            yes_selected(m);
            1
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D6178: answer it ("yes" leaves with 0x80; cancel keeps the styles).
pub fn answer_bad_data_d6178(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        answer(m, false, |m| {
            style(m, 1, 0);
            style(m, 2, 0);
            0x80
        })
    };
    m.pop_frame(0x18);
    result
}

/// 800D6304 (phase 11): a save from another session (flag 0x10): ask.
pub fn ask_other_d6304(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        let s = state(m);
        if m.lw(s) & 0x10 == 0 {
            2
        } else {
            yes_selected(m);
            question(m, 0x29);
            1
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D63C4: answer it: "yes" goes on (2); "no" or cancel clears flag
/// 0x10 and returns to the question (4).
pub fn answer_other_d63c4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        let input = pressed(m);
        let clear = |m: &mut Machine| {
            let s = state(m);
            let flags = m.lw(s);
            m.sw(s, flags & !0x10);
        };
        if input & 0x20 != 0 {
            sound(m, 0x3b);
            clear(m);
            4
        } else if input & 0x10 != 0 {
            sound(m, 0x3c);
            let s = state(m);
            if m.lw(s + 0x28) == 0 {
                2
            } else {
                clear(m);
                4
            }
        } else {
            move_cursor(m, input);
            0
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D6560: the slot icons: the 36 mission marks of record `k` (style =
/// nibble `i` of record +0x80, or 0 for an empty record), the six slot
/// tabs (style 2) and the chosen tab (style 1).
pub fn slot_icons_d6560(m: &mut Machine, k: u32) -> u32 {
    m.push_frame(0x20);
    let record = super::card::record_ce500(m, k);
    if record == 0 {
        for i in 0..0x24 {
            style(m, i + 0xb, 0);
        }
    } else {
        for i in 1..0x25u32 {
            let index = ((i as i32).wrapping_add((i >> 31) as i32) >> 1) as u32;
            let nibble = i.wrapping_sub(index << 1);
            let byte = u32::from(m.lbu(record.wrapping_add(index) + 0x80));
            let value = (((byte as i32) >> ((nibble << 2) & 31)) as u32) & 0xf;
            style(m, i + 0xa, value);
        }
    }
    for tab in 1..7 {
        style(m, tab, 2);
    }
    let result = call(m, SET_STYLE, [k.wrapping_add(1), 1, 0, 0]);
    m.pop_frame(0x20);
    result
}

/// Draw `string` with 800D04C8 (font `font`, at `x`, `y`, colour 0x808080)
/// into the mission block's next free primitive; `frame` is the caller's
/// pushed frame holding the three stack arguments.
fn draw_string(m: &mut Machine, frame: u32, font: u32, string: u32, x: u32, y: u32) -> u32 {
    m.sw(frame + 0x10, x);
    m.sw(frame + 0x14, y);
    m.sw(frame + 0x18, 0x0080_8080);
    let block = m.lw(MISSION);
    let ot = m.lw(block + 0x90);
    let prim = m.lw(block.wrapping_add(NEXT_PRIMITIVE));
    let next = call(m, DRAW_STRING, [font, string, ot, prim]);
    let block = m.lw(MISSION);
    m.sw(block.wrapping_add(NEXT_PRIMITIVE), next);
    next
}

/// 800D6634: the slot's details: an empty slot shows text 0xF; a used one
/// draws the mission name (100, 274), the rank text (0xC + rank), texts
/// 0xA and 0x15, and the play time "h:mm:ss" (150, 326; spaces of the
/// "%2d" fields become '0').
pub fn slot_info_d6634(m: &mut Machine, k: u32) -> u32 {
    let frame = m.push_frame(0xd8);
    let record = super::card::record_ce500(m, k);
    let result = if record == 0 {
        call(m, SET_TEXT, [0xf, 1, 0, 0])
    } else {
        let mission = u32::from(m.lbu(record + 0x129));
        let entry = super::MISSION_RECORDS.wrapping_add(mission.wrapping_mul(12));
        let name = m.lw(entry + 8);
        draw_string(m, frame, 5, name, 0x64, 0x112);
        text(m, 0xa);
        for rank in 0..3u32 {
            if rank == u32::from(m.lbu(record + 0x128)) {
                text(m, rank + 0xc);
            }
        }
        text(m, 0x15);
        let time = m.lw(record + 0x124) as i32;
        let minutes = (mulhi(time, 0x0576_19f1) >> 5).wrapping_sub(time >> 31);
        let seconds = (mulhi(time, 0x51eb_851f) >> 3).wrapping_sub(time >> 31);
        let seconds = seconds.wrapping_sub(minutes.wrapping_mul(60));
        let hours_of =
            (mulhi(minutes, 0x8888_8889).wrapping_add(minutes) >> 5).wrapping_sub(minutes >> 31);
        let minutes = minutes.wrapping_sub(hours_of.wrapping_mul(60));
        let clock = frame + 0x70;
        call(
            m,
            SPRINTF,
            [clock, TIME_FORMAT, minutes as u32, seconds as u32],
        );
        let length = call(m, STRLEN, [clock, 0, 0, 0]) as i32;
        for i in 0..length.max(0) as u32 {
            if m.lbu(clock + i) == b' ' {
                m.sb(clock + i, b'0');
            }
        }
        let time = m.lw(record + 0x124) as i32;
        let hours = (mulhi(time, 0x5d34_edef) >> 15).wrapping_sub(time >> 31);
        let line = frame + 0x20;
        call(m, SPRINTF, [line, HOURS_FORMAT, hours as u32, 0]);
        call(m, STRCAT, [line, clock, 0, 0]);
        draw_string(m, frame, 5, line, 0x96, 0x146)
    };
    m.pop_frame(0xd8);
    result
}

/// 800D6890 (phase 12): choose the slot (skipped without mission data):
/// start at the current record unless chosen before, page 0xD, zoom 0.
pub fn choose_slot_d6890(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let result = if card_lost(m) {
        0x10
    } else {
        let s = state(m);
        if m.lw(s + 4) & 1 != 0 {
            2
        } else {
            if m.lw(s) & 0x20 == 0 {
                let current = super::card::current_record_ccf50(m);
                let s = state(m);
                m.sw(s + 0x18, current);
            }
            call(m, SELECT_PAGE, [0xd, 0, 0, 0]);
            let s = state(m);
            m.sw(s + 0x20, 0);
            for slot in 1..7 {
                let s = state(m);
                let zoom = m.lw(s + 0x20);
                call(m, SET_PARAMETER, [slot, zoom, 0, 0]);
            }
            let s = state(m);
            let k = m.lw(s + 0x18);
            slot_icons_d6560(m, k);
            let s = state(m);
            let k = m.lw(s + 0x18);
            slot_info_d6634(m, k);
            clear_text(m);
            1
        }
    };
    m.pop_frame(0x20);
    result
}

/// 800D6988: browse the slots (repeat input +0x130: 8 next, 4 previous,
/// wrapping over 0..5); confirm goes on, cancel returns to the question.
pub fn browse_slots_d6988(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    clear_text(m);
    text(m, 0x23);
    let s = state(m);
    let k = m.lw(s + 0x18);
    slot_info_d6634(m, k);
    let result = if card_lost(m) {
        0x10
    } else {
        let ctx = context(m);
        let input = m.lw(ctx + 0x12c);
        if input & 0x10 != 0 {
            sound(m, 0x3c);
            2
        } else if input & 0x20 != 0 {
            sound(m, 0x3b);
            4
        } else {
            let repeat = m.lw(ctx + 0x130);
            if repeat & 8 != 0 || repeat & 4 != 0 {
                sound(m, 0x3d);
                let s = state(m);
                let k = m.lw(s + 0x18) as i32;
                let next = if repeat & 8 != 0 {
                    if k < 5 { k + 1 } else { 0 }
                } else if k > 0 {
                    k - 1
                } else {
                    5
                };
                m.sw(s + 0x18, next as u32);
                let s = state(m);
                let k = m.lw(s + 0x18);
                slot_icons_d6560(m, k);
            }
            0
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D6AD0 (phase 13): "Save to file N?" (text 0x24 empty, 0x25 used)
/// with "no" selected; zoom 0, flag 0x20.
pub fn ask_slot_d6ad0(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let s = state(m);
    let result = if m.lw(s + 4) & 1 != 0 {
        2
    } else {
        let k = m.lw(s + 0x18);
        let record = super::card::record_ce500(m, k);
        clear_text(m);
        let s = state(m);
        let flags = m.lw(s);
        m.sw(s + 0x20, 0);
        m.sw(s, flags | 0x20);
        for slot in 1..7 {
            let s = state(m);
            let zoom = m.lw(s + 0x20);
            call(m, SET_PARAMETER, [slot, zoom, 0, 0]);
        }
        text(m, if record != 0 { 0x25 } else { 0x24 });
        let s = state(m);
        let k = m.lw(s + 0x18);
        slot_info_d6634(m, k);
        let s = state(m);
        let k = m.lw(s + 0x18);
        text_highlight_d0f50(m, k.wrapping_add(1));
        let s = state(m);
        m.sw(s + 0x28, 1);
        style(m, 0x33, 2);
        style(m, 0x34, 1);
        text_style_d0efc(m, 0x2a, 3);
        text_style_d0efc(m, 0x2b, 2);
        1
    };
    m.pop_frame(0x20);
    result
}

/// 800D6BE8: zoom the chosen tab in (10 steps).
pub fn zoom_in_d6be8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let k = m.lw(s + 0x18);
    slot_info_d6634(m, k);
    let s = state(m);
    let zoom = m.lw(s + 0x20) as i32;
    let result = if zoom < 0xa {
        let k = m.lw(s + 0x18);
        m.sw(s + 0x20, zoom.wrapping_add(1) as u32);
        call(
            m,
            SET_PARAMETER,
            [k.wrapping_add(1), zoom.wrapping_add(1) as u32, 0, 0],
        );
        0
    } else {
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D6C50: the slot question: draw "yes"/"no" (fonts 0xA/7 by the
/// cursor) next to the tab (x from the per-language tables), and answer:
/// confirm on "yes" goes on (2), on "no" or cancel back to browsing (1,
/// cancel puts the cursor on "no").
pub fn answer_slot_d6c50(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x50);
    let s = state(m);
    let k = m.lw(s + 0x18);
    slot_info_d6634(m, k);
    let yes_x = frame + 0x20;
    let no_x = frame + 0x28;
    copy5(m, yes_x, YES_X);
    copy5(m, no_x, NO_X);
    let s = state(m);
    let tab = m.lw(s + 0x18) as i32;
    let ctx = context(m);
    let language = m.lw(ctx + 0x38c);
    let (column, base) = if tab < 3 {
        (tab, 0x65)
    } else {
        (tab - 3, 0x81)
    };
    let row = column.wrapping_mul(33);
    let yes = row
        .wrapping_add(i32::from(m.lbu(yes_x.wrapping_add(language))))
        .wrapping_add(base);
    let no = row
        .wrapping_add(i32::from(m.lbu(no_x.wrapping_add(language))))
        .wrapping_add(base);
    let s = state(m);
    let (yes_font, no_font) = if m.lw(s + 0x28) != 0 {
        (0xa, 7)
    } else {
        (7, 0xa)
    };
    for (entry, font, x, y) in [(0x2a, yes_font, yes, 0x9a), (0x2b, no_font, no, 0xb7)] {
        let block = m.lw(MISSION);
        let table = m.lw(block.wrapping_add(0x2_00b4));
        let string = call(m, TEXT_STRING, [entry, table, 0, 0]);
        draw_string(m, frame, font, string, sext16(x) as u32, y);
    }
    let s = state(m);
    if m.lw(s + 0x28) == 0 {
        style(m, 0x33, 1);
        style(m, 0x34, 2);
        text_style_d0efc(m, 0x2a, 2);
        text_style_d0efc(m, 0x2b, 3);
    } else {
        style(m, 0x33, 2);
        style(m, 0x34, 1);
        text_style_d0efc(m, 0x2a, 3);
        text_style_d0efc(m, 0x2b, 2);
    }
    let input = pressed(m);
    let result = if input & 0x20 != 0 {
        sound(m, 0x3b);
        let s = state(m);
        m.sw(s + 0x28, 1);
        1
    } else if input & 0x10 != 0 {
        let s = state(m);
        if m.lw(s + 0x28) == 0 {
            sound(m, 0x3c);
            2
        } else {
            sound(m, 0x3b);
            1
        }
    } else {
        let s = state(m);
        if input & 2 != 0 && m.lw(s + 0x28) == 0 {
            m.sw(s + 0x28, 1);
            sound(m, 0x3d);
        } else if pressed(m) & 1 != 0 && m.lw(s + 0x28) == 1 {
            m.sw(s + 0x28, 0);
            sound(m, 0x3d);
        }
        0
    };
    m.pop_frame(0x50);
    result
}

/// Sign-extend the low halfword.
fn sext16(x: i32) -> i32 {
    (x << 16) >> 16
}

/// The 5-byte copies of 800D6C50 (`lwl/lwr` of 4 bytes, `lb` of the fifth).
fn copy5(m: &mut Machine, dst: u32, src: u32) {
    for i in 0..5 {
        let b = m.lbu(src + i);
        m.sb(dst + i, b);
    }
}

/// 800D6FB4: zoom the tab back out, then return to browsing (0x100).
pub fn zoom_out_d6fb4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let k = m.lw(s + 0x18);
    slot_info_d6634(m, k);
    let s = state(m);
    let zoom = m.lw(s + 0x20) as i32;
    let result = if zoom > 0 {
        let k = m.lw(s + 0x18);
        m.sw(s + 0x20, zoom.wrapping_sub(1) as u32);
        call(
            m,
            SET_PARAMETER,
            [k.wrapping_add(1), zoom.wrapping_sub(1) as u32, 0, 0],
        );
        0
    } else {
        0x100
    };
    m.pop_frame(0x18);
    result
}

/// 800D7018 (phase 14): "saving" (text 0x26); confirmed saves skip the
/// file creation (1 = create, 2 = skip).
pub fn saving_d7018(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, SELECT_PAGE, [0xc, 0, 0, 0]);
    style(m, 1, 0);
    style(m, 2, 0);
    clear_text(m);
    text(m, 0x26);
    let result = if card_lost(m) {
        0x10
    } else {
        let s = state(m);
        if m.lw(s) & 2 != 0 { 1 } else { 2 }
    };
    m.pop_frame(0x18);
    result
}

/// 800D70AC: create the file: done (2), failed (text 0x27, 1).
pub fn create_file_d70ac(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let port = m.lw(s + 0x14);
    let result = match super::card::create_cdca8(m, port) {
        0 => 0,
        1 => 2,
        u32::MAX => {
            clear_text(m);
            text(m, 0x27);
            1
        }
        _ => 0,
    };
    m.pop_frame(0x18);
    result
}

/// 800D7118: wait for cancel after a failed creation.
pub fn wait_cancel_d7118(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = wait_cancel(m);
    m.pop_frame(0x18);
    result
}

/// 800D718C (phase 15): merge the loaded data unless it came from
/// another session (1 = merge, 2 = skip).
pub fn check_merge_d718c(m: &mut Machine) -> u32 {
    if card_lost(m) {
        return 0x10;
    }
    let s = state(m);
    if m.lw(s) & 0x10 != 0 { 1 } else { 2 }
}

/// 800D71E0: merge the card's progress into the context.
pub fn merge_d71e0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if card_lost(m) {
        0x10
    } else {
        super::card::merge_ce5c8(m);
        2
    };
    m.pop_frame(0x18);
    result
}

/// 800D7230 (phase 16): pack the progress for the chosen slot.
pub fn pack_progress_d7230(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let k = m.lw(s + 0x18);
    super::card::pack_progress_cd580(m, k);
    m.pop_frame(0x18);
    1
}

/// 800D725C: pack the chosen slot's record (with mission data).
pub fn pack_record_d725c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    if m.lw(s + 4) & 1 == 0 {
        let k = m.lw(s + 0x18);
        super::card::pack_record_cd98c(m, k);
    }
    m.pop_frame(0x18);
    1
}

/// 800D72A0: write the save: done (2), failed (text 0x27, 1).
pub fn write_d72a0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let port = m.lw(s + 0x14);
    let result = match super::card::save_cded4(m, port, 1) {
        0 => 0,
        1 => 2,
        u32::MAX => {
            clear_text(m);
            text(m, 0x27);
            1
        }
        _ => 0,
    };
    m.pop_frame(0x18);
    result
}

/// 800D730C: wait for cancel after a failed write.
pub fn wait_cancel_d730c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if pressed(m) & 0x20 != 0 {
        sound(m, 0x3b);
        4
    } else {
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800D734C (phase 17): "saved" (text 0x28) for 100 ticks.
pub fn saved_d734c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    m.sw(s + 0x24, 0x64);
    clear_text(m);
    text(m, 0x28);
    m.pop_frame(0x18);
    1
}

/// 800D73B8 (phase 18): leave: panel 2 out, sound 0x5B, map backdrop off.
pub fn leave_d73b8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    m.sw(s + 0x20, 0);
    clear_text(m);
    call(m, SET_OFFSET, [2, 0x140, 0, 0]);
    sound(m, 0x5b);
    backdrop_off_ea020(m);
    m.pop_frame(0x18);
    1
}

/// 800D7400: slide panel 1 out (25 steps).
pub fn slide_out_d7400(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let timer = m.lw(s + 0x20) as i32;
    let result = if timer < 0x19 {
        call(m, SET_OFFSET, [1, 0, timer.wrapping_mul(timer) as u32, 0]);
        let s = state(m);
        let t = m.lw(s + 0x20);
        m.sw(s + 0x20, t.wrapping_add(1));
        0
    } else {
        2
    };
    m.pop_frame(0x18);
    result
}

/// 800D7468 (phase 19): back to the hub: background fade 1 and the map
/// objects (option 8, after a mission: leave at once, 0x20).
pub fn back_to_hub_d7468(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let ctx = context(m);
    let result = if m.lw(ctx + 0x970) & 8 != 0 {
        0x20
    } else {
        let s = state(m);
        m.sw(s + 0x1c, 0);
        call(m, BACKGROUND_FADE, [1, 0, 0, 0]);
        call(m, MAP_OBJECTS_REVEAL, [0; 4]);
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D74C0: fade the scene colour back in over 50 ticks, then leave.
pub fn fade_back_d74c0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    let timer = m.lw(s + 0x1c) as i32;
    let result = if timer >= 0x32 {
        color(m, [0x8f, 0x87, 0x53]);
        0x20
    } else {
        fade_color(m, timer);
        let s = state(m);
        let t = m.lw(s + 0x1c);
        m.sw(s + 0x1c, t.wrapping_add(1));
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800D75A4: state 9 initialization: clear the state, prepare the card
/// layer and its events; options 0x10/8 mean no mission data (flag 1).
pub fn init_d75a4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = state(m);
    m.sw(s, 0);
    m.sw(s + 4, 0);
    m.sh(s + 8, 0);
    m.sh(s + 0xa, 0);
    m.sw(s + 0x14, 0);
    m.sw(s + 0x18, 0);
    super::card::prepare_cceb8(m, 0, 0);
    super::card::enable_events_cc774(m);
    clear_text(m);
    let ctx = context(m);
    let options = m.lw(ctx + 0x970);
    if options & 0x10 != 0 || options & 8 != 0 {
        let s = state(m);
        let flags = m.lw(s + 4);
        m.sw(s + 4, flags | 1);
    }
    m.pop_frame(0x18);
    1
}

/// 800D7640: state 9 tick: card monitor, port 0 status, scrolling
/// backgrounds (slots 1/2 by the frame clock), then the phase callback
/// (`jalr` through [`PHASES`]) and its result.
pub fn tick_d7640(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    super::card::monitor_cbfac(m);
    let card_status = super::card::status_cc554(m, 0);
    let s = state(m);
    m.sw(s + 0xc, card_status);
    let ctx = context(m);
    let clock = m.lw(ctx + 0x20);
    scroll_f1954(m, 1, clock << 6);
    let ctx = context(m);
    let clock = m.lw(ctx + 0x20);
    scroll_f1954(m, 2, clock.wrapping_neg() << 6);
    let s = state(m);
    let phase = m.lh(s + 8) as i32 as u32;
    let list = m.lw(PHASES.wrapping_add(phase << 2));
    let sub = m.lh(s + 0xa) as i32 as u32;
    let callback = m.lw(list.wrapping_add(sub << 2));
    // The phase callbacks read no argument register.
    let r = call(m, callback, [0; 4]);
    let s = state(m);
    let result = if r & 1 != 0 {
        let v = m.lhu(s + 0xa);
        m.sh(s + 0xa, v.wrapping_add(1));
        0
    } else if r & 0x40 != 0 {
        let v = m.lhu(s + 0xa);
        m.sh(s + 0xa, v.wrapping_add(2));
        0
    } else if r & 2 != 0 {
        let v = m.lhu(s + 8);
        m.sh(s + 0xa, 0);
        m.sh(s + 8, v.wrapping_add(1));
        0
    } else if r & 0x100 != 0 {
        let v = m.lhu(s + 8);
        m.sh(s + 0xa, 0);
        m.sh(s + 8, v.wrapping_sub(1));
        0
    } else if r & 0x10 != 0 {
        m.sh(s + 8, 3);
        m.sh(s + 0xa, 0);
        0
    } else if r & 4 != 0 {
        m.sh(s + 8, 2);
        m.sh(s + 0xa, 0);
        0
    } else if r & 0x20 != 0 {
        super::card::close_events_cc8f4(m);
        0x100
    } else if r & 0x80 != 0 {
        m.sh(s + 8, 0x12);
        m.sh(s + 0xa, 0);
        0
    } else {
        0
    };
    m.pop_frame(0x20);
    result
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
        let [a0, a1, ..] = args;
        Some(match target {
            0x800d_0efc => text_style_d0efc(m, a0, a1),
            0x800d_0f50 => text_highlight_d0f50(m, a0),
            0x800e_a008 => backdrop_on_ea008(m),
            0x800e_a020 => backdrop_off_ea020(m),
            0x800f_1954 => scroll_f1954(m, a0, a1),
            0x800d_4c8c => fade_in_d4c8c(m),
            0x800d_4d08 => fade_d4d08(m),
            0x800d_4de8 => panels_d4de8(m),
            0x800d_4e48 => slide_in_d4e48(m),
            0x800d_4ed4 => ask_save_d4ed4(m),
            0x800d_4f5c => answer_save_d4f5c(m),
            0x800d_50e8 => check_card_d50e8(m),
            0x800d_5170 => ask_no_card_d5170(m),
            0x800d_5210 => answer_no_card_d5210(m),
            0x800d_53a0 => ask_card_d53a0(m),
            0x800d_5460 => answer_card_d5460(m),
            0x800d_560c => ask_format_d560c(m),
            0x800d_56c4 => answer_format_d56c4(m),
            0x800d_5880 => format_d5880(m),
            0x800d_5918 | 0x800d_7384 => wait_d5918(m),
            0x800d_594c => wait_cancel_d594c(m),
            0x800d_59c0 => check_file_d59c0(m),
            0x800d_5a30 => list_card_d5a30(m),
            0x800d_5ad0 => answer_full_d5ad0(m),
            0x800d_5c7c => ask_new_file_d5c7c(m),
            0x800d_5d50 => answer_new_file_d5d50(m),
            0x800d_5f10 => start_load_d5f10(m),
            0x800d_5f80 => load_d5f80(m),
            0x800d_6034 => wait_cancel_d6034(m),
            0x800d_60a8 => ask_bad_data_d60a8(m),
            0x800d_6178 => answer_bad_data_d6178(m),
            0x800d_6304 => ask_other_d6304(m),
            0x800d_63c4 => answer_other_d63c4(m),
            0x800d_6560 => slot_icons_d6560(m, a0),
            0x800d_6634 => slot_info_d6634(m, a0),
            0x800d_6890 => choose_slot_d6890(m),
            0x800d_6988 => browse_slots_d6988(m),
            0x800d_6ad0 => ask_slot_d6ad0(m),
            0x800d_6be8 => zoom_in_d6be8(m),
            0x800d_6c50 => answer_slot_d6c50(m),
            0x800d_6fb4 => zoom_out_d6fb4(m),
            0x800d_7018 => saving_d7018(m),
            0x800d_70ac => create_file_d70ac(m),
            0x800d_7118 => wait_cancel_d7118(m),
            0x800d_718c => check_merge_d718c(m),
            0x800d_71e0 => merge_d71e0(m),
            0x800d_7230 => pack_progress_d7230(m),
            0x800d_725c => pack_record_d725c(m),
            0x800d_72a0 => write_d72a0(m),
            0x800d_730c => wait_cancel_d730c(m),
            0x800d_734c => saved_d734c(m),
            0x800d_73b8 => leave_d73b8(m),
            0x800d_7400 => slide_out_d7400(m),
            0x800d_7468 => back_to_hub_d7468(m),
            0x800d_74c0 => fade_back_d74c0(m),
            0x800d_75a4 => init_d75a4(m),
            0x800d_7640 => tick_d7640(m),
            _ => return None,
        })
    }
}
