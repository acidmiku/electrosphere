//! Overlay 2 inner state 10 (hub choice 3), the OPTIONS screen: its mode 2,
//! GAME CONFIG, translated from `analysis/overlays/overlay_002`. The
//! state's update (800DD214, `ov2_options`) runs 800E50B8 on the block
//! `[0x800F6060]` (+0 mode, +2 step, +4 choice as signed halfwords), which
//! calls `[[0x800F67A4 + mode*4] + step*4]` (the grid and the other modes
//! are `ov2_options`'s). This file holds mode 2's row 0x800F6798 and
//! everything only it reaches:
//!
//! - 800E4978 (queue the menu textures), 800E4B10 (poll them), 800E4D6C (the
//!   five-item menu, phases 0..6 at 0x800F97F8, jump table 0x800CB200). The
//!   selected item runs its row `[0x800F6784 + cursor*4]` step by step
//!   (800E4CF0) until a step returns 3: item 0 sound (800E72C4, 800E73DC,
//!   with the decoded-audio level 800D0D30/800D0DB8), 1 colour bars
//!   (800E594C, 800E59E8), 2 screen position (800E5328, 800E53C8), 3 option
//!   rows (800E6504, 800E66A0), 4 language (800E5D50, 800E5ECC).
//!
//! The items' phase jump tables (0x800CB234, 0x800CB248, 0x800CB284,
//! 0x800CB2E4, 0x800CB324) are translated as `match`es with the original
//! bounds checks. The menu's globals live in the overlay image from
//! 0x800F97F8; the panel slide timers are the words from 0x800F9840.
//! Conventions as in [`crate::ov2_misc`]: routines owned elsewhere
//! (`ov2_core`, `ov2_options`, `ov2_aircraft`, resident) are called by original address through
//! [`Calls`], and original frames are pushed wherever a routine calls out.
use crate::common::CONTEXT;
use crate::machine::{Machine, SmallData};
use crate::ov2_aircraft::{
    CLEAR_LABELS, Calls, LABEL, OFFSET, PAGE, SCENE, SCENE_COLOUR, SOUND, STYLE, div_magic,
};

// ---- routines owned elsewhere, called by original address ----------------

/// 800CE7E4 (`ov2_core`, loader): queue a TIM list (a0 list, a1 capacity,
/// a2 batch size).
const QUEUE_TEXTURES: u32 = 0x800c_e7e4;
/// 800CE844 (`ov2_core`, loader): process the next batch of the texture
/// queue; 1 once the list is done.
const POLL_TEXTURES: u32 = 0x800c_e844;
/// 800D0C58 / 800D0C68 / 800D0C88 (`ov2_core`): select stream `a0`, start
/// it, stop it.
const STREAM_SELECT: u32 = 0x800d_0c58;
const STREAM_START: u32 = 0x800d_0c68;
const STREAM_STOP: u32 = 0x800d_0c88;
/// 800F1BA0 (`ov2_core`): the current visual ordering table.
const CURRENT_OT: u32 = 0x800f_1ba0;
/// 8002B900 (resident): store the CD audio mode `a0` (sound block +0x160).
const AUDIO_MODE: u32 = 0x8002_b900;
/// 8002B934 (resident): apply the sound block's mode and volume.
const AUDIO_APPLY: u32 = 0x8002_b934;
/// 8009F358 / 8009F7D8 / 800A73C0 (resident SDK): SpuReadDecodedData,
/// SpuIsTransferCompleted, GetODE.
const SPU_READ_DECODED: u32 = 0x8009_f358;
const SPU_TRANSFER_DONE: u32 = 0x8009_f7d8;
const GET_ODE: u32 = 0x800a_73c0;

// ---- data --------------------------------------------------------------------

/// `[gp+0x2C4]`: resident display block (+0 flags, +8/+0xC screen offset).
const DISPLAY: u32 = 0x2c4;
/// 0x800F5394: pointer to the stream block (decoded CD audio at +4).
const STREAM: u32 = 0x800f_5394;
/// 0x800F6734 / 0x800F6748: the menu's five panel slots and item captions.
const MENU_SLOTS: u32 = 0x800f_6734;
const CAPTIONS: u32 = 0x800f_6748;
/// 0x800F6784: the items' step rows, indexed by the cursor.
pub const ITEM_ROWS: u32 = 0x800f_6784;
/// 0x800F67B8 / 0x800F67CC: language panel slots and language ids.
const LANGUAGE_SLOTS: u32 = 0x800f_67b8;
const LANGUAGES: u32 = 0x800f_67cc;
/// 0x800F67E0 / 0x800F67F8: option panel slots (6) and sound panel slots
/// (2).
const OPTION_SLOTS: u32 = 0x800f_67e0;
const SOUND_SLOTS: u32 = 0x800f_67f8;
/// Overlay rodata: -1-terminated slot lists the routines copy to their
/// stack (screen position, language, option rows, sound), and the option
/// rows' cursor slots (1, 4, 8).
const POSITION_DIM: u32 = 0x800c_b21c;
const LANGUAGE_DIM: u32 = 0x800c_b264;
const OPTION_DIM: u32 = 0x800c_b2a4;
const OPTION_ROW_SLOTS: u32 = 0x800c_b2d8;
const SOUND_DIM: u32 = 0x800c_b304;

/// Menu: phase (800E4D6C), panel 0xB slide timer, item step, list active
/// flag (1 while the list has the input), cursor (0..4).
const MENU_PHASE: u32 = 0x800f_97f8;
const PANEL_TIMER: u32 = 0x800f_97fc;
const ITEM_STEP: u32 = 0x800f_9800;
const LIST_ACTIVE: u32 = 0x800f_9804;
const CURSOR: u32 = 0x800f_9808;
/// Screen position: phase, saved offset x and y.
const POSITION_PHASE: u32 = 0x800f_9810;
const SAVED_X: u32 = 0x800f_9814;
const SAVED_Y: u32 = 0x800f_9818;
/// Colour bars: phase.
const BARS_PHASE: u32 = 0x800f_9820;
/// Language: blink counter, entry index, cursor, phase.
const LANGUAGE_BLINK: u32 = 0x800f_9828;
const LANGUAGE_ENTRY: u32 = 0x800f_982c;
const LANGUAGE_CURSOR: u32 = 0x800f_9830;
const LANGUAGE_PHASE: u32 = 0x800f_9834;
/// Option rows: blink counter; entry values of rows 0 and 2 (bytes).
const OPTION_BLINK: u32 = 0x800f_9838;
const OPTION_ENTRY_0: u32 = 0x800f_983c;
const OPTION_ENTRY_2: u32 = 0x800f_983d;
/// 0x800F9840: panel slide timers (words).
const TIMERS: u32 = 0x800f_9840;
/// Option rows: row 0 value (byte), row cursor (word), row 1 entry value
/// (byte), row 2 value (byte), phase (word), row 1 value (byte).
const OPTION_VALUE_0: u32 = 0x800f_9858;
const OPTION_ROW: u32 = 0x800f_985c;
const OPTION_ENTRY_1: u32 = 0x800f_9860;
const OPTION_VALUE_2: u32 = 0x800f_9861;
const OPTION_PHASE: u32 = 0x800f_9864;
const OPTION_VALUE_1: u32 = 0x800f_9868;
/// Sound: level meter width, pointers to context +0x39E (flags), +0x39C
/// and +0x39D (volumes), phase, stereo flag, row cursor, saved volumes and
/// flag (bytes).
const METER: u32 = 0x800f_9870;
const SOUND_FLAGS: u32 = 0x800f_9874;
const VOLUME_0: u32 = 0x800f_9878;
const SOUND_PHASE: u32 = 0x800f_987c;
const VOLUME_1: u32 = 0x800f_9880;
const STEREO: u32 = 0x800f_9884;
const SOUND_ROW: u32 = 0x800f_9888;
const SAVED_VOLUME_0: u32 = 0x800f_988c;
const SAVED_VOLUME_1: u32 = 0x800f_988d;
const SAVED_STEREO: u32 = 0x800f_988e;

// ---- shared helpers ----------------------------------------------------------

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn style(m: &mut Machine, c: &mut dyn Calls, slot: u32, value: u32) {
    c.call(m, STYLE, [slot, value, 0, 0]);
}

fn offset(m: &mut Machine, c: &mut dyn Calls, slot: u32, x: u32, y: u32) {
    c.call(m, OFFSET, [slot, x, y, 0]);
}

