//! Overlay module 2 (state 3), inner states off the first-mission path,
//! translated from `analysis/overlays/overlay_002` (callback pairs from the
//! table at 0x800F1DD4; routing in `ac3-mission`'s `dispatcher.rs`):
//!
//! - 5 (free-mission hub choice 0): the free-mission list, block
//!   `[0x800F60C0]`; phases at 0x800F60D8 (800DDBD0/800DDC10 build the list
//!   of cleared missions, 800DDD2C/800DDE5C show and move it, 800DE2F4
//!   commits the mission id).
//! - 6 (after 5): difficulty selection, block `[0x800F6090]`; phases at
//!   0x800F60AC (slide in, input, save the panel images and run the
//!   mission loader's phases from row `[0x800F39FC]`).
//! - 7 and 8 (hub choice 1): mission records ([`crate::ov2_misc_records`]).
//! - 15 (progress flag 0x20 at the state-3 entry): "now loading" text over
//!   the loader phases of table 0x800F39F4, then state 10.
//!
//! - 10 (hub choice 3), the OPTIONS screen: its mode 2, GAME CONFIG, which
//!   only phase tables reach ([`crate::ov2_misc_settings`]).
//!
//! Conventions as in [`crate::ov2_aircraft`]: routines owned elsewhere
//! (`ov2_core`, `ov2_hub`, `ov2_options`, resident) are called by original
//! address through [`Calls`]; original frames are pushed wherever a routine
//! calls out; `dispatch` claims only while overlay module 2 is loaded.
use crate::{
    arena,
    common::{CONTEXT, loaded_module},
    machine::{Machine, SmallData},
    ov2_aircraft::{CLEAR_LABELS, Calls, Dispatch, LABEL, OFFSET, PAGE, SCENE, SOUND, STYLE, TEXT},
    ov2_misc_records as records, ov2_misc_settings as settings,
};

/// 800CE978 / 800D17D0 (`ov2_aircraft`): resource load, mission category.
pub const LOAD_RESOURCE: u32 = 0x800c_e978;
pub const MISSION_CATEGORY: u32 = 0x800d_17d0;
/// 800D1828 (`ov2_hub`): id byte of the mission record of category `a0`.
pub const MISSION_RECORD: u32 = 0x800d_1828;
/// 800D170C (`ov2_hub`): common layer drawing (a0 primitive, a1, a2);
/// returns the next primitive.
pub const COMMON_LAYER: u32 = 0x800d_170c;
/// 800CEB80 / 800CEB90 (`ov2_core`, loader) and 800CEC18 (`ov2_hub`): loader page word,
/// loader finish, loader rectangle.
pub const LOADER_PAGE: u32 = 0x800c_eb80;
pub const LOADER_FINISH: u32 = 0x800c_eb90;
pub const LOADER_RECT: u32 = 0x800c_ec18;
/// 800E79D4 (`ov2_core`): background colour.
pub const SCENE_COLOUR: u32 = 0x800e_79d4;
/// Resident: sprintf, strcat, strlen (800AC328), LoadImage, StoreImage,
/// DrawSync.
pub const SPRINTF: u32 = 0x800a_cd28;
pub const STRCAT: u32 = 0x800a_c1b8;
pub const STRLEN: u32 = 0x800a_c328;
pub const LOAD_IMAGE: u32 = 0x800a_6910;
pub const STORE_IMAGE: u32 = 0x800a_6970;
pub const DRAW_SYNC: u32 = 0x800a_6664;
/// 0x800F5958: mission records (12 bytes; +8 name string pointer).
pub const MISSIONS: u32 = 0x800f_5958;
/// 0x800F60C0 / 0x800F6090 / 0x800F5E80: blocks of inner states 5, 6, 15.
pub const FREE_LIST: u32 = 0x800f_60c0;
pub const DIFFICULTY: u32 = 0x800f_6090;
pub const RELOAD: u32 = 0x800f_5e80;
/// 0x800F60D8 / 0x800F60AC / 0x800F39F4: phase tables of states 5, 6, 15
/// (rows of routines, indexed by the block's phase and substate halfwords).
pub const FREE_LIST_PHASES: u32 = 0x800f_60d8;
pub const DIFFICULTY_PHASES: u32 = 0x800f_60ac;
pub const RELOAD_PHASES: u32 = 0x800f_39f4;
/// `[0x800F39FC]`: the loader row state 6 runs after a confirm.
pub const LOADER_ROW: u32 = 0x800f_39fc;
/// 0x800F5B54: three pairs of 16-byte panel image rectangles.
pub const PANEL_RECTS: u32 = 0x800f_5b54;
/// 0x800F3A30: "now loading" text per language (context +0x38C).
pub const LOADING_TEXT: u32 = 0x800f_3a30;
const GREY: u32 = 0x0080_8080;

