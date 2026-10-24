//! Overlay module 2 (game state 3): the OPTIONS screen (mission state 10,
//! choice 3 of the hub menu) and the post-flight DEBRIEFING, state 11.
//!
//! **State 10, OPTIONS** (steps 800DD1A4 initialization, 800DD214 tick):
//! a small task machine (800E50B8 over the control block `[0x800F6060]` and
//! the mode table 0x800F67A4) runs the options menu, a 2 x 2 grid of KEY
//! CONFIG, GAME CONFIG, BACK TO TITLE and MUSIC PLAYER (mode 0:
//! 800DCAAC/800DCC48/800DCD34/800DCCC4; the music player cell is locked
//! until context +0x974 bit 1), then the selected screen:
//! - mode 1, KEY CONFIG ([`keys`]): the controller settings screen (MAIN tab:
//!   control type EASY/NORMAL, response, neutral zone; CUSTOM tab: the
//!   button assignment, four presets 800DE73C..800DE978), with its own task
//!   machine (800E47E0, table 0x800F670C);
//! - mode 2, GAME CONFIG (800E4978/800E4B10/800E4D6C and table 0x800F6784);
//! - mode 3, BACK TO TITLE (800DCF70/800DCFA4/800DCFEC: the panels slide
//!   out and the screen fades over 50 frames, then result 0x40 sets context
//!   +0x964 bit 2 and leaves for the title, state 4);
//! - mode 4, MUSIC PLAYER (800DD0CC/800DD170: result 0x80 copies the mission
//!   pair to +0x394 and leaves with 0x10).
//!
//! **State 11** ([`results`], steps 800D2C98/800D2E18/800D31FC): entered at
//! the overlay's initialization after a flight returned (progress flag 8):
//! the DEBRIEFING screen (results, map replay, rank and story texts while
//! the next mission loads), followed by state 12.
//!
//! Every routine follows its original instructions and store order;
//! routines with an original stack frame that call other routines push it.
//! Routines owned elsewhere (ov2core's visual slots, text and stream
//! helpers, ov2hub's 800D170C, ov2air's 800E7B50, the SDK and the sound
//! driver) are called by original address through
//! [`external`](crate::ov2_briefing::external) (which tests may replay).
//! [`dispatch`] claims these routines only while module 2 is loaded.
use crate::{
    common::{self, CONTEXT},
    machine::{Machine, SmallData},
    ov2_briefing::{WORK_HIGH, external, lh, lhu, mul, mulhi, sh, sound, sra, work},
};

#[path = "ov2_options_keys.rs"]
pub mod keys;
#[path = "ov2_options_results.rs"]
pub mod results;

/// Overlay module number of the mission screens.
pub const MODULE: u32 = 2;
/// 800F6060: pointer to the OPTIONS control block {+0 mode, +2 step, +4
/// choice, +6 timer (halfwords), +8 flags halfword (1 slide sound, 2
/// leave backwards, 4 leaving for the title)}.
pub const CONTROL: u32 = 0x800f_6060;
/// 800F67A4: the OPTIONS mode table (five step lists).
pub const MODES: u32 = 0x800f_67a4;

// Routines owned elsewhere, called by original address.
pub(crate) const SET_VISUAL_OFFSET: u32 = 0x800f_18ec;
pub(crate) const SET_VISUAL_PARAMETER: u32 = 0x800f_193c;
pub(crate) const SET_VISUAL_STYLE: u32 = 0x800f_1770;
pub(crate) const SELECT_VISUAL_PAGE: u32 = 0x800f_196c;
pub(crate) const VISUAL_ADDRESS: u32 = 0x800f_1ba0;
pub(crate) const CLEAR_TEXT: u32 = 0x800d_0f20;
pub(crate) const SHOW_TEXT: u32 = 0x800d_0ee4;
/// ov2core: scene color (r, g, b) (800E79D4).
pub(crate) const SCENE_COLOR: u32 = 0x800e_79d4;
/// ov2air: background mode (800E7B50).
pub(crate) const BACKGROUND: u32 = 0x800e_7b50;
/// ov2core: 800E983C and 800E9824 (scene helpers).
pub(crate) const SCENE_ENTER: u32 = 0x800e_983c;
pub(crate) const SCENE_LEAVE: u32 = 0x800e_9824;
/// ov2hub: the hub panel drawer (800D170C).
pub(crate) const HUB_PANELS: u32 = 0x800d_170c;