fn label(m: &mut Machine, c: &mut dyn Calls, index: u32) {
    c.call(m, LABEL, [index, 1, 0, 0]);
}

fn clear_labels(m: &mut Machine, c: &mut dyn Calls) {
    c.call(m, CLEAR_LABELS, [0; 4]);
}

fn sound(m: &mut Machine, c: &mut dyn Calls, id: u32) {
    c.call(m, SOUND, [id, 0, 0, 0]);
}

fn page(m: &mut Machine, c: &mut dyn Calls, index: u32) {
    c.call(m, PAGE, [index, 0, 0, 0]);
}

fn colour(m: &mut Machine, c: &mut dyn Calls, red: u32, green: u32, blue: u32) {
    c.call(m, SCENE_COLOUR, [red, green, blue, 0]);
}

/// Word `index` of the table at `table`.
fn word(m: &mut Machine, table: u32, index: u32) -> u32 {
    m.lw((index << 2).wrapping_add(table))
}

/// Negative values clamp to 0 (`bgez x; x = 0`).
fn clamp(x: u32) -> u32 {
    if (x as i32) < 0 { 0 } else { x }
}

/// `-t*t` for t > 0, else 0 (a panel's x while it slides).
fn slide_x(t: u32) -> u32 {
    if (t as i32) > 0 {
        t.wrapping_mul(t).wrapping_neg()
    } else {
        0
    }
}

/// Style 2 for each slot of the -1-terminated `count`-word list at `list`
/// (the original copies the list to its frame first, then walks it).
fn dim_slots(m: &mut Machine, c: &mut dyn Calls, list: u32, count: u32) {
    let slots: Vec<u32> = (0..count).map(|i| m.lw(list + 4 * i)).collect();
    for &slot in slots.iter().take_while(|&&slot| slot != u32::MAX) {
        style(m, c, slot, 2);
    }
}

/// Panel timers 0x800F9840.. = `values`.
fn set_timers(m: &mut Machine, values: &[u32]) {
    for (i, &value) in values.iter().enumerate() {
        m.sw(TIMERS + 4 * i as u32, value);
    }
}

/// Each panel of `slots` at x = -t*t for its timer.
fn place_panels(m: &mut Machine, c: &mut dyn Calls, slots: u32, count: u32) {
    for i in 0..count {
        let t = m.lw(TIMERS + 4 * i);
        let slot = m.lw(slots + 4 * i);
        offset(m, c, slot, t.wrapping_mul(t).wrapping_neg(), 0);
    }
}

/// Count each panel timer down and place its slot (x = -t*t, 0 once at
/// rest). Returns the number of panels at rest.
fn slide_in(m: &mut Machine, c: &mut dyn Calls, slots: u32, count: u32) -> u32 {
    let mut settled = 0;
    for i in 0..count {
        let at = TIMERS + 4 * i;
        let t = m.lw(at).wrapping_sub(1);
        m.sw(at, t);
        let slot = m.lw(slots + 4 * i);
        offset(m, c, slot, slide_x(t), 0);
        if (t as i32) <= 0 {
            settled += 1;
        }
    }
    settled
}

/// Count each panel timer up and place its slot. Returns the number of
/// panels whose timer reached 0x11.
fn slide_out(m: &mut Machine, c: &mut dyn Calls, slots: u32, count: u32) -> u32 {
    let mut out = 0;
    for i in 0..count {
        let at = TIMERS + 4 * i;
        let t = m.lw(at).wrapping_add(1);
        m.sw(at, t);
        let slot = m.lw(slots + 4 * i);
        offset(m, c, slot, slide_x(t), 0);
        if (t as i32) >= 0x11 {
            out += 1;
        }
    }
    out
}

/// Panel slot 0xB (x = t*t, 0 when t <= 0) after the timer moves by
/// `step` (-1 in, +1 out).
fn panel_slide(m: &mut Machine, c: &mut dyn Calls, step: u32) {
    let t = m.lw(PANEL_TIMER).wrapping_add(step);
    m.sw(PANEL_TIMER, t);
    let x = if (t as i32) > 0 { t.wrapping_mul(t) } else { 0 };
    offset(m, c, 0xb, x, 0);
}

/// The caption label of the item under the cursor.
fn caption(m: &mut Machine, c: &mut dyn Calls) {
    let cursor = m.lw(CURSOR);
    let index = word(m, CAPTIONS, cursor);
    label(m, c, index);
}

// ---- stream level ------------------------------------------------------------

/// 800D0D30: 100 decoded CD audio samples for `side` (0 left, else right)
/// of the stream block: SpuReadDecodedData(+4, 5), SpuIsTransferCompleted(1),
/// then +0xCC/+0x4CC when the first returned 0, else +4/+0x404.
pub fn decoded_half_800d0d30(m: &mut Machine, c: &mut dyn Calls, side: u32) -> u32 {
    m.push_frame(0x20);
    let stream = m.lw(STREAM);
    let half = c.call(m, SPU_READ_DECODED, [stream.wrapping_add(4), 5, 0, 0]);
    c.call(m, SPU_TRANSFER_DONE, [1, 0, 0, 0]);
    let stream = m.lw(STREAM);
    let (left, right) = if half == 0 {
        (stream.wrapping_add(0xcc), stream.wrapping_add(0x4cc))
    } else {
        (stream.wrapping_add(4), stream.wrapping_add(0x404))
    };
    m.pop_frame(0x20);
    if side == 0 { left } else { right }
}

/// 800D0DB8: CD audio level: the mean absolute value of 100 left and 100
/// right samples, averaged over both channels (a sign-extended halfword).
pub fn stream_level_800d0db8(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let mut left = decoded_half_800d0d30(m, c, 0);
    let mut right = decoded_half_800d0d30(m, c, 1);
    let mut sum_left = 0u32;
    let mut sum_right = 0u32;
    for _ in 0..100 {
        let l = i32::from(m.lh(left));
        left = left.wrapping_add(2);
        let r = i32::from(m.lh(right));
        right = right.wrapping_add(2);
        sum_left = sum_left.wrapping_add(l.unsigned_abs());
        sum_right = sum_right.wrapping_add(r.unsigned_abs());
    }
    let mean_left = div_magic(sum_left, 0x51eb_851f, 5, false);
    let mean_right = div_magic(sum_right, 0x51eb_851f, 5, false);
    let sum = mean_left.wrapping_add(mean_right);
    let halved = sum.wrapping_add(sum >> 31);
    m.pop_frame(0x20);
    ((halved << 15) as i32 >> 16) as u32
}

// ---- mode 2: the menu --------------------------------------------------------

/// 800E4978 (mode 2 step 0): cursor, item step and phase 0, list active;
/// queue the menu textures (scene +0x200C4, batch 10), clear the labels.
/// Returns 2.
pub fn menu_load_800e4978(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    m.sw(CURSOR, 0);
    m.sw(ITEM_STEP, 0);
    m.sw(MENU_PHASE, 0);
    m.sw(LIST_ACTIVE, 1);
    let scene = m.lw(SCENE);
    let textures = m.lw(scene + 0x2_00c4);
    c.call(m, QUEUE_TEXTURES, [textures, 0x9000, 0xa, 0]);
    clear_labels(m, c);
    m.pop_frame(0x18);
    2
}

/// 800E4B10 (mode 2 step 1): poll the texture queue; 2 once done, else 1.
pub fn menu_poll_textures_800e4b10(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let done = c.call(m, POLL_TEXTURES, [0; 4]);
    m.pop_frame(0x18);
    if done == 1 { 2 } else { 1 }
}

/// The list's styles: slots 6, 7, 1..5 dimmed (2), the cursor's slot lit.
fn list_styles(m: &mut Machine, c: &mut dyn Calls) {
    for slot in [6, 7, 1, 2, 3, 4, 5] {
        style(m, c, slot, 2);
    }
    let cursor = m.lw(CURSOR);
    style(m, c, cursor.wrapping_add(1), 1);
}

/// 800E49DC: show the menu: page 0x1B, the list styles, the five panels
/// off-screen (timers 0x11, 0x14, 0x17, 0x13, 0x16), panel 0xB at x 0x121
/// (timer 0x11), labels cleared.
pub fn menu_show_800e49dc(m: &mut Machine, c: &mut dyn Calls) {
    m.push_frame(0x20);
    page(m, c, 0x1b);
    list_styles(m, c);
    set_timers(m, &[0x11, 0x14, 0x17, 0x13, 0x16]);
    place_panels(m, c, MENU_SLOTS, 5);
    m.sw(PANEL_TIMER, 0x11);
    offset(m, c, 0xb, 0x121, 0);
    clear_labels(m, c);
    m.pop_frame(0x20);
}