pub(crate) fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

fn style(m: &mut Machine, c: &mut dyn Calls, slot: u32, value: u32) {
    c.call(m, STYLE, [slot, value, 0, 0]);
}

fn sound(m: &mut Machine, c: &mut dyn Calls, id: u32) {
    c.call(m, SOUND, [id, 0, 0, 0]);
}

/// 800D170C(`[scene+0x20094]`, 0, 1), stored back to scene +0x20094.
fn common_layer(m: &mut Machine, c: &mut dyn Calls) {
    let scene = m.lw(SCENE);
    let prim = m.lw(scene + 0x2_0094);
    let next = c.call(m, COMMON_LAYER, [prim, 0, 1, 0]);
    let scene = m.lw(SCENE);
    m.sw(scene + 0x2_0094, next);
}

/// `[[table + phase*4] + substate*4]` for the signed halfwords +0/+2 of
/// `block`.
fn phase_routine(m: &mut Machine, table: u32, block: u32) -> u32 {
    let phase = m.lh(block) as i32 as u32;
    let row = m.lw((phase << 2).wrapping_add(table));
    let substate = m.lh(block + 2) as i32 as u32;
    m.lw((substate << 2).wrapping_add(row))
}

/// Phase routine results: 1 next substate, 2 next phase; otherwise the
/// value is returned for the caller's own handling.
fn advance(m: &mut Machine, block: u32, result: u32) -> bool {
    if result & 1 != 0 {
        let substate = m.lhu(block + 2);
        m.sh(block + 2, substate.wrapping_add(1));
        true
    } else if result & 2 != 0 {
        let phase = m.lhu(block);
        m.sh(block + 2, 0);
        m.sh(block, phase.wrapping_add(1));
        true
    } else {
        false
    }
}

// ---- inner state 5: free-mission list ---------------------------------

/// 800DE354: free-mission list entry (common layer, phase 0, flags 0,
/// slots 1..4). Returns 1.
pub fn free_list_init_800de354(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x20);
    common_layer(m, c);
    let b = m.lw(FREE_LIST);
    m.sh(b + 2, 0);
    m.sh(b, 0);
    m.sw(b + 0x30, 0);
    style(m, c, 1, 3);
    style(m, c, 2, 4);
    style(m, c, 3, 4);
    style(m, c, 4, 4);
    m.pop_frame(0x20);
    1
}

/// 800DE3EC: free-mission list update: common layer, phase routine; result
/// 4 returns 0x100 (confirmed, flag 1) or 0x200.
pub fn free_list_update_800de3ec(m: &mut Machine, c: &mut dyn Calls, args: [u32; 4]) -> u32 {
    m.push_frame(0x20);
    common_layer(m, c);
    let b = m.lw(FREE_LIST);
    let routine = phase_routine(m, FREE_LIST_PHASES, b);
    let result = c.call(m, routine, [b, args[1], args[2], args[3]]);
    let b = m.lw(FREE_LIST);
    let value = if advance(m, b, result) {
        0
    } else if result & 4 != 0 {
        let b = m.lw(FREE_LIST);
        if m.lw(b + 0x30) & 1 != 0 {
            0x100
        } else {
            0x200
        }
    } else {
        0
    };
    m.pop_frame(0x20);
    value
}

/// 800DDBD0: clear the 0x24-byte list and its count/cursor. Returns 1.
pub fn free_list_clear_800ddbd0(m: &mut Machine) -> u32 {
    for i in 0..0x24 {
        let b = m.lw(FREE_LIST);
        m.sb(b + i + 4, 0);
    }
    let b = m.lw(FREE_LIST);
    m.sw(b + 0x28, 0);
    m.sw(b + 0x2c, 0);
    1
}