pub(crate) fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}
pub(crate) fn control(m: &mut Machine) -> u32 {
    m.lw(CONTROL)
}
/// Halfword `+offset` of the control block (signed, unsigned).
fn control_half(m: &mut Machine, offset: u32) -> u32 {
    let block = control(m);
    lh(m, block + offset)
}
fn control_halfu(m: &mut Machine, offset: u32) -> u32 {
    let block = control(m);
    lhu(m, block + offset)
}

/// Routines translated here, by original address (original register ABI,
/// result = `v0`). Claimed only while overlay module 2 is loaded. Routines
/// without a result return 0.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if common::loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        // OPTIONS menu, BACK TO TITLE, MUSIC PLAYER and the mode machine.
        0x800d_c8f4 => slide_left(m, a0, a1),
        0x800d_c938 => slide_right(m, a0, a1),
        0x800d_c974 => slide_down(m, a0, a1),
        0x800d_c9b0 => {
            place_panels(m, a0);
            0
        }
        0x800d_caac => menu_open(m),
        0x800d_cc48 => menu_slide_in(m),
        0x800d_cd34 => menu_select(m),
        0x800d_ccc4 => menu_slide_out(m),
        0x800d_cf70 => title_slide_out(m),
        0x800d_cfa4 => title_fade_start(m),
        0x800d_cfec => title_fade(m),
        0x800d_d0cc => music_start(m),
        0x800d_d170 => music_wait(m),
        0x800d_d1a4 => initialize(m),
        0x800d_d214 => tick(m),
        0x800e_50b8 => run_modes(m, a0),
        0x800e_9854 => set_scene_mode(m, a0),
        // KEY CONFIG.
        0x800d_e4f8 => keys::glyph(m, a0, a1, a2, a3),
        0x800d_e73c => {
            keys::preset_a(m);
            0
        }
        0x800d_e818 => {
            keys::preset_b(m);
            0
        }
        0x800d_e8e0 => {
            keys::preset_c(m);
            0
        }
        0x800d_e978 => {
            keys::preset_d(m);
            0
        }
        0x800d_ea48 => keys::type_index(a0),
        0x800d_eab0 => {
            keys::load_settings(m);
            0
        }
        0x800d_ee34 => {
            keys::store_settings(m, a0);
            0
        }
        0x800d_f39c => {
            keys::store_for_type(m);
            0
        }
        0x800d_f424 => {
            keys::main_rows(m);
            0
        }
        0x800d_f650 => keys::axis_row(m),
        0x800d_f93c => keys::action_list_open(m),
        0x800d_f9d0 => keys::action_list_grow(m),
        0x800d_fabc => keys::action_list(m),
        0x800d_ff58 => keys::action_list_shrink(m),
        0x800e_0054 => {
            keys::inversion_text(m);
            0
        }
        0x800e_0108 => keys::inversion_open(m),
        0x800e_0140 => keys::inversion_choice(m),
        0x800e_0440 => keys::inversion_close(m),
        0x800e_0538 => {
            keys::item_text(m, a0);
            0
        }
        0x800e_05d0 => {
            keys::tab_arrows(m, a0);
            0
        }
        0x800e_0810 => keys::button_grid(m),
        0x800e_09d4 => keys::button_list_open(m),
        0x800e_0a9c => keys::button_list_grow(m),
        0x800e_0b94 => keys::button_list(m),
        0x800e_0f8c => keys::button_list_shrink(m),
        0x800e_1040 => keys::confirm_open(m),
        0x800e_1128 => keys::confirm_grow(m),
        0x800e_11b8 => keys::confirm_answer(m),
        0x800e_1518 => keys::confirm_close(m),
        0x800e_1764 => keys::pad_check(m),
        0x800e_182c => keys::pad_wait(m),
        0x800e_1850 => keys::init_type(m),
        0x800e_19c4 => keys::show_type(m),
        0x800e_1a2c => {
            keys::settings_rows(m);
            0
        }
        0x800e_1ea0 => keys::settings_open(m),
        0x800e_1ed0 => keys::settings_input(m),
        0x800e_2298 => keys::analog_open(m),
        0x800e_2390 => keys::analog_page(m),
        0x800e_2970 => keys::buttons_open(m),
        0x800e_2ab4 => keys::button_page(m),
        0x800e_339c => keys::single_axis_calibration(m),
        0x800e_3adc => keys::stick_calibration(m),
        0x800e_4578 => keys::calibration_open(m),
        0x800e_4688 => keys::calibration(m),
        0x800e_46ec => keys::store_and_leave(m),
        0x800e_470c => keys::keys_open(m),
        0x800e_47ac => keys::keys_load(m),
        0x800e_47e0 => keys::keys_tick(m),
        // State 11, DEBRIEFING.
        0x800c_f358 => results::line(m, a0, a1, a2, a3),
        0x800d_191c => {
            results::record_results(m);
            0
        }
        0x800d_1ba4 => results::tally(m),
        0x800d_1f28 => results::draw_kinds(m, a0, a1),
        0x800d_2174 => results::draw_time(m, a0, a1),
        0x800d_233c => results::draw_area(m, a0, a1),
        0x800d_2410 => results::draw_markers(m, a0, a1),
        0x800d_27d0 => results::draw_trail(m, a0, a1),
        0x800d_29d8 => results::draw_map(m, a0, a1),
        0x800d_2bc8 => results::draw_results(m, a0, a1),
        0x800d_2c98 => results::results_open(m),
        0x800d_2e18 => results::results_tick(m),
        0x800d_31fc => results::results_close(m),
        _ => return None,
    })
}