/// 800E4B44: list input. Held bits 1/2 light the arrows (slots 6/7);
/// repeat bit 2 (down) / 1 (up) move the cursor around 0..4 (sound 0x3D)
/// and reset the item step; otherwise the caption, and confirm (0x10)
/// plays 0x3C and hands the input to the item. Returns 1.
pub fn menu_input_800e4b44(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x28);
    let ctx = context(m);
    let repeat = m.lw(ctx + 0x130);
    let held = m.lw(ctx + 0x128);
    list_styles(m, c);
    if held & 1 != 0 {
        style(m, c, 6, 1);
    }
    if held & 2 != 0 {
        style(m, c, 7, 1);
    }
    if repeat & 2 != 0 {
        sound(m, c, 0x3d);
        style(m, c, 7, 1);
        let cursor = m.lw(CURSOR).wrapping_add(1);
        m.sw(CURSOR, cursor);
        if cursor == 5 {
            m.sw(CURSOR, 0);
        }
        m.sw(ITEM_STEP, 0);
    } else if repeat & 1 != 0 {
        sound(m, c, 0x3d);
        style(m, c, 6, 1);
        let cursor = m.lw(CURSOR).wrapping_sub(1);
        m.sw(CURSOR, cursor);
        if (cursor as i32) < 0 {
            m.sw(CURSOR, 4);
        }
        m.sw(ITEM_STEP, 0);
    } else {
        clear_labels(m, c);
        caption(m, c);
        let ctx = context(m);
        if m.lw(ctx + 0x12c) & 0x10 != 0 {
            sound(m, c, 0x3c);
            m.sw(LIST_ACTIVE, 0);
        }
    }
    m.pop_frame(0x28);
    1
}

/// 800E4CF0: run the selected item's current step
/// `[[0x800F6784 + cursor*4] + step*4]`; result 2 advances the step.
/// Returns the step's result.
pub fn menu_item_step_800e4cf0(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let cursor = m.lw(CURSOR);
    let step = m.lw(ITEM_STEP);
    let row = word(m, ITEM_ROWS, cursor);
    let routine = word(m, row, step);
    // a0 still holds the row table's address at the `jalr`.
    let result = c.call(m, routine, [ITEM_ROWS, 0, 0, 0]);
    if result == 2 {
        let step = m.lw(ITEM_STEP);
        m.sw(ITEM_STEP, step.wrapping_add(1));
    }
    m.pop_frame(0x18);
    result
}

/// 800E4D6C (mode 2 step 2): the menu. Phases (0x800F97F8): 0 show, 1, 2
/// slide the panels in, 3 list input (cancel 0x20 plays 0x3B; cancel or
/// confirm slides out) or the item's steps (result 3 returns to the list),
/// 4 slide out (then 5 when the list was active, else 3 to run the item),
/// 5, 6 returns 8 (leave). Returns 1 otherwise.
pub fn menu_update_800e4d6c(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let phase = m.lw(MENU_PHASE);
    m.push_frame(0x28);
    let result = match phase {
        0 => {
            menu_show_800e49dc(m, c);
            caption(m, c);
            m.sw(MENU_PHASE, 1);
            1
        }
        1 => {
            m.sw(MENU_PHASE, 2);
            1
        }
        2 => {
            if slide_in(m, c, MENU_SLOTS, 5) >= 5 {
                m.sw(MENU_PHASE, 3);
            }
            panel_slide(m, c, u32::MAX);
            caption(m, c);
            1
        }
        3 => {
            if m.lw(LIST_ACTIVE) != 0 {
                menu_input_800e4b44(m, c);
                let ctx = context(m);
                let pressed = m.lw(ctx + 0x12c);
                if pressed & 0x20 != 0 {
                    sound(m, c, 0x3b);
                    m.sw(MENU_PHASE, 4);
                } else if pressed & 0x10 != 0 {
                    m.sw(MENU_PHASE, 4);
                }
            } else if menu_item_step_800e4cf0(m, c) == 3 {
                m.sw(LIST_ACTIVE, 1);
                m.sw(ITEM_STEP, 0);
                m.sw(MENU_PHASE, 0);
            }
            1
        }
        4 => {
            if slide_out(m, c, MENU_SLOTS, 5) >= 5 {
                let next = if m.lw(LIST_ACTIVE) != 0 { 5 } else { 3 };
                m.sw(MENU_PHASE, next);
            }
            panel_slide(m, c, 1);
            1
        }
        5 => {
            m.sw(MENU_PHASE, 6);
            1
        }
        6 => 8,
        _ => 1,
    };
    m.pop_frame(0x28);
    result
}

// ---- item 2: screen position -------------------------------------------------

/// 800E5288: style 2 for the screen-position slots (list 0x800CB21C).
pub fn position_dim_800e5288(m: &mut Machine, c: &mut dyn Calls) {
    m.push_frame(0x38);
    dim_slots(m, c, POSITION_DIM, 5);
    m.pop_frame(0x38);
}

/// Labels 0x96 and 0xA3 (context +0x124 bit 0x10) or 0x97.
fn position_labels(m: &mut Machine, c: &mut dyn Calls) {
    label(m, c, 0x96);
    let ctx = context(m);
    let index = if m.lw(ctx + 0x124) & 0x10 != 0 {
        0xa3
    } else {
        0x97
    };
    label(m, c, index);
}

/// 800E5328 (screen position step 0): phase 0, save the display offset
/// (+8, +0xC), black background, page 0x1C, the dimmed slots, the labels.
/// Returns 2.
pub fn position_init_800e5328(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let display = m.gw(DISPLAY);
    m.sw(POSITION_PHASE, 0);
    let x = m.lw(display + 8);
    m.sw(SAVED_X, x);
    let y = m.lw(display + 0xc);
    m.sw(SAVED_Y, y);
    colour(m, c, 0, 0, 0);
    page(m, c, 0x1c);
    position_dim_800e5288(m, c);
    clear_labels(m, c);
    position_labels(m, c);
    m.pop_frame(0x18);
    2
}

/// Screen position phase 2: arrows lit by the held bits; repeat bits 8/4
/// move x within -12..12, 2/1 move y within 8..24 (sound 0x2E); confirm
/// (0x10, sound 0x3C) or cancel (0x20, sound 0x3B, offset restored) go to
/// phase 3; a reset input (analog axis +0x68 above 0x80 when context
/// +0x124 bit 0x10, else button bit 0x2000 of +0x60) sets (-2, 16) with
/// sound 0x30. Returns 0.
fn position_adjust(m: &mut Machine, c: &mut dyn Calls, held: u32, repeat: u32) -> u32 {
    position_dim_800e5288(m, c);
    if held & 1 != 0 {
        style(m, c, 1, 1);
    }
    if held & 2 != 0 {
        style(m, c, 2, 1);
    }
    if held & 8 != 0 {
        style(m, c, 4, 1);
    }
    if held & 4 != 0 {
        style(m, c, 3, 1);
    }
    let step = 'step: {
        if repeat & 8 != 0 {
            let display = m.gw(DISPLAY);
            let x = m.lw(display + 8);
            if (x as i32) < 0xc {
                break 'step Some((display + 8, x.wrapping_add(1)));
            }
        }
        if repeat & 4 != 0 {
            let display = m.gw(DISPLAY);
            let x = m.lw(display + 8);
            if (x as i32) >= -0xb {
                break 'step Some((display + 8, x.wrapping_sub(1)));
            }
        }
        if repeat & 2 != 0 {
            let display = m.gw(DISPLAY);
            let y = m.lw(display + 0xc);
            if (y as i32) < 0x18 {
                break 'step Some((display + 0xc, y.wrapping_add(1)));
            }
        }
        if repeat & 1 != 0 {
            let display = m.gw(DISPLAY);
            let y = m.lw(display + 0xc);
            if (y as i32) >= 9 {
                break 'step Some((display + 0xc, y.wrapping_sub(1)));
            }
        }
        None
    };
    if let Some((at, value)) = step {
        m.sw(at, value);
        sound(m, c, 0x2e);
        return 0;
    }
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    if pressed & 0x10 != 0 {
        sound(m, c, 0x3c);
        m.sw(POSITION_PHASE, 3);
        return 0;
    }
    if pressed & 0x20 != 0 {
        sound(m, c, 0x3b);
        let display = m.gw(DISPLAY);
        let x = m.lw(SAVED_X);
        let y = m.lw(SAVED_Y);
        m.sw(POSITION_PHASE, 3);
        m.sw(display + 8, x);
        m.sw(display + 0xc, y);
        return 0;
    }
    let reset = if m.lw(ctx + 0x124) & 0x10 != 0 {
        i32::from(m.lh(ctx + 0x68)) >= 0x81
    } else {
        m.lhu(ctx + 0x60) & 0x2000 != 0
    };
    if reset {
        let display = m.gw(DISPLAY);
        let home = m.lw(display + 8) == (-2i32) as u32 && m.lw(display + 0xc) == 0x10;
        if !home {
            m.sw(display + 8, (-2i32) as u32);
            m.sw(display + 0xc, 0x10);
            sound(m, c, 0x30);
        }
    }
    0
}