/// 800DDC10: list every category 1..0x24 whose mission was cleared at any
/// difficulty (context +0x960+0x300/+0x33C/+0x378); the cursor (+0x2C)
/// starts at the context mission. Returns 2.
pub fn free_list_build_800ddc10(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x30);
    let ctx = context(m);
    let b = m.lw(FREE_LIST);
    let records = ctx + 0x960;
    let list = b + 4;
    for i in 0..0x24u32 {
        let category = i + 1;
        let mission = c.call(m, MISSION_RECORD, [category, 0, 0, 0]);
        let at = records + (category & 0xff);
        let cleared = m.lbu(at + 0x300) != 0 || m.lbu(at + 0x33c) != 0 || m.lbu(at + 0x378) != 0;
        if cleared {
            let b = m.lw(FREE_LIST);
            let count = m.lw(b + 0x28);
            m.sb(list.wrapping_add(count), category as u8);
            let ctx = context(m);
            if m.lh(ctx + 0x390) as i32 as u32 == mission & 0xff {
                let b = m.lw(FREE_LIST);
                let count = m.lw(b + 0x28);
                m.sw(b + 0x2c, count);
            }
            let b = m.lw(FREE_LIST);
            let count = m.lw(b + 0x28);
            m.sw(b + 0x28, count.wrapping_add(1));
        }
    }
    m.pop_frame(0x30);
    2
}

/// MIPS `x / 4 * 4` for signed `x` (rounding toward zero).
fn page_start(x: u32) -> u32 {
    let biased = if (x as i32) < 0 { x.wrapping_add(3) } else { x };
    (((biased as i32) >> 2) as u32) << 2
}

/// The four row cursors (slots 7..10): 1 at the cursor's row, 2 for other
/// listed rows of its page, 0 past the end.
fn free_list_rows(m: &mut Machine, c: &mut dyn Calls) {
    for row in 0..4u32 {
        let b = m.lw(FREE_LIST);
        let cursor = m.lw(b + 0x2c);
        let first = page_start(cursor);
        let value = if cursor.wrapping_sub(first) == row {
            1
        } else if ((first.wrapping_add(row)) as i32) < (m.lw(b + 0x28) as i32) {
            2
        } else {
            0
        };
        style(m, c, 7 + row, value);
    }
}

/// The selected category's image: 800CE978(scene +0x200A4, category - 1,
/// 0x1480, no rectangle, no wait).
fn free_list_image(m: &mut Machine, c: &mut dyn Calls, frame: u32) {
    let b = m.lw(FREE_LIST);
    let cursor = m.lw(b + 0x2c);
    let category = u32::from(m.lbu(b.wrapping_add(cursor) + 4));
    let scene = m.lw(SCENE);
    m.sw(frame + 0x10, 0);
    let table = m.lw(scene + 0x2_00a4);
    c.call(
        m,
        LOAD_RESOURCE,
        [table, category.wrapping_sub(1), 0x1480, 0],
    );
}

/// 800DDD2C: show the list: the cursor's image, the page (800DDA70), page
/// 0xA, the row cursors. Returns 1.
pub fn free_list_show_800ddd2c(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let frame = m.push_frame(0x30);
    free_list_image(m, c, frame);
    draw_free_list(m, c, frame);
    c.call(m, PAGE, [0xa, 0, 0, 0]);
    free_list_rows(m, c);
    m.pop_frame(0x30);
    1
}

/// 800DDA70 on the scene's primitive area and OT entry with the list,
/// count and cursor; stores the new primitive pointer.
fn draw_free_list(m: &mut Machine, c: &mut dyn Calls, frame: u32) {
    let b = m.lw(FREE_LIST);
    let scene = m.lw(SCENE);
    let cursor = m.lw(b + 0x2c);
    m.sw(frame + 0x10, cursor);
    let prim = m.lw(scene + 0x2_0094);
    let ot = m.lw(scene + 0x90);
    let count = m.lw(b + 0x28);
    let next = free_list_page_800dda70(m, c, prim, ot, b + 4, count);
    let scene = m.lw(SCENE);
    m.sw(scene + 0x2_0094, next);
}

/// 800DDA70: the names of the cursor's page of four list entries (rows at
/// y 0xD6, 0x101, 0x12C, 0x157), font 8 at x 0x38. The cursor is the stack
/// argument (`sp+0x10`). Returns the next primitive.
pub fn free_list_page_800dda70(
    m: &mut Machine,
    c: &mut dyn Calls,
    prim: u32,
    ot: u32,
    list: u32,
    count: u32,
) -> u32 {
    let cursor = m.lw(m.sp.wrapping_add(0x10));
    let frame = m.push_frame(0x48);
    let first = page_start(cursor);
    let mut prim = prim;
    let mut y = 0u32;
    let mut i = first;
    while (i as i32) < (first.wrapping_add(4) as i32) {
        if (i as i32) < (count as i32) {
            if i & 3 == 0 {
                y = 0xd6;
            } else {
                match i.wrapping_sub(page_start(i)) {
                    1 => y = 0x101,
                    2 => y = 0x12c,
                    3 => y = 0x157,
                    _ => {}
                }
            }
            let category = m.lbu(list.wrapping_add(i));
            let mission = c.call(m, MISSION_RECORD, [u32::from(category), 0, 0, 0]);
            m.sw(frame + 0x10, 0x38);
            m.sw(frame + 0x14, y);
            m.sw(frame + 0x18, GREY);
            let name = m.lw(mission.wrapping_mul(12).wrapping_add(MISSIONS) + 8);
            prim = c.call(m, TEXT, [8, name, ot, prim]);
        }
        i = i.wrapping_add(1);
    }
    m.pop_frame(0x48);
    prim
}