// ---- the OPTIONS menu (mode 0) ----------------------------------------------------------

/// 800DC8F4: visual slot `slot` at (-t*t, 0) while `t` > 0, else (0, 0).
pub fn slide_left(m: &mut Machine, slot: u32, t: u32) -> u32 {
    m.push_frame(0x18);
    let (x, y) = if (t as i32) > 0 {
        (mul(0u32.wrapping_sub(t), t), 0)
    } else {
        (0, 0)
    };
    let v0 = external(m, SET_VISUAL_OFFSET, [slot, x, y, 0]);
    m.pop_frame(0x18);
    v0
}

/// 800DC938: visual slot `slot` at (t*t, 0) while `t` > 0, else (0, 0).
pub fn slide_right(m: &mut Machine, slot: u32, t: u32) -> u32 {
    m.push_frame(0x18);
    let x = if (t as i32) > 0 { mul(t, t) } else { 0 };
    let v0 = external(m, SET_VISUAL_OFFSET, [slot, x, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 800DC974: visual slot `slot` at (0, t*t) while `t` > 0, else (0, 0).
pub fn slide_down(m: &mut Machine, slot: u32, t: u32) -> u32 {
    m.push_frame(0x18);
    let y = if (t as i32) > 0 { mul(t, t) } else { 0 };
    let v0 = external(m, SET_VISUAL_OFFSET, [slot, 0, y, 0]);
    m.pop_frame(0x18);
    v0
}

/// 800DC9B0: place the menu panels for slide timer `t`: the header (slot 2,
/// when flag 1), the title (slot 1, when flag 4; else the selected cell's
/// style), the four cells (slots 3..7) and the footer (slot 8); then the
/// text: line 0x9F + choice.
pub fn place_panels(m: &mut Machine, t: u32) {
    m.push_frame(0x20);
    let flags = control_halfu(m, 8);
    if flags & 1 != 0 {
        slide_left(m, 2, t.wrapping_add(8));
    }
    let block = control(m);
    if lhu(m, block + 8) & 4 != 0 {
        slide_down(m, 1, t.wrapping_add(8));
    } else {
        let choice = lh(m, block + 4);
        external(m, SET_VISUAL_STYLE, [choice.wrapping_add(5), 1, 0, 0]);
    }
    slide_left(m, 3, t);
    let later = t.wrapping_add(8);
    let soon = t.wrapping_add(4);
    slide_left(m, 4, later);
    slide_left(m, 5, soon);
    slide_left(m, 6, later);
    slide_left(m, 7, soon);
    slide_right(m, 8, later);
    external(m, CLEAR_TEXT, [0; 4]);
    let choice = control_half(m, 4);
    external(m, SHOW_TEXT, [choice.wrapping_add(0x9f), 1, 0, 0]);
    m.pop_frame(0x20);
}

/// 800DCAAC (mode 0, step 0): open the menu: visual page 14; on a return
/// from a screen that set context +0x964 bit 6, select MUSIC PLAYER (3)
/// and restore the background; styles of slots 1..4, the hub choice and
/// the cells; slide timer 18; slots 3..8 off screen. Returns 2.
pub fn menu_open(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    external(m, SELECT_VISUAL_PAGE, [0xe, 0, 0, 0]);
    let ctx = context(m);
    let flags = m.lw(ctx + 0x964);
    if flags & 0x40 != 0 {
        m.sw(ctx + 0x964, flags & !0x40);
        let block = control(m);
        sh(m, block + 4, 3);
        let bits = m.lw(ctx + 0x970) & !0x40;
        m.sw(ctx + 0x970, bits);
        external(m, SET_VISUAL_OFFSET, [1, 0, 0, 0]);
        external(m, BACKGROUND, [2, 0, 0, 0]);
        set_scene_mode(m, 2);
        external(m, SCENE_COLOR, [0x8f, 0x87, 0x53, 0]);
    }
    for slot in 1..5 {
        external(m, SET_VISUAL_STYLE, [slot, 4, 0, 0]);
    }
    let hub = work(m);
    let choice = lh(m, hub + 6);
    external(m, SET_VISUAL_STYLE, [choice.wrapping_add(1), 3, 0, 0]);
    if control_halfu(m, 8) & 1 != 0 {
        sound(m, 0x5a);
        external(m, SET_VISUAL_OFFSET, [2, 0xffff_febc, 0, 0]);
    }
    let block = control(m);
    sh(m, block + 6, 0x12);
    for slot in 5..9 {
        external(m, SET_VISUAL_STYLE, [slot, 2, 0, 0]);
    }
    external(m, SET_VISUAL_STYLE, [9, 1, 0, 0]);
    let ctx = context(m);
    if m.lw(ctx + 0x974) & 2 == 0 {
        external(m, SET_VISUAL_STYLE, [8, 3, 0, 0]);
    }
    for slot in 3..9 {
        external(m, SET_VISUAL_OFFSET, [slot, 0xffff_febc, 0, 0]);
    }
    m.pop_frame(0x20);
    2
}

/// 800DCC48 (mode 0, step 1): slide the panels in (timer 18 down to -8;
/// sound 0x5C at the end when flag 1). Returns 1 while sliding, then 2.
pub fn menu_slide_in(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let t = control_half(m, 6);
    place_panels(m, t);
    let block = control(m);
    let t = lh(m, block + 6);
    let raw = lhu(m, block + 6);
    let result = if !slt_imm(t, -7) {
        sh(m, block + 6, raw.wrapping_sub(1));
        1
    } else {
        if lhu(m, block + 8) & 1 != 0 {
            sound(m, 0x5c);
        }
        2
    };
    m.pop_frame(0x18);
    result
}

/// 800DCCC4 (mode 0, step 3, and modes 3/4): slide the panels out (timer up
/// to 18). Returns 1 while sliding, then 0x20 (flag 2: back) or 0x10.
pub fn menu_slide_out(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let t = control_half(m, 6);
    place_panels(m, t);
    let block = control(m);
    let t = lh(m, block + 6);
    let raw = lhu(m, block + 6);
    let result = if slt_imm(t, 0x12) {
        sh(m, block + 6, raw.wrapping_add(1));
        1
    } else if lhu(m, block + 8) & 2 != 0 {
        0x20
    } else {
        0x10
    };
    m.pop_frame(0x18);
    result
}

fn slt_imm(value: u32, imm: i32) -> bool {
    (value as i32) < imm
}

/// 800DCD34 (mode 0, step 2): the menu. CROSS confirms (sound 0x3C; with
/// sound 0x5B and flag 1 unless MUSIC PLAYER; BACK TO TITLE needs START and
/// sets flag 4); TRIANGLE goes back (flag 2, sound 0x3B); the held
/// directions (context +0x130: 0xC left/right, 3 up/down) move in the
/// grid, never onto the locked MUSIC PLAYER cell (sound 0x3D). Returns 2
/// when leaving, else 1.
pub fn menu_select(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let ctx = context(m);
    let block = control(m);
    let unlocked = m.lw(ctx + 0x974);
    let mut choice = lh(m, block + 4);
    external(m, CLEAR_TEXT, [0; 4]);
    let unlocked = unlocked & 2;
    let block = control(m);
    let shown = lh(m, block + 4);
    external(m, SHOW_TEXT, [shown.wrapping_add(0x9f), 1, 0, 0]);
    let ctx = context(m);
    let buttons = m.lw(ctx + 0x12c);
    let confirm = (buttons & 0x40 != 0 && choice == 2) || (buttons & 0x10 != 0 && choice != 2);
    let result = if confirm {
        sound(m, 0x3c);
        let block = control(m);
        if lh(m, block + 4) != 3 {
            let flags = lhu(m, block + 8) | 1;
            sh(m, block + 8, flags);
            sound(m, 0x5b);
        } else {
            let flags = lhu(m, block + 8) & 0xfffe;
            sh(m, block + 8, flags);
        }
        let block = control(m);
        if lh(m, block + 4) == 2 {
            let flags = lhu(m, block + 8) | 4;
            sh(m, block + 8, flags);
        }
        external(m, SET_VISUAL_STYLE, [9, 0, 0, 0]);
        external(m, SET_VISUAL_STYLE, [7, 2, 0, 0]);
        2
    } else {
        let ctx = context(m);
        if m.lw(ctx + 0x12c) & 0x20 != 0 {
            let block = control(m);
            let flags = (lhu(m, block + 8) & 0xfffe) | 2;
            sh(m, block + 8, flags);
            sound(m, 0x3b);
            2
        } else {
            let held = m.lw(ctx + 0x130);
            let mut moved = 0;
            if held & 0xc != 0 {
                if !(choice & 2 != 0 && unlocked == 0) {
                    choice ^= 1;
                    moved += 1;
                }
            } else if held & 3 != 0 && !(choice & 1 != 0 && unlocked == 0) {
                choice ^= 2;
                moved += 1;
            }
            let block = control(m);
            choice &= 3;
            sh(m, block + 4, choice);
            if moved != 0 {
                sound(m, 0x3d);
            }
            for slot in 5..9 {
                external(m, SET_VISUAL_STYLE, [slot, 2, 0, 0]);
            }
            external(m, SET_VISUAL_STYLE, [choice.wrapping_add(5), 1, 0, 0]);
            if unlocked == 0 {
                external(m, SET_VISUAL_STYLE, [8, 3, 0, 0]);
            }
            1
        }
    };
    m.pop_frame(0x28);
    result
}

// ---- BACK TO TITLE (mode 3) and MUSIC PLAYER (mode 4) ------------------------------

/// 800DCF70 (mode 3, step 0): slide the menu out; 1 while sliding, else 2.
pub fn title_slide_out(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if menu_slide_out(m) == 1 { 1 } else { 2 };
    m.pop_frame(0x18);
    result
}

/// 800DCFA4 (mode 3, step 1): 50-frame fade timer, background 3, scene
/// color (0x8F, 0x87, 0x53). Returns 2.
pub fn title_fade_start(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = control(m);
    sh(m, block + 6, 0x32);
    external(m, BACKGROUND, [3, 0, 0, 0]);
    external(m, SCENE_ENTER, [0; 4]);
    external(m, SCENE_COLOR, [0x8f, 0x87, 0x53, 0]);
    m.pop_frame(0x18);
    2
}

/// `x / 50` as compiled: `mult` by 0x51EB851F, `mfhi`, `sra 4`, minus the
/// sign.
fn div50(x: u32) -> u32 {
    sra(mulhi(x, 0x51eb_851f), 4).wrapping_sub(sra(x, 31))
}

/// 800DCFEC (mode 3, step 2): fade the scene color to black over the timer;
/// at 0, black and result 0x40 (leave for the title), else 0.
pub fn title_fade(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let t = control_half(m, 6);
    let result = if (t as i32) <= 0 {
        external(m, SCENE_COLOR, [0, 0, 0, 0]);
        0x40
    } else {
        let r = ((t << 3).wrapping_add(t) << 4).wrapping_sub(t);
        let g = ((t << 4).wrapping_add(t) << 3).wrapping_sub(t);
        let b = ((((t << 2).wrapping_add(t)) << 2).wrapping_add(t) << 2).wrapping_sub(t);
        external(m, SCENE_COLOR, [div50(r), div50(g), div50(b), 0]);
        let block = control(m);
        let t = lhu(m, block + 6);
        sh(m, block + 6, t.wrapping_sub(1));
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800DD0CC (mode 4, step 0): slide the menu out, then timer 3, background
/// 0, scene mode 0, slots 2/1 off screen, styles of slots 3..8 cleared,
/// black. 1 while sliding, else 2.
pub fn music_start(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if menu_slide_out(m) == 1 {
        1
    } else {
        let block = control(m);
        sh(m, block + 6, 3);
        external(m, BACKGROUND, [0, 0, 0, 0]);
        set_scene_mode(m, 0);
        external(m, SET_VISUAL_OFFSET, [2, 0xffff_feb6, 0, 0]);
        external(m, SET_VISUAL_OFFSET, [1, 0xffff_feb6, 0, 0]);
        for slot in 3..9 {
            external(m, SET_VISUAL_STYLE, [slot, 0, 0, 0]);
        }
        external(m, SCENE_COLOR, [0, 0, 0, 0]);
        2
    };
    m.pop_frame(0x18);
    result
}

/// 800DD170 (mode 4, step 1): count the timer down; at 0, 0x80 (leave for
/// the music player), else 1.
pub fn music_wait(m: &mut Machine) -> u32 {
    let block = control(m);
    let t = lhu(m, block + 6).wrapping_sub(1);
    sh(m, block + 6, t);
    if t << 16 == 0 { 0x80 } else { 1 }
}

// ---- state 10 entry points and the mode machine ----------------------------------------

/// 800DD1A4 (state 10, step 0): clear the control block and draw the hub
/// panels (ov2hub 800D170C, page 3). Returns 1.
pub fn initialize(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let block = control(m);
    sh(m, block, 0);
    sh(m, block + 2, 0);
    sh(m, block + 4, 0);
    sh(m, block + 8, 0);
    draw_hub_panels(m);
    m.pop_frame(0x20);
    1
}

fn draw_hub_panels(m: &mut Machine) {
    let high = work(m).wrapping_add(WORK_HIGH);
    let cursor = m.lw(high + 0x94);
    let next = external(m, HUB_PANELS, [cursor, 3, 1, 0]);
    let high = work(m).wrapping_add(WORK_HIGH);
    m.sw(high + 0x94, next);
}

/// 800DD214 (state 10, step 1): run the mode machine; in mode 0 also draw
/// the hub panels. Returns the machine's result.
pub fn tick(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let block = control(m);
    let result = run_modes(m, block);
    if control_half(m, 0) == 0 {
        draw_hub_panels(m);
    }
    m.pop_frame(0x20);
    result
}

/// 800E50B8: one step of the mode machine `block` {mode, step, choice}
/// (table 0x800F67A4). Step results: 2 next step; 0x10 the chosen screen
/// (mode = choice + 1); 4 or 8 back to the menu; 0x20 leave backwards
/// (returns 0x200); 0x40 leave for the title (context +0x964 |= 4, +0x970
/// &= ~0x10; returns 0x20); 0x80 the music player (context +0x394 = +0x390,
/// +0x964 |= 0x40; returns 0x10). Anything else returns 0.
pub fn run_modes(m: &mut Machine, block: u32) -> u32 {
    m.push_frame(0x18);
    let mode = lh(m, block);
    let list = m.lw(MODES.wrapping_add(mode << 2));
    let step = lh(m, block + 2);
    let routine = m.lw(list.wrapping_add(step << 2));
    let result = call_pointer(m, routine, [0; 4]);
    let v0 = match result {
        0x10 => {
            let choice = lh(m, block + 4);
            sh(m, block + 2, 0);
            match choice {
                0 => sh(m, block, 1),
                1 => sh(m, block, 2),
                2 => sh(m, block, 3),
                3 => sh(m, block, 4),
                _ => {}
            }
            0
        }
        4 | 8 => {
            sh(m, block, 0);
            sh(m, block + 2, 0);
            0
        }
        2 => {
            let step = lhu(m, block + 2);
            sh(m, block + 2, step.wrapping_add(1));
            0
        }
        0x20 => 0x200,
        0x40 => {
            let ctx = context(m);
            let flags = m.lw(ctx + 0x964) | 4;
            m.sw(ctx + 0x964, flags);
            let bits = m.lw(ctx + 0x970) & !0x10;
            m.sw(ctx + 0x970, bits);
            0x20
        }
        0x80 => {
            let ctx = context(m);
            let first = lhu(m, ctx + 0x390);
            let flags = m.lw(ctx + 0x964) | 0x40;
            let second = lhu(m, ctx + 0x392);
            sh(m, ctx + 0x394, first);
            sh(m, ctx + 0x396, second);
            m.sw(ctx + 0x964, flags);
            0x10
        }
        _ => 0,
    };
    m.pop_frame(0x18);
    v0
}

/// 800E9854: scene block `[0x800F9890]` +0x18 = `mode`, +0x24 = 0. Returns
/// the scene block.
pub fn set_scene_mode(m: &mut Machine, mode: u32) -> u32 {
    let scene = m.lw(0x800f_9890);
    m.sw(scene + 0x18, mode);
    m.sw(scene + 0x24, 0);
    scene
}

/// `jalr` through a table: this module's routines run natively, anything
/// else goes through [`external`].
pub(crate) fn call_pointer(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    match dispatch(m, target, args) {
        Some(v0) => v0,
        None => external(m, target, args),
    }
}