/// 800E53C8 (screen position step 1): display flag 2 when GetODE is
/// nonzero; the labels; phases (0x800F9810, table 0x800CB234) 0, 1, 2
/// adjust, 3 background (143, 135, 83) and labels cleared, 4 returns 3.
/// Returns 0 otherwise.
pub fn position_update_800e53c8(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let ctx = context(m);
    let repeat = m.lw(ctx + 0x130);
    let held = m.lw(ctx + 0x128);
    if c.call(m, GET_ODE, [0; 4]) != 0 {
        let display = m.gw(DISPLAY);
        let flags = m.lw(display);
        m.sw(display, flags | 2);
    }
    clear_labels(m, c);
    position_labels(m, c);
    let result = match m.lw(POSITION_PHASE) {
        0 => {
            m.sw(POSITION_PHASE, 1);
            0
        }
        1 => {
            m.sw(POSITION_PHASE, 2);
            0
        }
        2 => position_adjust(m, c, held, repeat),
        3 => {
            colour(m, c, 0x8f, 0x87, 0x53);
            clear_labels(m, c);
            m.sw(POSITION_PHASE, 4);
            0
        }
        4 => 3,
        _ => 0,
    };
    m.pop_frame(0x20);
    result
}

// ---- item 1: colour bars -----------------------------------------------------

/// One POLY_G4 bar from x0 to x1, y 0xB6..0x15E, black at the top and
/// `bottom` at the bottom, linked into `ot` unless both x are 0.
fn gouraud_bar(m: &mut Machine, prim: u32, ot: u32, x0: u32, x1: u32, bottom: u32) {
    m.sw(prim.wrapping_add(4), 0x3800_0000);
    m.sw(prim.wrapping_add(0x14), bottom);
    m.sw(prim.wrapping_add(0x1c), bottom);
    m.sw(prim.wrapping_add(0x10), x1 | 0x00b6_0000);
    m.sw(prim.wrapping_add(8), x0 | 0x00b6_0000);
    m.sw(prim.wrapping_add(0xc), 0);
    m.sw(prim.wrapping_add(0x18), x0 | 0x015e_0000);
    m.sw(prim.wrapping_add(0x20), x1 | 0x015e_0000);
    if x0 != 0 || x1 != 0 {
        let tag = m.lw(ot) | 0x0800_0000;
        m.sw(prim, tag);
        m.sw(ot, prim & 0x00ff_ffff);
    }
}

/// 800E571C: four colour bars (red, green, blue, white) at `prim` into OT
/// entry `ot`, shifted left by t*t (timer 0x800F9840). Returns the next
/// primitive.
pub fn draw_colour_bars_800e571c(m: &mut Machine, prim: u32, ot: u32) -> u32 {
    let t = m.lw(TIMERS);
    let left = t.wrapping_mul(t).wrapping_neg();
    let mut prim = prim;
    for (x0, x1, bottom) in [
        (0x3c, 0x41, 0x0000_00ff),
        (0x48, 0x4d, 0x0000_ff00),
        (0x53, 0x58, 0x00ff_0000),
        (0x63, 0x68, 0x00ff_ffff),
    ] {
        let x0 = clamp(left.wrapping_add(x0));
        let x1 = clamp(left.wrapping_add(x1));
        gouraud_bar(m, prim, ot, x0, x1, bottom);
        prim = prim.wrapping_add(0x24);
    }
    prim
}

/// 800E571C on the scene's primitive area and OT entry; returns the next
/// primitive (the caller stores it back).
fn colour_bars(m: &mut Machine) -> u32 {
    let scene = m.lw(SCENE);
    let prim = m.lw(scene + 0x2_0094);
    let ot = m.lw(scene + 0x90);
    draw_colour_bars_800e571c(m, prim, ot)
}

/// 800E594C (colour bars step 0): phase 0, timers 0x11, page 0x1D, label
/// 0x90, panel 2 and panel 0xB off-screen. Returns 2.
pub fn bars_init_800e594c(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    m.sw(BARS_PHASE, 0);
    m.sw(TIMERS, 0x11);
    m.sw(PANEL_TIMER, 0x11);
    page(m, c, 0x1d);
    clear_labels(m, c);
    label(m, c, 0x90);
    let t = m.lw(TIMERS);
    offset(m, c, 2, t.wrapping_mul(t).wrapping_neg(), 0);
    let t = m.lw(PANEL_TIMER);
    offset(m, c, 0xb, t.wrapping_mul(t), 0);
    m.pop_frame(0x20);
    2
}

/// 800E59E8 (colour bars step 1): phases (0x800F9820, table 0x800CB248) 0,
/// 1, 2 slide in (labels 0x94, 0x95 when in), 3 show until confirm or
/// cancel (0x30: sound 0x3B, label 0x90), 4 slide out, 5, 6 returns 3. The
/// bars are drawn in phases 2..4. Returns 0 otherwise.
pub fn bars_update_800e59e8(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let phase = m.lw(BARS_PHASE);
    m.push_frame(0x20);
    let result = match phase {
        0 => {
            m.sw(BARS_PHASE, 1);
            0
        }
        1 => {
            m.sw(BARS_PHASE, 2);
            0
        }
        2 => {
            let t = m.lw(TIMERS).wrapping_sub(1);
            m.sw(TIMERS, t);
            offset(m, c, 2, slide_x(t), 0);
            panel_slide(m, c, u32::MAX);
            let next = colour_bars(m);
            let scene = m.lw(SCENE);
            let t = m.lw(TIMERS);
            m.sw(scene + 0x2_0094, next);
            if (t as i32) <= 0 {
                label(m, c, 0x94);
                label(m, c, 0x95);
                m.sw(BARS_PHASE, 3);
            }
            0
        }
        3 => {
            let next = colour_bars(m);
            let scene = m.lw(SCENE);
            let ctx = context(m);
            m.sw(scene + 0x2_0094, next);
            if m.lw(ctx + 0x12c) & 0x30 != 0 {
                sound(m, c, 0x3b);
                m.sw(BARS_PHASE, 4);
                clear_labels(m, c);
                label(m, c, 0x90);
            }
            0
        }
        4 => {
            let t = m.lw(TIMERS).wrapping_add(1);
            m.sw(TIMERS, t);
            offset(m, c, 2, slide_x(t), 0);
            panel_slide(m, c, 1);
            let next = colour_bars(m);
            let scene = m.lw(SCENE);
            let t = m.lw(TIMERS);
            m.sw(scene + 0x2_0094, next);
            if (t as i32) >= 0x11 {
                m.sw(BARS_PHASE, 5);
            }
            0
        }
        5 => {
            m.sw(BARS_PHASE, 6);
            0
        }
        6 => 3,
        _ => 0,
    };
    m.pop_frame(0x20);
    result
}

// ---- item 4: language --------------------------------------------------------

/// 800E5C98: style 2 for the language slots (list 0x800CB264).
pub fn language_dim_800e5c98(m: &mut Machine, c: &mut dyn Calls) {
    m.push_frame(0x40);
    dim_slots(m, c, LANGUAGE_DIM, 8);
    m.pop_frame(0x40);
}