/// After a cursor move: image reload, flash timer 25, slide flags 2/4
/// replaced by `side` (4 down, 2 up; bit 0 kept).
fn free_list_moved(m: &mut Machine, c: &mut dyn Calls, frame: u32, side: u32) {
    free_list_image(m, c, frame);
    let b = m.lw(FREE_LIST);
    m.sw(b + 0x34, 25);
    let flags = m.lw(b + 0x30);
    m.sw(b + 0x30, (flags & !6) | side);
}

/// 800DDE5C: free-mission list input. Confirm (0x10, flag 1) and cancel
/// (0x20) play 0x3C/0x3B and return 2. Repeat down/up (+0x130 bits 2/1)
/// move by one, pressed 0x200/0x100 by four (clamped); then the row
/// cursors and the arrow flash (slots 5/6). Returns 0.
pub fn free_list_input_800dde5c(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let frame = m.push_frame(0x38);
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    let repeat = m.lw(ctx + 0x130);
    draw_free_list(m, c, frame);
    if pressed & 0x30 != 0 {
        let id = if pressed & 0x10 != 0 {
            let b = m.lw(FREE_LIST);
            let flags = m.lw(b + 0x30);
            m.sw(b + 0x30, flags | 1);
            0x3c
        } else {
            0x3b
        };
        sound(m, c, id);
        m.pop_frame(0x38);
        return 2;
    }
    let b = m.lw(FREE_LIST);
    if repeat & 2 != 0 {
        let last = m.lw(b + 0x28).wrapping_sub(1);
        if (m.lw(b + 0x2c) as i32) < (last as i32) {
            sound(m, c, 0x3d);
            let b = m.lw(FREE_LIST);
            let cursor = m.lw(b + 0x2c).wrapping_add(1);
            m.sw(b + 0x2c, cursor);
            free_list_moved(m, c, frame, 4);
        }
    } else if repeat & 1 != 0 {
        // (The original reloads +0x130 here; nothing was stored since.)
        if (m.lw(b + 0x2c) as i32) > 0 {
            sound(m, c, 0x3d);
            let b = m.lw(FREE_LIST);
            let cursor = m.lw(b + 0x2c).wrapping_sub(1);
            m.sw(b + 0x2c, cursor);
            free_list_moved(m, c, frame, 2);
        }
    } else if pressed & 0x200 != 0 {
        let last = m.lw(b + 0x28).wrapping_sub(1);
        if (m.lw(b + 0x2c) as i32) < (last as i32) {
            sound(m, c, 0x3d);
        }
        let b = m.lw(FREE_LIST);
        let cursor = m.lw(b + 0x2c).wrapping_add(4);
        let count = m.lw(b + 0x28);
        m.sw(b + 0x2c, cursor);
        if (cursor as i32) >= (count as i32) {
            m.sw(b + 0x2c, count.wrapping_sub(1));
        }
        free_list_moved(m, c, frame, 4);
    } else if pressed & 0x100 != 0 {
        if m.lw(b + 0x2c) != 0 {
            sound(m, c, 0x3d);
        }
        let b = m.lw(FREE_LIST);
        let cursor = m.lw(b + 0x2c).wrapping_sub(4);
        m.sw(b + 0x2c, cursor);
        if (cursor as i32) < 0 {
            m.sw(b + 0x2c, 0);
        }
        free_list_moved(m, c, frame, 2);
    }
    free_list_rows(m, c);
    let b = m.lw(FREE_LIST);
    let flash = m.lw(b + 0x34);
    if (flash as i32) > 0 {
        let flags = m.lw(b + 0x30);
        if flags & 2 != 0 {
            style(m, c, 5, 1);
            style(m, c, 6, 2);
        } else if flags & 4 != 0 {
            style(m, c, 5, 2);
            style(m, c, 6, 1);
        }
        let b = m.lw(FREE_LIST);
        let flash = m.lw(b + 0x34);
        m.sw(b + 0x34, flash.wrapping_sub(1));
    } else {
        let flags = m.lw(b + 0x30);
        let cursor = m.lw(b + 0x2c);
        m.sw(b + 0x30, flags & !6);
        style(m, c, 5, if cursor == 0 { 0 } else { 2 });
        let b = m.lw(FREE_LIST);
        let last = m.lw(b + 0x28).wrapping_sub(1);
        let cursor = m.lw(b + 0x2c);
        style(m, c, 6, if cursor == last { 0 } else { 2 });
    }
    m.pop_frame(0x38);
    0
}