/// 800E5D50 (language step 0): phase 0; cursor and entry index of the
/// context language (+0x38C) in the id table 0x800F67CC; page 0x1F, the
/// slots (cursor lit), the five panels off-screen (timers 0x16, 0x13,
/// 0x17, 0x14, 0x11), panel 0xB at x 0x121, labels 1..5. Returns 2.
pub fn language_init_800e5d50(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let ctx = context(m);
    m.sw(LANGUAGE_PHASE, 0);
    m.sw(LANGUAGE_CURSOR, 0);
    m.sw(LANGUAGE_ENTRY, 0);
    let language = m.lw(ctx + 0x38c);
    for i in 0..5u32 {
        if language == m.lw(LANGUAGES + 4 * i) {
            m.sw(LANGUAGE_CURSOR, i);
            m.sw(LANGUAGE_ENTRY, i);
        }
    }
    page(m, c, 0x1f);
    language_dim_800e5c98(m, c);
    let cursor = m.lw(LANGUAGE_CURSOR);
    style(m, c, cursor.wrapping_add(1), 1);
    set_timers(m, &[0x16, 0x13, 0x17, 0x14, 0x11]);
    place_panels(m, c, LANGUAGE_SLOTS, 5);
    m.sw(PANEL_TIMER, 0x11);
    offset(m, c, 0xb, 0x121, 0);
    for index in 1..6 {
        label(m, c, index);
    }
    m.pop_frame(0x20);
    2
}

/// Language phase 3: slots (cursor lit, arrows by the held bits); repeat
/// bits 1/2 move the cursor around 0..4 (sound 0x3D); confirm (0x10, sound
/// 0x3C) stores the cursor's language to context +0x38C and blinks (phase
/// 5); cancel (0x20, sound 0x3B) restores the entry language (phase 6).
fn language_input(m: &mut Machine, c: &mut dyn Calls, held: u32, repeat: u32, pressed: u32) -> u32 {
    language_dim_800e5c98(m, c);
    let cursor = m.lw(LANGUAGE_CURSOR);
    style(m, c, cursor.wrapping_add(1), 1);
    if held & 1 != 0 {
        style(m, c, 6, 1);
    }
    if held & 2 != 0 {
        style(m, c, 7, 1);
    }
    if repeat & 1 != 0 {
        sound(m, c, 0x3d);
        let cursor = m.lw(LANGUAGE_CURSOR).wrapping_sub(1);
        m.sw(LANGUAGE_CURSOR, cursor);
        if (cursor as i32) < 0 {
            m.sw(LANGUAGE_CURSOR, 4);
        }
    } else if repeat & 2 != 0 {
        sound(m, c, 0x3d);
        let cursor = m.lw(LANGUAGE_CURSOR).wrapping_add(1);
        m.sw(LANGUAGE_CURSOR, cursor);
        if (cursor as i32) >= 5 {
            m.sw(LANGUAGE_CURSOR, 0);
        }
    } else if pressed & 0x10 != 0 {
        sound(m, c, 0x3c);
        m.sw(LANGUAGE_BLINK, 0);
        let cursor = m.lw(LANGUAGE_CURSOR);
        let ctx = context(m);
        let language = word(m, LANGUAGES, cursor);
        m.sw(LANGUAGE_PHASE, 5);
        m.sw(ctx + 0x38c, language);
    } else if pressed & 0x20 != 0 {
        sound(m, c, 0x3b);
        let entry = m.lw(LANGUAGE_ENTRY);
        let ctx = context(m);
        let language = word(m, LANGUAGES, entry);
        m.sw(LANGUAGE_PHASE, 6);
        m.sw(ctx + 0x38c, language);
    }
    0
}

/// 800E5ECC (language step 1): phases (0x800F9834, table 0x800CB284) 0, 1,
/// 2 slide in, 3 input, 4 (to 7), 5 blink the choice every 6 updates for
/// 13 updates (the option-row slots are dimmed, 800E62C0), 6 slide out
/// (then 4), 7 returns 3. Returns 0 otherwise.
pub fn language_update_800e5ecc(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let phase = m.lw(LANGUAGE_PHASE);
    let ctx = context(m);
    m.push_frame(0x28);
    let repeat = m.lw(ctx + 0x130);
    let pressed = m.lw(ctx + 0x12c);
    let held = m.lw(ctx + 0x128);
    let result = match phase {
        0 => {
            m.sw(LANGUAGE_PHASE, 1);
            0
        }
        1 => {
            m.sw(LANGUAGE_PHASE, 2);
            0
        }
        2 => {
            if slide_in(m, c, LANGUAGE_SLOTS, 5) >= 5 {
                m.sw(LANGUAGE_PHASE, 3);
            }
            panel_slide(m, c, u32::MAX);
            0
        }
        3 => language_input(m, c, held, repeat, pressed),
        4 => {
            m.sw(LANGUAGE_PHASE, 7);
            0
        }
        5 => {
            options_dim_800e62c0(m, c);
            let blink = m.lw(LANGUAGE_BLINK);
            if blink == div_magic(blink, 0x2aaa_aaab, 0, false).wrapping_mul(6) {
                let cursor = m.lw(LANGUAGE_CURSOR);
                style(m, c, cursor.wrapping_add(1), 1);
            }
            let blink = m.lw(LANGUAGE_BLINK).wrapping_add(1);
            m.sw(LANGUAGE_BLINK, blink);
            if (blink as i32) >= 0xd {
                m.sw(LANGUAGE_PHASE, 6);
            }
            0
        }
        6 => {
            if slide_out(m, c, LANGUAGE_SLOTS, 5) >= 5 {
                m.sw(LANGUAGE_PHASE, 4);
            }
            panel_slide(m, c, 1);
            0
        }
        7 => 3,
        _ => 0,
    };
    m.pop_frame(0x28);
    result
}

// ---- item 3: option rows -----------------------------------------------------

/// 800E62C0: style 2 for the option-row slots (list 0x800CB2A4).
pub fn options_dim_800e62c0(m: &mut Machine, c: &mut dyn Calls) {
    m.push_frame(0x58);
    dim_slots(m, c, OPTION_DIM, 13);
    m.pop_frame(0x58);
}

/// 800E6378: the option rows. With `draw`, the row cursor's slot (1, 4, 8
/// from 0x800CB2D8) is lit and each row's value slot styled (4 on the
/// cursor's row, else 1); the cursor's row also shows its value label
/// (row 0: 0x98 + value, row 1: 0x9C - value, row 2: 0x9D + value).
pub fn options_show_800e6378(m: &mut Machine, c: &mut dyn Calls, draw: u32) {
    let frame = m.push_frame(0x28);
    // The cursor slots are a stack copy indexed by the row word.
    let slots: Vec<u32> = (0..3).map(|i| m.lw(OPTION_ROW_SLOTS + 4 * i)).collect();
    for (i, &slot) in slots.iter().enumerate() {
        m.sw(frame + 0x10 + 4 * i as u32, slot);
    }
    if draw != 0 {
        let row = m.lw(OPTION_ROW);
        let slot = word(m, frame + 0x10, row);
        style(m, c, slot, 1);
    }
    if m.lw(OPTION_ROW) == 0 {
        if draw != 0 {
            let value = u32::from(m.lbu(OPTION_VALUE_0));
            style(m, c, value + 2, 4);
        }
        let value = u32::from(m.lbu(OPTION_VALUE_0));
        label(m, c, value + 0x98);
    } else if draw != 0 {
        let value = u32::from(m.lbu(OPTION_VALUE_0));
        style(m, c, value + 2, 1);
    }
    if m.lw(OPTION_ROW) == 1 {
        if draw != 0 {
            let value = u32::from(m.lbu(OPTION_VALUE_1));
            style(m, c, value + 5, 4);
        }
        let value = u32::from(m.lbu(OPTION_VALUE_1));
        label(m, c, 0x9cu32.wrapping_sub(value));
    } else if draw != 0 {
        let value = u32::from(m.lbu(OPTION_VALUE_1));
        style(m, c, value + 5, 1);
    }
    if m.lw(OPTION_ROW) == 2 {
        if draw != 0 {
            let value = u32::from(m.lbu(OPTION_VALUE_2));
            style(m, c, value + 9, 4);
        }
        let value = u32::from(m.lbu(OPTION_VALUE_2));
        label(m, c, value + 0x9d);
    } else if draw != 0 {
        let value = u32::from(m.lbu(OPTION_VALUE_2));
        style(m, c, value + 9, 1);
    }
    m.pop_frame(0x28);
}