/// 800DE2F4: after a confirm, the cursor's mission id goes to context
/// +0x390. Returns 4.
pub fn free_list_commit_800de2f4(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let b = m.lw(FREE_LIST);
    if m.lw(b + 0x30) & 1 != 0 {
        let cursor = m.lw(b + 0x2c);
        let category = m.lbu(b.wrapping_add(cursor) + 4);
        let mission = c.call(m, MISSION_RECORD, [u32::from(category), 0, 0, 0]);
        let ctx = context(m);
        m.sh(ctx + 0x390, mission as u16);
    }
    m.pop_frame(0x18);
    4
}

// ---- inner state 6: difficulty --------------------------------------------

fn difficulty_block(m: &mut Machine) -> u32 {
    m.lw(DIFFICULTY)
}

/// The three panel image pairs: `copy`(rect 0x800F5B54 + 0x20*i, scene
/// +0x200C8 + 4*i) and (rect + 8, scene +0x200D4 + 4*i), then DrawSync(0).
fn panel_images(m: &mut Machine, c: &mut dyn Calls, copy: u32) {
    let mut first = PANEL_RECTS;
    let mut second = PANEL_RECTS + 8;
    for i in 0..3u32 {
        let scene = m.lw(SCENE);
        let image = m.lw(scene + (i << 2) + 0x2_00c8);
        c.call(m, copy, [first, image, 0, 0]);
        first += 0x10;
        let scene = m.lw(SCENE);
        let image = m.lw(scene + (i << 2) + 0x2_00d4);
        c.call(m, copy, [second, image, 0, 0]);
        second += 0x10;
    }
    c.call(m, DRAW_SYNC, [0; 4]);
}

/// 800DD754: difficulty entry: common layer, the mission's image (waiting),
/// slots 1..4, phase 0, difficulty from context +0x398, the panel images
/// (LoadImage), page 0xB, the difficulty slots, label 0xB9. Returns 1.
pub fn difficulty_init_800dd754(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let frame = m.push_frame(0x38);
    let ctx = context(m);
    let mission = m.lh(ctx + 0x390) as i32 as u32;
    let category = c.call(m, MISSION_CATEGORY, [mission, 0, 0, 0]);
    common_layer(m, c);
    let scene = m.lw(SCENE);
    m.sw(frame + 0x10, 1);
    let table = m.lw(scene + 0x2_00a4);
    c.call(
        m,
        LOAD_RESOURCE,
        [table, category.wrapping_sub(1), 0x1480, 0],
    );
    style(m, c, 1, 3);
    style(m, c, 2, 4);
    style(m, c, 3, 4);
    style(m, c, 4, 4);
    let b = difficulty_block(m);
    let ctx = context(m);
    m.sh(b + 2, 0);
    m.sh(b, 0);
    m.sw(b + 0xc, 0);
    let difficulty = m.lw(ctx + 0x398);
    m.sw(b + 8, difficulty);
    panel_images(m, c, LOAD_IMAGE);
    c.call(m, PAGE, [0xb, 0, 0, 0]);
    for i in 0..3u32 {
        let b = difficulty_block(m);
        // The original passes slot 6 for every unselected entry.
        if m.lw(b + 8) == i {
            style(m, c, i + 5, 1);
        } else {
            style(m, c, 6, 2);
        }
    }
    c.call(m, CLEAR_LABELS, [0; 4]);
    c.call(m, LABEL, [0xb9, 1, 0, 0]);
    m.pop_frame(0x38);
    1
}

/// 800DD918: difficulty update: common layer, phase routine (result 4:
/// 0x100 when confirmed, flag 1, else 0x200), then slots 5..7.
pub fn difficulty_update_800dd918(m: &mut Machine, c: &mut dyn Calls, args: [u32; 4]) -> u32 {
    m.push_frame(0x20);
    common_layer(m, c);
    let b = difficulty_block(m);
    let routine = phase_routine(m, DIFFICULTY_PHASES, b);
    let result = c.call(m, routine, [b, args[1], args[2], args[3]]);
    let b = difficulty_block(m);
    if !advance(m, b, result) && result & 4 != 0 {
        let b = difficulty_block(m);
        let value = if m.lw(b + 0xc) & 1 != 0 { 0x100 } else { 0x200 };
        m.pop_frame(0x20);
        return value;
    }
    for i in 0..3u32 {
        let b = difficulty_block(m);
        let value = if m.lw(b + 8) == i { 1 } else { 2 };
        style(m, c, 5 + i, value);
    }
    m.pop_frame(0x20);
    0
}