/// 800E6504 (option rows step 0): phase and row 0; the row values from the
/// context (row 0: 0 when +0x3A0 is 1, else 1; row 1: +0x120 bits 0/1 as
/// 0 both, 1 bit 0 only, 2 bit 0 clear; row 2: byte +0x95E), kept as entry
/// values too; page 0x1E, the rows, the six panels off-screen (timers 0x16,
/// 0x11, 0x15, 0x13, 0x14, 0x12), panel 0xB at x 0x121, labels cleared.
/// Returns 2.
pub fn options_init_800e6504(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    m.sw(OPTION_PHASE, 0);
    let ctx = context(m);
    m.sw(OPTION_ROW, 0);
    let value = if m.lbu(ctx + 0x3a0) == 1 { 0 } else { 1 };
    m.sb(OPTION_ENTRY_0, value);
    m.sb(OPTION_VALUE_0, value);
    let ctx = context(m);
    let flags = m.lw(ctx + 0x120);
    let value = if flags & 1 == 0 {
        2
    } else if flags & 2 != 0 {
        0
    } else {
        1
    };
    m.sb(OPTION_ENTRY_1, value);
    m.sb(OPTION_VALUE_1, value);
    let ctx = context(m);
    let value = m.lbu(ctx + 0x95e);
    m.sb(OPTION_ENTRY_2, value);
    m.sb(OPTION_VALUE_2, value);
    page(m, c, 0x1e);
    options_dim_800e62c0(m, c);
    options_show_800e6378(m, c, 1);
    set_timers(m, &[0x16, 0x11, 0x15, 0x13, 0x14, 0x12]);
    place_panels(m, c, OPTION_SLOTS, 6);
    m.sw(PANEL_TIMER, 0x11);
    offset(m, c, 0xb, 0x121, 0);
    clear_labels(m, c);
    m.pop_frame(0x20);
    2
}

/// Store row values (bytes at `first`, `second`, `third`) to the context:
/// +0x3A0 = 1 when the first is 0 (else 0); +0x120 bits 0/1 from the
/// second (0: both set, 1: bit 0 only, 2: both clear); +0x95E = the third.
fn options_commit(m: &mut Machine, first: u32, second: u32, third: u32) {
    let value = if m.lbu(first) == 0 { 1 } else { 0 };
    let ctx = context(m);
    m.sb(ctx + 0x3a0, value);
    match m.lbu(second) {
        0 => {
            let ctx = context(m);
            let flags = m.lw(ctx + 0x120);
            m.sw(ctx + 0x120, flags | 3);
        }
        1 => {
            let ctx = context(m);
            let flags = m.lw(ctx + 0x120);
            m.sw(ctx + 0x120, (flags | 1) & !2);
        }
        2 => {
            let ctx = context(m);
            let flags = m.lw(ctx + 0x120);
            m.sw(ctx + 0x120, flags & !3);
        }
        _ => {}
    }
    let ctx = context(m);
    let value = m.lbu(third);
    m.sb(ctx + 0x95e, value);
}

/// Pressed right (8) sets the 0/1 byte at `at`, left (4) clears it (sound
/// 0x2E on a change).
fn options_toggle(m: &mut Machine, c: &mut dyn Calls, pressed: u32, at: u32) {
    if pressed & 8 != 0 && m.lbu(at) == 0 {
        sound(m, c, 0x2e);
        m.sb(at, 1);
    } else if pressed & 4 != 0 && m.lbu(at) == 1 {
        sound(m, c, 0x2e);
        m.sb(at, 0);
    }
}

/// Option rows phase 3: dimmed slots, the rows, arrows by the held bits
/// (slots 0xB/0xC); confirm (0x10, sound 0x3C) commits the values (blink,
/// phase 4); cancel (0x20, sound 0x3B) commits the entry values (phase 5);
/// repeat bits 1/2 move the row around 0..2 (sound 0x3D); pressed bits 8/4
/// change the cursor row's value (row 1 within 0..2).
fn options_input(m: &mut Machine, c: &mut dyn Calls, held: u32, repeat: u32, pressed: u32) -> u32 {
    options_dim_800e62c0(m, c);
    options_show_800e6378(m, c, 1);
    if held & 1 != 0 {
        style(m, c, 0xb, 1);
    }
    if held & 2 != 0 {
        style(m, c, 0xc, 1);
    }
    if pressed & 0x10 != 0 {
        sound(m, c, 0x3c);
        options_commit(m, OPTION_VALUE_0, OPTION_VALUE_1, OPTION_VALUE_2);
        m.sw(OPTION_BLINK, 0);
        m.sw(OPTION_PHASE, 4);
        return 0;
    }
    if pressed & 0x20 != 0 {
        sound(m, c, 0x3b);
        options_commit(m, OPTION_ENTRY_0, OPTION_ENTRY_1, OPTION_ENTRY_2);
        m.sw(OPTION_PHASE, 5);
        return 0;
    }
    if repeat & 1 != 0 {
        sound(m, c, 0x3d);
        let row = m.lw(OPTION_ROW).wrapping_sub(1);
        m.sw(OPTION_ROW, row);
        if (row as i32) < 0 {
            m.sw(OPTION_ROW, 2);
        }
        return 0;
    }
    if repeat & 2 != 0 {
        sound(m, c, 0x3d);
        let row = m.lw(OPTION_ROW).wrapping_add(1);
        m.sw(OPTION_ROW, row);
        if (row as i32) >= 3 {
            m.sw(OPTION_ROW, 0);
        }
        return 0;
    }
    match m.lw(OPTION_ROW) {
        0 => options_toggle(m, c, pressed, OPTION_VALUE_0),
        1 => {
            if pressed & 8 != 0 && m.lbu(OPTION_VALUE_1) < 2 {
                sound(m, c, 0x2e);
                let value = m.lbu(OPTION_VALUE_1);
                m.sb(OPTION_VALUE_1, value.wrapping_add(1));
            } else if pressed & 4 != 0 && m.lbu(OPTION_VALUE_1) != 0 {
                sound(m, c, 0x2e);
                let value = m.lbu(OPTION_VALUE_1);
                m.sb(OPTION_VALUE_1, value.wrapping_sub(1));
            }
        }
        2 => options_toggle(m, c, pressed, OPTION_VALUE_2),
        _ => {}
    }
    0
}

/// 800E66A0 (option rows step 1): labels cleared; phases (0x800F9864,
/// table 0x800CB2E4) 0, 1, 2 slide in (until the first timer is <= 0), 3
/// input, 4 blink the rows every 6 updates for 13 updates, 5 slide out
/// (until the second timer reaches 0x11), 6, 7 returns 3. Returns 0
/// otherwise.
pub fn options_update_800e66a0(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    let ctx = context(m);
    let repeat = m.lw(ctx + 0x130);
    let pressed = m.lw(ctx + 0x12c);
    let held = m.lw(ctx + 0x128);
    clear_labels(m, c);
    let result = match m.lw(OPTION_PHASE) {
        0 => {
            m.sw(OPTION_PHASE, 1);
            0
        }
        1 => {
            m.sw(OPTION_PHASE, 2);
            0
        }
        2 => {
            slide_in(m, c, OPTION_SLOTS, 6);
            if (m.lw(TIMERS) as i32) <= 0 {
                m.sw(OPTION_PHASE, 3);
            }
            panel_slide(m, c, u32::MAX);
            0
        }
        3 => options_input(m, c, held, repeat, pressed),
        4 => {
            options_dim_800e62c0(m, c);
            let blink = m.lw(OPTION_BLINK);
            let draw = blink == div_magic(blink, 0x2aaa_aaab, 0, false).wrapping_mul(6);
            options_show_800e6378(m, c, u32::from(draw));
            let blink = m.lw(OPTION_BLINK).wrapping_add(1);
            m.sw(OPTION_BLINK, blink);
            if (blink as i32) >= 0xd {
                m.sw(OPTION_PHASE, 5);
            }
            0
        }
        5 => {
            slide_out(m, c, OPTION_SLOTS, 6);
            if (m.lw(TIMERS + 4) as i32) >= 0x11 {
                m.sw(OPTION_PHASE, 6);
            }
            panel_slide(m, c, 1);
            0
        }
        6 => {
            m.sw(OPTION_PHASE, 7);
            0
        }
        7 => 3,
        _ => 0,
    };
    m.pop_frame(0x20);
    result
}