/// 800DD2A0: once scene +0x200E4 bit 0 is set: panel timer 25, slot 2 at
/// x -625, sound 0x5A. Returns 1.
pub fn difficulty_reveal_800dd2a0(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let scene = m.lw(SCENE);
    if m.lw(scene + 0x2_00e4) & 1 != 0 {
        let b = difficulty_block(m);
        m.sw(b + 0x10, 25);
        c.call(m, OFFSET, [2, (-0x271i32) as u32, 0, 0]);
        sound(m, c, 0x5a);
    }
    m.pop_frame(0x18);
    1
}

/// 800DD300: slide panel 2 in (x = -t*t after the decrement); sound 0x5C
/// at the end. Returns 2 when done or while scene +0x200E4 bit 0 is clear.
pub fn difficulty_slide_800dd300(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let scene = m.lw(SCENE);
    let result = if m.lw(scene + 0x2_00e4) & 1 == 0 {
        2
    } else {
        let b = difficulty_block(m);
        let t = m.lw(b + 0x10);
        let next = t.wrapping_sub(1);
        if (t as i32) > 0 {
            m.sw(b + 0x10, next);
            c.call(m, OFFSET, [2, next.wrapping_neg().wrapping_mul(next), 0, 0]);
            0
        } else {
            sound(m, c, 0x5c);
            2
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800DD38C: clear scene +0x200E4 bit 0. Returns 1.
pub fn difficulty_settle_800dd38c(m: &mut Machine) -> u32 {
    let scene = m.lw(SCENE) + 0x2_0000;
    let flags = m.lw(scene + 0xe4);
    m.sw(scene + 0xe4, flags & !1);
    1
}

/// 800DD3B4: difficulty input. The mission name (font 8); confirm (0x10)
/// stores the difficulty to context +0x398, sets flag 1 and scene +0x200E4
/// bit 0, plays 0x3C; cancel plays 0x3B (both return 2); pressed bits 2/1
/// step the difficulty within 0..2 (sound 0x3D). Returns 0.
pub fn difficulty_input_800dd3b4(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    let frame = m.push_frame(0x30);
    let ctx = context(m);
    let mission = m.lh(ctx + 0x390) as i32 as u32;
    m.sw(frame + 0x10, 0x38);
    m.sw(frame + 0x18, GREY);
    m.sw(frame + 0x14, 0xd6);
    let name = m.lw(mission.wrapping_mul(12).wrapping_add(MISSIONS) + 8);
    let scene = m.lw(SCENE);
    let ot = m.lw(scene + 0x90);
    let prim = m.lw(scene + 0x2_0094);
    let next = c.call(m, TEXT, [8, name, ot, prim]);
    let scene = m.lw(SCENE) + 0x2_0000;
    let ctx = context(m);
    m.sw(scene + 0x94, next);
    let pressed = m.lw(ctx + 0x12c);
    let result = if pressed & 0x10 != 0 {
        let b = difficulty_block(m);
        let flags = m.lw(b + 0xc);
        let difficulty = m.lw(b + 8);
        m.sw(b + 0xc, flags | 1);
        m.sw(ctx + 0x398, difficulty);
        let flags = m.lw(scene + 0xe4);
        m.sw(scene + 0xe4, flags | 1);
        sound(m, c, 0x3c);
        2
    } else if pressed & 0x20 != 0 {
        sound(m, c, 0x3b);
        2
    } else if pressed & 2 != 0 {
        let b = difficulty_block(m);
        if (m.lw(b + 8) as i32) < 2 {
            sound(m, c, 0x3d);
            let b = difficulty_block(m);
            let difficulty = m.lw(b + 8);
            m.sw(b + 8, difficulty.wrapping_add(1));
        }
        0
    } else if pressed & 1 != 0 {
        let b = difficulty_block(m);
        if (m.lw(b + 8) as i32) > 0 {
            sound(m, c, 0x3d);
            let b = difficulty_block(m);
            let difficulty = m.lw(b + 8);
            m.sw(b + 8, difficulty.wrapping_sub(1));
        }
        0
    } else {
        0
    };
    m.pop_frame(0x30);
    result
}

/// 800DD540: after a confirm: save the panel images (StoreImage), release
/// the arena to scene +0x200E0, panel timer 0, sound 0x5B, return 1;
/// otherwise 4.
pub fn difficulty_store_800dd540(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x30);
    let b = difficulty_block(m);
    if m.lw(b + 0xc) & 1 == 0 {
        m.pop_frame(0x30);
        return 4;
    }
    m.sw(b + 4, 0);
    panel_images(m, c, STORE_IMAGE);
    let scene = m.lw(SCENE);
    let mark = m.lw(scene + 0x2_00e0);
    arena::reset(m, mark);
    let b = difficulty_block(m);
    m.sw(b + 0x10, 0);
    sound(m, c, 0x5b);
    m.pop_frame(0x30);
    1
}

/// 800DD64C: slide panel 2 out over 25 updates while running the loader
/// row `[0x800F39FC]` (step +4; result 1 next step, 2 sets done flag 2).
/// Returns 4 once done and slid out.
pub fn difficulty_load_800dd64c(m: &mut Machine, c: &mut dyn Calls) -> u32 {
    m.push_frame(0x18);
    let b = difficulty_block(m);
    let t = m.lw(b + 0x10);
    let run = if (t as i32) < 25 {
        let next = t.wrapping_add(1);
        m.sw(b + 0x10, next);
        c.call(m, OFFSET, [2, next.wrapping_neg().wrapping_mul(next), 0, 0]);
        let b = difficulty_block(m);
        m.lw(b + 0xc) & 2 == 0
    } else {
        if m.lw(b + 0xc) & 2 != 0 {
            m.pop_frame(0x18);
            return 4;
        }
        true
    };
    if run {
        let b = difficulty_block(m);
        let row = m.lw(LOADER_ROW);
        let step = m.lw(b + 4);
        let routine = m.lw((step << 2).wrapping_add(row));
        let result = c.call(m, routine, [0; 4]);
        let b = difficulty_block(m);
        if result & 1 != 0 {
            let step = m.lw(b + 4);
            m.sw(b + 4, step.wrapping_add(1));
        } else if result & 2 != 0 {
            let flags = m.lw(b + 0xc);
            m.sw(b + 0xc, flags | 2);
        }
    }
    m.pop_frame(0x18);
    0
}

// ---- inner state 15: reload ------------------------------------------------

/// 800D3534: reload entry: phase 0, loader page 5, slot 1 at x -500 (y:
/// the incoming `a2`, which the original passes on unchanged), timer 25.
/// Returns 1.
pub fn reload_init_800d3534(m: &mut Machine, c: &mut dyn Calls, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let b = m.lw(RELOAD);
    m.sh(b + 2, 0);
    m.sh(b, 0);
    c.call(m, LOADER_PAGE, [5, 0, 0, 0]);
    let b = m.lw(RELOAD);
    m.sw(b + 4, 25);
    c.call(m, OFFSET, [1, (-0x1f4i32) as u32, args[2], 0]);
    m.pop_frame(0x18);
    1
}

/// 800D3584: reload update: slot 1 slides in (x = -t*20, y the incoming
/// `a2`) and then the
/// language's "now loading" text (font 0x13 at 200, 380); the loader phase
/// routine; result 4 finishes the loader, sets the background colour (143,
/// 135, 83), in free-mission mode 800CEC18, and returns 0x100.
pub fn reload_update_800d3584(m: &mut Machine, c: &mut dyn Calls, args: [u32; 4]) -> u32 {
    let frame = m.push_frame(0x30);
    let b = m.lw(RELOAD);
    let t = m.lw(b + 4);
    if (t as i32) > 0 {
        let next = t.wrapping_sub(1);
        m.sw(b + 4, next);
        let x = next.wrapping_neg();
        c.call(m, OFFSET, [1, ((x << 2).wrapping_add(x)) << 2, args[2], 0]);
    } else {
        let ctx = context(m);
        let scene = m.lw(SCENE);
        let language = m.lw(ctx + 0x38c);
        m.sw(frame + 0x10, 0xc8);
        m.sw(frame + 0x14, 0x17c);
        m.sw(frame + 0x18, GREY);
        let ot = m.lw(scene + 0x90);
        let prim = m.lw(scene + 0x2_0094);
        let string = m.lw((language << 2).wrapping_add(LOADING_TEXT));
        let next = c.call(m, TEXT, [0x13, string, ot, prim]);
        let scene = m.lw(SCENE);
        m.sw(scene + 0x2_0094, next);
    }
    let b = m.lw(RELOAD);
    let routine = phase_routine(m, RELOAD_PHASES, b);
    let result = c.call(m, routine, [b, 0, 0, 0]);
    let b = m.lw(RELOAD);
    let value = if advance(m, b, result) {
        0
    } else if result & 4 != 0 {
        c.call(m, LOADER_FINISH, [0; 4]);
        c.call(m, SCENE_COLOUR, [0x8f, 0x87, 0x53, 0]);
        let ctx = context(m);
        if m.lw(ctx + 0x970) & 0x10 != 0 {
            c.call(m, LOADER_RECT, [0; 4]);
        }
        0x100
    } else {
        0
    };
    m.pop_frame(0x30);
    value
}

/// The claims of [`dispatch`](fn@dispatch) with outside calls through any
/// [`Calls`].
pub mod routines {
    use super::*;

    /// Run a routine of this module by original address with the original
    /// argument registers; `None` for any other address.
    pub fn dispatch(
        m: &mut Machine,
        c: &mut dyn Calls,
        target: u32,
        args: [u32; 4],
    ) -> Option<u32> {
        let [a0, a1, a2, a3] = args;
        Some(match target {
            0x800d_e354 => free_list_init_800de354(m, c),
            0x800d_e3ec => free_list_update_800de3ec(m, c, args),
            0x800d_dbd0 => free_list_clear_800ddbd0(m),
            0x800d_dc10 => free_list_build_800ddc10(m, c),
            0x800d_dd2c => free_list_show_800ddd2c(m, c),
            0x800d_de5c => free_list_input_800dde5c(m, c),
            0x800d_e2f4 => free_list_commit_800de2f4(m, c),
            0x800d_da70 => free_list_page_800dda70(m, c, a0, a1, a2, a3),
            0x800d_d754 => difficulty_init_800dd754(m, c),
            0x800d_d918 => difficulty_update_800dd918(m, c, args),
            0x800d_d2a0 => difficulty_reveal_800dd2a0(m, c),
            0x800d_d300 => difficulty_slide_800dd300(m, c),
            0x800d_d38c => difficulty_settle_800dd38c(m),
            0x800d_d3b4 => difficulty_input_800dd3b4(m, c),
            0x800d_d540 => difficulty_store_800dd540(m, c),
            0x800d_d64c => difficulty_load_800dd64c(m, c),
            0x800d_3534 => reload_init_800d3534(m, c, args),
            0x800d_3584 => reload_update_800d3584(m, c, args),
            0x800d_b454 => records::records_init_800db454(m, c),
            0x800d_b618 => records::records_intro_800db618(m, c),
            0x800d_b6cc => records::records_input_800db6cc(m, c),
            0x800d_b8c8 => records::records_exit_800db8c8(m, c),
            _ => {
                match target {
                    0x800d_a9b8 => records::draw_records_800da9b8(m, c, a0),
                    _ => return settings::routines::dispatch(m, c, target, args),
                }
                0
            }
        })
    }
}

/// Every routine this module and its files translate (original addresses).
pub const ROUTINES: [u32; 49] = [
    0x800d_0d30,
    0x800d_0db8,
    0x800d_3534,
    0x800d_3584,
    0x800d_a9b8,
    0x800d_b454,
    0x800d_b618,
    0x800d_b6cc,
    0x800d_b8c8,
    0x800d_d2a0,
    0x800d_d300,
    0x800d_d38c,
    0x800d_d3b4,
    0x800d_d540,
    0x800d_d64c,
    0x800d_d754,
    0x800d_d918,
    0x800d_da70,
    0x800d_dbd0,
    0x800d_dc10,
    0x800d_dd2c,
    0x800d_de5c,
    0x800d_e2f4,
    0x800d_e354,
    0x800d_e3ec,
    0x800e_4978,
    0x800e_49dc,
    0x800e_4b10,
    0x800e_4b44,
    0x800e_4cf0,
    0x800e_4d6c,
    0x800e_5288,
    0x800e_5328,
    0x800e_53c8,
    0x800e_571c,
    0x800e_594c,
    0x800e_59e8,
    0x800e_5c98,
    0x800e_5d50,
    0x800e_5ecc,
    0x800e_62c0,
    0x800e_6378,
    0x800e_6504,
    0x800e_66a0,
    0x800e_6d44,
    0x800e_6dfc,
    0x800e_6ec0,
    0x800e_72c4,
    0x800e_73dc,
];

/// Claims this module's routines while overlay module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    routines::dispatch(m, &mut Dispatch, target, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_start_rounds_toward_zero() {
        assert_eq!(page_start(0), 0);
        assert_eq!(page_start(5), 4);
        assert_eq!(page_start(-1i32 as u32), 0);
        assert_eq!(page_start(-5i32 as u32), -4i32 as u32);
    }
}