// ---- item 0: sound -----------------------------------------------------------

/// 800E6D44: style 2 for the sound slots (list 0x800CB304).
pub fn sound_dim_800e6d44(m: &mut Machine, c: &mut dyn Calls) {
    m.push_frame(0x40);
    dim_slots(m, c, SOUND_DIM, 8);
    m.pop_frame(0x40);
}

/// 800E6DFC: the sound rows: slot 2 (stereo) or 1 styled 4 on row 0, else
/// 1; slots 3 and 4 styled 4 on rows 1 and 2, else 1; slot 7 styled 4.
pub fn sound_show_800e6dfc(m: &mut Machine, c: &mut dyn Calls) {
    m.push_frame(0x18);
    let slot = if m.lw(STEREO) != 0 { 2 } else { 1 };
    let value = if m.lw(SOUND_ROW) != 0 { 1 } else { 4 };
    style(m, c, slot, value);
    let value = if m.lw(SOUND_ROW) == 1 { 4 } else { 1 };
    style(m, c, 3, value);
    let value = if m.lw(SOUND_ROW) == 2 { 4 } else { 1 };
    style(m, c, 4, value);
    style(m, c, 7, 4);
    m.pop_frame(0x18);
}

/// One POLY_F4 rectangle from x0 to x1 between the pre-shifted y words
/// `top` and `bottom`, linked into `ot` unless both x are 0.
#[allow(clippy::too_many_arguments)]
fn flat_quad(
    m: &mut Machine,
    prim: u32,
    ot: u32,
    head: u32,
    x0: u32,
    x1: u32,
    top: u32,
    bottom: u32,
) {
    m.sw(prim.wrapping_add(4), head);
    m.sw(prim.wrapping_add(0xc), x1 | top);
    m.sw(prim.wrapping_add(8), x0 | top);
    m.sw(prim.wrapping_add(0x10), x0 | bottom);
    m.sw(prim.wrapping_add(0x14), x1 | bottom);
    if x0 != 0 || x1 != 0 {
        let tag = m.lw(ot) | 0x0500_0000;
        m.sw(prim, tag);
        m.sw(ot, prim & 0x00ff_ffff);
    }
}

/// One volume bar of the sound panel at `prim`: a 2-pixel marker at the
/// volume byte `[pointer]` (0..255 over 222 pixels, colour 0x010101), then
/// the bar from the left edge to the marker (highlighted on `selected`).
/// Returns the next primitive.
fn volume_bar(
    m: &mut Machine,
    prim: u32,
    ot: u32,
    left: u32,
    pointer: u32,
    rows: (u32, u32),
    selected: bool,
) -> u32 {
    let at = m.lw(pointer);
    let volume = u32::from(m.lbu(at));
    let width = div_magic(volume.wrapping_mul(222), 0x8080_8081, 7, true);
    let base = left.wrapping_add(0x38);
    let end = width.wrapping_add(base);
    let x1 = clamp(end);
    let x0 = clamp(end.wrapping_sub(2));
    flat_quad(m, prim, ot, 0x2801_0101, x0, x1, rows.0, rows.1);
    let prim = prim.wrapping_add(0x18);
    let colour = if selected { 0x0017_8bcf } else { 0x00d5_e5e5 };
    let x0 = clamp(left.wrapping_add(0x38));
    flat_quad(m, prim, ot, colour | 0x2800_0000, x0, x1, rows.0, rows.1);
    prim.wrapping_add(0x18)
}

/// 800E6EC0: draw the sound panel shifted left by t*t (timer 0x800F9844)
/// into the current ordering table: the level meter (when GetODE is nonzero
/// the width is re-measured from the stream level, 800D0DB8, capped at 0x62)
/// into entry 2, and the two volume bars (context +0x39C, +0x39D; `row` 1
/// or 2 highlights one) into entry 5. Stores the next primitive.
pub fn draw_sound_panel_800e6ec0(m: &mut Machine, c: &mut dyn Calls, row: u32) {
    m.push_frame(0x30);
    let t = m.lw(TIMERS + 4);
    let scene = m.lw(SCENE);
    let mut prim = m.lw(scene + 0x2_0094);
    let left = t.wrapping_mul(t).wrapping_neg();
    let ot = c.call(m, CURRENT_OT, [0; 4]);
    let meter_ot = ot.wrapping_add(8);
    let bars_ot = ot.wrapping_add(0x14);
    if c.call(m, GET_ODE, [0; 4]) != 0 {
        let level = stream_level_800d0db8(m, c);
        let scaled = level.wrapping_mul(14);
        let width = div_magic(scaled, 0xc003_000d, 13, true).wrapping_mul(7);
        m.sw(METER, width);
        if (width as i32) >= 0x63 {
            m.sw(METER, 0x62);
        }
    }
    let x0 = clamp(left.wrapping_add(0x32));
    let meter = m.lw(METER).wrapping_add(0x32);
    let x1 = clamp(left.wrapping_add(meter));
    flat_quad(
        m,
        prim,
        meter_ot,
        0x2817_8bcf,
        x0,
        x1,
        0x00bd_0000,
        0x00c9_0000,
    );
    prim = prim.wrapping_add(0x18);
    prim = volume_bar(
        m,
        prim,
        bars_ot,
        left,
        VOLUME_0,
        (0x00c2_0000, 0x0102_0000),
        row == 1,
    );
    prim = volume_bar(
        m,
        prim,
        bars_ot,
        left,
        VOLUME_1,
        (0x010c_0000, 0x014c_0000),
        row == 2,
    );
    let scene = m.lw(SCENE);
    m.sw(scene + 0x2_0094, prim);
    m.pop_frame(0x30);
}

/// 800E72C4 (sound step 0): phase 0; pointers to context +0x39E, +0x39C,
/// +0x39D; stereo flag = +0x39E bit 0; save the flag and both volumes; row
/// 0; page 0x20, the rows, the two panels off-screen (timers 0x11, 0x14 at
/// x -0x121, -0x190), panel 0xB at x 0x121, stream 0x81C started.
/// Returns 2.
pub fn sound_init_800e72c4(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    m.sw(SOUND_PHASE, 0);
    let ctx = context(m);
    m.sw(SOUND_FLAGS, ctx + 0x39e);
    let flags = u32::from(m.lbu(ctx + 0x39e));
    m.sw(VOLUME_0, ctx + 0x39c);
    m.sw(VOLUME_1, ctx + 0x39d);
    m.sw(STEREO, flags & 1);
    let stereo = m.lbu(STEREO);
    m.sb(SAVED_STEREO, stereo);
    let volume = m.lbu(ctx + 0x39c);
    m.sb(SAVED_VOLUME_0, volume);
    let volume = m.lbu(ctx + 0x39d);
    m.sw(SOUND_ROW, 0);
    m.sb(SAVED_VOLUME_1, volume);
    page(m, c, 0x20);
    sound_dim_800e6d44(m, c);
    sound_show_800e6dfc(m, c);
    m.sw(TIMERS, 0x11);
    let slot = m.lw(SOUND_SLOTS);
    offset(m, c, slot, (-0x121i32) as u32, 0);
    let slot = m.lw(SOUND_SLOTS + 4);
    m.sw(TIMERS + 4, 0x14);
    offset(m, c, slot, (-0x190i32) as u32, 0);
    m.sw(PANEL_TIMER, 0x11);
    offset(m, c, 0xb, 0x121, 0);
    c.call(m, STREAM_SELECT, [0x81c, 0, 0, 0]);
    c.call(m, STREAM_START, [0; 4]);
    m.pop_frame(0x20);
    2
}

/// 8002B900(`mode`), then 8002B934.
fn apply_audio(m: &mut Machine, c: &mut dyn Calls, mode: u32) {
    c.call(m, AUDIO_MODE, [mode, 0, 0, 0]);
    c.call(m, AUDIO_APPLY, [0; 4]);
}

/// Repeat right (8) raises the volume byte `[pointer]` by 0x10 (to 0xFF
/// from 0xF0 up), left (4) lowers it by 0x10 (to 0 below 0x11); sound 0x2E
/// on a change.
fn volume_step(m: &mut Machine, c: &mut dyn Calls, repeat: u32, pointer: u32) {
    if repeat & 8 != 0 {
        let at = m.lw(pointer);
        let volume = m.lbu(at);
        if volume < 0xf0 {
            sound(m, c, 0x2e);
            let at = m.lw(pointer);
            let volume = m.lbu(at);
            m.sb(at, volume.wrapping_add(0x10));
        } else if volume != 0xff {
            sound(m, c, 0x2e);
            let at = m.lw(pointer);
            m.sb(at, 0xff);
        }
    } else if repeat & 4 != 0 {
        let at = m.lw(pointer);
        let volume = m.lbu(at);
        if volume >= 0x11 {
            sound(m, c, 0x2e);
            let at = m.lw(pointer);
            let volume = m.lbu(at);
            m.sb(at, volume.wrapping_sub(0x10));
        } else if volume != 0 {
            sound(m, c, 0x2e);
            let at = m.lw(pointer);
            m.sb(at, 0);
        }
    }
}

/// Sound phase 3: dimmed slots, the rows, the panel; arrows by the held
/// bits (slots 5/6); confirm (0x10, sound 0x3C) keeps the settings,
/// cancel (0x20, sound 0x3B) restores the flag byte (0 when the saved flag
/// is 0, else bit 0 set) and applies it, and the volumes (both phase 4);
/// repeat bits 1/2 move the row around 0..2 (sound 0x3D); row 0: pressed
/// 8/4 set/clear stereo (bit 0, or the whole byte cleared) and apply it;
/// rows 1/2: repeat 8/4 step a volume.
fn sound_input(m: &mut Machine, c: &mut dyn Calls, held: u32, repeat: u32, pressed: u32) -> u32 {
    sound_dim_800e6d44(m, c);
    sound_show_800e6dfc(m, c);
    let row = m.lw(SOUND_ROW);
    draw_sound_panel_800e6ec0(m, c, row);
    if held & 1 != 0 {
        style(m, c, 5, 1);
    }
    if held & 2 != 0 {
        style(m, c, 6, 1);
    }
    if pressed & 0x10 != 0 {
        sound(m, c, 0x3c);
        m.sw(SOUND_PHASE, 4);
        return 0;
    }
    if pressed & 0x20 != 0 {
        sound(m, c, 0x3b);
        let mode = if m.lbu(SAVED_STEREO) == 0 {
            let at = m.lw(SOUND_FLAGS);
            m.sb(at, 0);
            0
        } else {
            let at = m.lw(SOUND_FLAGS);
            let flags = m.lbu(at);
            m.sb(at, flags | 1);
            1
        };
        apply_audio(m, c, mode);
        let at = m.lw(VOLUME_0);
        let volume = m.lbu(SAVED_VOLUME_0);
        m.sb(at, volume);
        let at = m.lw(VOLUME_1);
        let volume = m.lbu(SAVED_VOLUME_1);
        m.sb(at, volume);
        m.sw(SOUND_PHASE, 4);
        return 0;
    }
    if repeat & 1 != 0 {
        sound(m, c, 0x3d);
        let row = m.lw(SOUND_ROW).wrapping_sub(1);
        m.sw(SOUND_ROW, row);
        if (row as i32) < 0 {
            m.sw(SOUND_ROW, 2);
        }
        return 0;
    }
    if repeat & 2 != 0 {
        sound(m, c, 0x3d);
        let row = m.lw(SOUND_ROW).wrapping_add(1);
        m.sw(SOUND_ROW, row);
        if (row as i32) >= 3 {
            m.sw(SOUND_ROW, 0);
        }
        return 0;
    }
    match m.lw(SOUND_ROW) {
        0 => {
            if pressed & 8 != 0 && m.lw(STEREO) != 1 {
                sound(m, c, 0x2e);
                let at = m.lw(SOUND_FLAGS);
                m.sw(STEREO, 1);
                let flags = m.lbu(at);
                m.sb(at, flags | 1);
                apply_audio(m, c, 1);
            } else if pressed & 4 != 0 && m.lw(STEREO) != 0 {
                sound(m, c, 0x2e);
                let at = m.lw(SOUND_FLAGS);
                m.sw(STEREO, 0);
                m.sb(at, 0);
                apply_audio(m, c, 0);
            }
        }
        1 => volume_step(m, c, repeat, VOLUME_0),
        2 => volume_step(m, c, repeat, VOLUME_1),
        _ => {}
    }
    0
}

/// 800E73DC (sound step 1): phases (0x800F987C, table 0x800CB324) 0, 1, 2
/// slide in (until the second timer is <= 0), 3 input, 4 slide out (when
/// the first timer reaches 0x11: phase 5 and stop the stream), 5, 6
/// returns 3. Phases 2 and 4 also draw the panel. Returns 0 otherwise.
pub fn sound_update_800e73dc(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let phase = m.lw(SOUND_PHASE);
    let ctx = context(m);
    m.push_frame(0x28);
    let repeat = m.lw(ctx + 0x130);
    let pressed = m.lw(ctx + 0x12c);
    let held = m.lw(ctx + 0x128);
    let result = match phase {
        0 => {
            m.sw(SOUND_PHASE, 1);
            0
        }
        1 => {
            m.sw(SOUND_PHASE, 2);
            0
        }
        2 => {
            slide_in(m, c, SOUND_SLOTS, 2);
            if (m.lw(TIMERS + 4) as i32) <= 0 {
                m.sw(SOUND_PHASE, 3);
            }
            panel_slide(m, c, u32::MAX);
            let row = m.lw(SOUND_ROW);
            draw_sound_panel_800e6ec0(m, c, row);
            0
        }
        3 => sound_input(m, c, held, repeat, pressed),
        4 => {
            slide_out(m, c, SOUND_SLOTS, 2);
            if (m.lw(TIMERS) as i32) >= 0x11 {
                m.sw(SOUND_PHASE, 5);
                c.call(m, STREAM_STOP, [0; 4]);
            }
            panel_slide(m, c, 1);
            let row = m.lw(SOUND_ROW);
            draw_sound_panel_800e6ec0(m, c, row);
            0
        }
        5 => {
            m.sw(SOUND_PHASE, 6);
            0
        }
        6 => 3,
        _ => 0,
    };
    m.pop_frame(0x28);
    result
}

/// The claims of this file's routines.
pub mod routines {
    use super::*;

    /// Run a routine of this file by original address with the original
    /// argument registers; `None` for any other address.
    pub fn dispatch(
        m: &mut Machine,
        c: &mut dyn Calls,
        target: u32,
        args: [u32; 4],
    ) -> Option<u32> {
        let [a0, a1, ..] = args;
        Some(match target {
            0x800d_0d30 => decoded_half_800d0d30(m, c, a0),
            0x800d_0db8 => stream_level_800d0db8(m, c),
            0x800e_4978 => menu_load_800e4978(m, c),
            0x800e_49dc => {
                menu_show_800e49dc(m, c);
                0
            }
            0x800e_4b10 => menu_poll_textures_800e4b10(m, c),
            0x800e_4b44 => menu_input_800e4b44(m, c),
            0x800e_4cf0 => menu_item_step_800e4cf0(m, c),
            0x800e_4d6c => menu_update_800e4d6c(m, c),
            0x800e_5288 => {
                position_dim_800e5288(m, c);
                0
            }
            0x800e_5328 => position_init_800e5328(m, c),
            0x800e_53c8 => position_update_800e53c8(m, c),
            0x800e_571c => draw_colour_bars_800e571c(m, a0, a1),
            0x800e_594c => bars_init_800e594c(m, c),
            0x800e_59e8 => bars_update_800e59e8(m, c),
            0x800e_5c98 => {
                language_dim_800e5c98(m, c);
                0
            }
            0x800e_5d50 => language_init_800e5d50(m, c),
            0x800e_5ecc => language_update_800e5ecc(m, c),
            0x800e_62c0 => {
                options_dim_800e62c0(m, c);
                0
            }
            0x800e_6378 => {
                options_show_800e6378(m, c, a0);
                0
            }
            0x800e_6504 => options_init_800e6504(m, c),
            0x800e_66a0 => options_update_800e66a0(m, c),
            0x800e_6d44 => {
                sound_dim_800e6d44(m, c);
                0
            }
            0x800e_6dfc => {
                sound_show_800e6dfc(m, c);
                0
            }
            0x800e_6ec0 => {
                draw_sound_panel_800e6ec0(m, c, a0);
                0
            }
            0x800e_72c4 => sound_init_800e72c4(m, c),
            0x800e_73dc => sound_update_800e73dc(m, c),
            _ => return None,
        })
    }
}
