//! JP overlay 2, part 3: the panels of the JP save screens and the story
//! screen's input handlers.
//!
//! `[0x80112DF8]` is the save screen's state block (+0 state, +1 step, +3
//! pending choice, +4 selected port, +5 account, +8 / +9 flags);
//! `[0x80112DC0] + 0x5C` the screen's message block (+1 message, +2 / +3
//! port marks, +8 / +0x1E / +0x2E flags); the scene block `[0x80112DC4]`
//! holds the story screen's handler table indices (+0x84 / +0x86), its
//! selection (+0xE0) and its pending transition (+0x94).
use super::ov2_p3::{DRAW, MODULE, RCOS, RSIN, SCENE, call5, div4096, mult_hi, sext16};
use super::ov2_p3_records as records;
use crate::common::{CONTEXT, GAMEPLAY, loaded_module};
use crate::dispatch::call;
use crate::machine::{Machine, SmallData};

/// `[0x80112DF8]`: the save screen's state block.
pub const SAVE: u32 = 0x8011_2df8;
/// `[0x80112DA8]`: the story screen's text block.
const TEXT_BLOCK: u32 = 0x8011_2da8;
/// 0x8010B1E8: the story screen's handler tables (by +0x84, then +0x86).
const HANDLERS: u32 = 0x8010_b1e8;
/// 0x800BCE48 / 0x800BCE58 (`[gp+0x2D4]` / `[gp+0x2E4]`): the resident
/// settings block and the camera table.
const SETTINGS: u32 = 0x800b_ce48;
const CAMERAS: u32 = 0x800b_ce58;
/// 0x800CCE40 the empty-account text; 0x800CCE48 "no data"; 0x800CCE50 ..
/// 0x800CCE68 the four record labels.
const NO_ACCOUNT: u32 = 0x800c_ce40;
const NO_RECORD: u32 = 0x800c_ce48;
const LABELS: [u32; 4] = [0x800c_ce50, 0x800c_ce58, 0x800c_ce60, 0x800c_ce68];

/// Resident routines: the sound request (8002DB4C, `script`), 8007B438 and
/// 80068334 (`render`), TransposeMatrix (`sdk`).
const SOUND: u32 = 0x8002_db4c;
const CAMERA_SETUP: u32 = 0x8007_b438;
const SCENE_PRIMITIVES: u32 = 0x8006_8334;
const TRANSPOSE_MATRIX: u32 = 0x800a_7448;
/// Other parts of overlay 2.
const DIALOG_OPEN: u32 = 0x800d_edf0;
const DIALOG_SET: u32 = 0x800d_ee08;
const DIALOG_STATE: u32 = 0x800d_ee84;
const GAUGE_LEVEL: u32 = 0x800d_eea8;
const STORY_SOUND: u32 = 0x800f_f19c;
const STORY_RESET: u32 = 0x8010_0994;
const TEXT_AT: u32 = 0x800f_33a8;
const TEXT_RGB: u32 = 0x800f_33d0;
const TEXT_F33F8: u32 = 0x800f_33f8;
const TEXT_F34B0: u32 = 0x800f_34b0;
const TEXT_F34C8: u32 = 0x800f_34c8;
const TEXT_F35B8: u32 = 0x800f_35b8;
const TEXT_PRINT: u32 = 0x800f_4ae0;

fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

/// The save screen's message block (`[[0x80112DC0] + 0x5C]`).
pub fn messages(m: &mut Machine) -> u32 {
    let draw = m.lw(DRAW);
    m.lw(draw.wrapping_add(0x5c))
}

/// A pad button newly pressed (context +0x12C & `down`) with none of
/// `held` held (+0x128).
pub fn pressed(m: &mut Machine, down: u32, held: u32) -> bool {
    let ctx = context(m);
    m.lw(ctx.wrapping_add(0x12c)) & down != 0 && m.lw(ctx.wrapping_add(0x128)) & held == 0
}

/// The text settings shared by the panels.
fn text_setup(m: &mut Machine) {
    call(m, TEXT_F35B8, [4, 0, 0, 0]);
    call(m, TEXT_F33F8, [2, 0, 0, 0]);
    call(m, TEXT_F34C8, [1, 0xa, 0x14, 0]);
    let scale = call(m, GAUGE_LEVEL, [0xf, 0, 0, 0]);
    call(m, TEXT_F34B0, [scale, 0, 0, 0]);
}

/// The `0x1B4E81B5` (`sra 6` / `sra 5`) signed divisions by 600 and 300.
fn by_600(x: u32) -> u32 {
    ((mult_hi(x, 0x1b4e_81b5) as i32 >> 6).wrapping_sub(x as i32 >> 31)) as u32
}
fn by_300(x: u32) -> u32 {
    ((mult_hi(x, 0x1b4e_81b5) as i32 >> 5).wrapping_sub(x as i32 >> 31)) as u32
}

/// 800E5F88: the two cursor trails at (`x`, `y`): per trail its ten
/// sprites (16 bytes, scene +0x40 + 0x250) move one step along (each takes
/// the previous one's position) and are linked first into the context's
/// ordering table (+8); the head moves on a wave of the scene's frame
/// counter (+0x50), the second trail a quarter turn later.
pub fn trails_e5f88(m: &mut Machine, x: u32, y: u32) {
    m.push_frame(0x48);
    let scene = m.lw(SCENE);
    let ctx = context(m);
    let (x, y) = (x & 0xffff, y & 0xffff);
    let sprites = m.lw(scene.wrapping_add(0x40));
    let ot = m.lw(ctx.wrapping_add(8));
    for trail in 0..2u32 {
        let first = trail * 10;
        let mut j = 9u32;
        loop {
            let at = (first + j) * 16;
            let sprite = sprites.wrapping_add(at);
            let before = sprites.wrapping_add((first + j - 1) * 16);
            let v = m.lhu(before.wrapping_add(0x258));
            m.sh(sprite.wrapping_add(0x258), v);
            let v = m.lhu(before.wrapping_add(0x25a));
            m.sh(sprite.wrapping_add(0x25a), v);
            link_first(m, ot, sprite.wrapping_add(0x250));
            j -= 1;
            if j == 0 {
                break;
            }
        }
        if trail == 0 {
            let head = sprites.wrapping_add(0x250);
            let scene = m.lw(SCENE);
            let t = m.lw(scene.wrapping_add(0x50));
            let s = call(m, RSIN, [t << 6, 0, 0, 0]);
            m.sh(head.wrapping_add(8), x.wrapping_add(by_600(s)) as u16);
            let scene = m.lw(SCENE);
            let t = m.lw(scene.wrapping_add(0x50));
            let s = call(m, RSIN, [t << 6, 0, 0, 0]);
            let scene = m.lw(SCENE);
            let t = m.lw(scene.wrapping_add(0x50));
            let c = call(m, RCOS, [t << 6, 0, 0, 0]);
            let v = y.wrapping_add(by_300(s)).wrapping_add(by_600(c));
            m.sh(head.wrapping_add(0xa), v as u16);
        } else {
            let head = sprites.wrapping_add(0x2f0);
            let scene = m.lw(SCENE);
            let t = m.lw(scene.wrapping_add(0x50));
            let s = call(m, RSIN, [(t << 6).wrapping_add(0x400), 0, 0, 0]);
            m.sh(head.wrapping_add(8), x.wrapping_add(by_600(s)) as u16);
            let scene = m.lw(SCENE);
            let t = m.lw(scene.wrapping_add(0x50));
            let s = call(m, RSIN, [(t << 6).wrapping_add(0x400), 0, 0, 0]);
            let scene = m.lw(SCENE);
            let t = m.lw(scene.wrapping_add(0x50));
            let c = call(m, RCOS, [(t << 6).wrapping_add(0x400), 0, 0, 0]);
            let v = y
                .wrapping_add(by_300(s.wrapping_neg()))
                .wrapping_add(by_600(c));
            m.sh(head.wrapping_add(0xa), v as u16);
        }
        link_first(m, ot, sprites.wrapping_add(trail * 160 + 0x250));
    }
    m.pop_frame(0x48);
}

/// `addPrim(ot, prim)` (the 0xFF000000 / 0x00FFFFFF masks).
fn link_first(m: &mut Machine, ot: u32, prim: u32) {
    let tag = m.lw(prim);
    let next = m.lw(ot);
    m.sw(prim, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let next = m.lw(ot);
    m.sw(ot, (next & 0xff00_0000) | (prim & 0x00ff_ffff));
}

/// 800E6288: open the story screen's broadcast: dialog 0 with 0x01004001,
/// the story sound 3, the reset (80100994), sound request 0x68, and the
/// message flag +0x2E from context +0x96C bit 0x10.
pub fn open_broadcast_e6288(m: &mut Machine) {
    m.push_frame(0x20);
    let ctx = context(m);
    m.sh(ctx.wrapping_add(0x954), 1);
    call(m, DIALOG_OPEN, [0, 0x0100_4001, 0, 0]);
    let scene = m.lw(SCENE);
    m.sh(scene.wrapping_add(0x86), 1);
    call(m, STORY_SOUND, [3, 0, 0, 0]);
    call(m, STORY_RESET, [0, 0, 0, 0]);
    call(m, SOUND, [0x68, 0, 0, 0]);
    let ctx = context(m);
    let flag = m.lw(ctx.wrapping_add(0x96c)) & 0x10 != 0;
    let block = messages(m);
    m.sb(block.wrapping_add(0x2e), u8::from(!flag));
    m.pop_frame(0x20);
}

/// 800E6334: the broadcast's frame: a press of 0x10 (none of 0x2F held)
/// silences the story sound; while the text block's +0x14 is clear, the
/// three dialogs are set and the step (+0x86) advances.
pub fn broadcast_e6334(m: &mut Machine) {
    m.push_frame(0x18);
    if pressed(m, 0x10, 0x2f) {
        call(m, STORY_SOUND, [0, 0, 0, 0]);
    }
    let text = m.lw(TEXT_BLOCK);
    if m.lw(text.wrapping_add(0x14)) == 0 {
        call(m, DIALOG_SET, [1, 3, 0x1e, 0]);
        call(m, DIALOG_SET, [0xf, 3, u32::MAX, u32::MAX]);
        call(m, DIALOG_SET, [0xb, 3, u32::MAX, u32::MAX]);
        let scene = m.lw(SCENE);
        let step = m.lhu(scene.wrapping_add(0x86));
        m.sh(scene.wrapping_add(0x86), step.wrapping_add(1));
    }
    m.pop_frame(0x18);
}

/// 800E63F4: the end of the broadcast: with dialog 1 closed, the next
/// transition by the mission (context +0x38C: 0x10, 0x25, 0x2A or other);
/// open, the gauge colour (settings +0x10..+0x12: 143, 135, 83 of 30ths
/// of the gauge level) and settings flag 8.
pub fn broadcast_end_e63f4(m: &mut Machine) {
    m.push_frame(0x18);
    let level = call(m, GAUGE_LEVEL, [1, 0, 0, 0]);
    if call(m, DIALOG_STATE, [1, 0, 0, 0]) == 0 {
        let ctx = context(m);
        match m.lh(ctx.wrapping_add(0x38c)) {
            0x10 => {
                m.sb(ctx.wrapping_add(0x3a8), 8);
                let ctx = context(m);
                let scene = m.lw(SCENE);
                m.sw(ctx.wrapping_add(0x3a0), 2);
                m.sw(scene.wrapping_add(0x94), 0x6000_0005);
            }
            0x25 => {
                m.sb(ctx.wrapping_add(0x3a8), 0x1a);
                let ctx = context(m);
                m.sh(ctx.wrapping_add(0x38c), 0x39);
                let scene = m.lw(SCENE);
                m.sw(ctx.wrapping_add(0x3a0), 2);
                m.sw(scene.wrapping_add(0x94), 0x6000_0005);
            }
            0x2a => {
                let scene = m.lw(SCENE);
                m.sh(ctx.wrapping_add(0x38c), 0x38);
                m.sw(scene.wrapping_add(0x94), 0x6000_0002);
            }
            _ => {
                let scene = m.lw(SCENE);
                m.sw(scene.wrapping_add(0x94), 0x6000_0002);
            }
        }
    } else {
        let by_30 = |x: u32| {
            ((mult_hi(x, 0x8888_8889).wrapping_add(x) as i32 >> 4).wrapping_sub(x as i32 >> 31))
                as u32
        };
        let red = by_30(level.wrapping_mul(143));
        let green = by_30(level.wrapping_mul(135));
        let blue = by_30(level.wrapping_mul(83));
        let settings = m.lw(SETTINGS);
        m.sb(settings.wrapping_add(0x10), red as u8);
        let settings = m.lw(SETTINGS);
        m.sb(settings.wrapping_add(0x11), green as u8);
        let settings = m.lw(SETTINGS);
        m.sb(settings.wrapping_add(0x12), blue as u8);
        let settings = m.lw(SETTINGS);
        let flags = m.lw(settings);
        m.sw(settings, flags | 8);
    }
    m.pop_frame(0x18);
}

/// 800E65A4: a press of 0x10 (none of 0x2F held) leaves for the hub
/// (context +0x3B0 = 2, transition 0x60000002).
pub fn leave_e65a4(m: &mut Machine) {
    if pressed(m, 0x10, 0x2f) {
        let ctx = context(m);
        let scene = m.lw(SCENE);
        m.sw(ctx.wrapping_add(0x3b0), 2);
        m.sw(scene.wrapping_add(0x94), 0x6000_0002);
    }
}

/// 800E65F8: step +0x86 = 1. Returns 1.
pub fn step_one_e65f8(m: &mut Machine) -> u32 {
    let scene = m.lw(SCENE);
    m.sh(scene.wrapping_add(0x86), 1);
    1
}

/// 800E660C: a press of 0x10 (none of 0x2F held) selects handler 5, step 0.
pub fn skip_e660c(m: &mut Machine) {
    if pressed(m, 0x10, 0x2f) {
        let scene = m.lw(SCENE);
        m.sh(scene.wrapping_add(0x84), 5);
        m.sh(scene.wrapping_add(0x86), 0);
    }
}

/// 800E6658: start the story screen: no selection, its handler (+0x84) by
/// the screen kind (context +0x954) and the story flags (+0x96C), step 0.
pub fn start_e6658(m: &mut Machine) {
    let scene = m.lw(SCENE);
    m.sb(scene.wrapping_add(0x7e), 0);
    let scene = m.lw(SCENE);
    m.sb(scene.wrapping_add(0x7d), 1);
    let ctx = context(m);
    let handler = match m.lh(ctx.wrapping_add(0x954)) {
        0 => 1,
        1 => 5,
        2 => {
            let flags = m.lw(ctx.wrapping_add(0x96c));
            if flags & 0x80 != 0 {
                7
            } else if flags & 0x40 != 0 {
                5
            } else {
                7
            }
        }
        3 => 8,
        _ => 7,
    };
    let scene = m.lw(SCENE);
    m.sh(scene.wrapping_add(0x84), handler);
    let ctx = context(m);
    let flags = m.lw(ctx.wrapping_add(0x96c));
    if flags & 0x20 != 0 {
        let scene = m.lw(SCENE);
        m.sh(scene.wrapping_add(0x84), 5);
        m.sh(scene.wrapping_add(0x86), 0);
    } else if flags & 8 != 0 {
        let scene = m.lw(SCENE);
        m.sh(scene.wrapping_add(0x84), 6);
    } else if m.lh(ctx.wrapping_add(0x38c)) == 0x3b {
        let scene = m.lw(SCENE);
        m.sh(scene.wrapping_add(0x84), 8);
    }
    let scene = m.lw(SCENE);
    m.sh(scene.wrapping_add(0x86), 0);
}

/// 800E67AC: the 3D backdrop's frame setup: the camera (8007B438 of
/// gameplay +0x28), the transposed view matrix, and two drawing-offset
/// primitives (0xE5) around the scene primitives (80068334): the first at
/// the camera's screen centre, linked at ordering-table entry 1, the
/// second at its offset centre, at entry 0xFFF.
pub fn backdrop_e67ac(m: &mut Machine) {
    m.push_frame(0x28);
    let gameplay = m.gw(GAMEPLAY);
    let camera = m.lw(gameplay.wrapping_add(0x28));
    call(m, CAMERA_SETUP, [camera, 0, 0, 0]);
    let g = m.gw(GAMEPLAY);
    call(
        m,
        TRANSPOSE_MATRIX,
        [g.wrapping_add(0x2c), g.wrapping_add(0x4c), 0, 0],
    );
    let ctx = context(m);
    let index = m.lw(ctx.wrapping_add(4));
    let g = m.gw(GAMEPLAY);
    let table = m.lw(CAMERAS);
    let view = m.lw(table.wrapping_add(index.wrapping_mul(4116)));
    let prim = m.lw(g.wrapping_add(0x14));
    let x = m.lh(view.wrapping_add(8)) as i32 as u32;
    let y = m.lh(view.wrapping_add(0xa)) as i32 as u32;
    let w = u32::from(m.lhu(view.wrapping_add(4)));
    let h = u32::from(m.lhu(view.wrapping_add(6)));
    m.sw(g.wrapping_add(0x14), prim.wrapping_add(0xc));
    let half_w = ((sext16(w) as i32) >> 1) as u32;
    m.sh(g.wrapping_add(0x294), x as u16);
    let cx = x.wrapping_add(half_w);
    let half_h = ((sext16(h) as i32) >> 1) as u32;
    let ox = m.lh(g.wrapping_add(0x294)) as i32 as u32;
    m.sh(g.wrapping_add(0x296), y as u16);
    let oy = m.lh(g.wrapping_add(0x296)) as i32 as u32;
    let cy = y.wrapping_add(half_h);
    m.sh(g.wrapping_add(0x298), cx as u16);
    m.sh(g.wrapping_add(0x29a), cy as u16);
    m.sw(prim.wrapping_add(8), 0);
    m.sw(prim.wrapping_add(4), (oy << 11) | ox | 0xe500_0000);
    let ot = m.lw(g.wrapping_add(0x10));
    let tag = m.lw(prim);
    let next = m.lw(ot.wrapping_add(4));
    m.sw(prim, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let ot = m.lw(g.wrapping_add(0x10));
    let next = m.lw(ot.wrapping_add(4));
    m.sw(
        ot.wrapping_add(4),
        (next & 0xff00_0000) | (prim & 0x00ff_ffff),
    );
    let ot = m.lw(g.wrapping_add(0x10));
    let cursor = m.lw(g.wrapping_add(0x14));
    // a3 still holds the screen centre x: 80068334 takes it as its
    // position list and clears its first word
    let prim = call(
        m,
        SCENE_PRIMITIVES,
        [g.wrapping_add(0x244), ot.wrapping_add(4), cursor, cx],
    );
    let g = m.gw(GAMEPLAY);
    let cx = m.lh(g.wrapping_add(0x298)) as i32 as u32;
    let cy = m.lh(g.wrapping_add(0x29a)) as i32 as u32;
    m.sw(g.wrapping_add(0x14), prim.wrapping_add(0xc));
    m.sw(prim.wrapping_add(8), 0);
    m.sw(prim.wrapping_add(4), (cy << 11) | cx | 0xe500_0000);
    let ot = m.lw(g.wrapping_add(0x10));
    let tag = m.lw(prim);
    let next = m.lw(ot.wrapping_add(0x3ffc));
    m.sw(prim, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let ot = m.lw(g.wrapping_add(0x10));
    let next = m.lw(ot.wrapping_add(0x3ffc));
    m.sw(
        ot.wrapping_add(0x3ffc),
        (next & 0xff00_0000) | (prim & 0x00ff_ffff),
    );
    m.pop_frame(0x28);
}

/// 800E6954: run the story screen's handler (+0x84, step +0x86; tables
/// 0x8010B1E8). Returns its result.
pub fn run_handler_e6954(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let scene = m.lw(SCENE);
    let kind = m.lh(scene.wrapping_add(0x84)) as i32 as u32;
    let table = m.lw((kind << 2).wrapping_add(HANDLERS));
    let step = m.lh(scene.wrapping_add(0x86)) as i32 as u32;
    let handler = m.lw((step << 2).wrapping_add(table));
    let v0 = call(m, handler, [scene, 0, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 800E69A8: the save screen state after the story choice `choice` (scene
/// +0xE0): 0 -> 0x12, 1 -> 0x11, 2 -> 0 (context +0x964 bit 0 cleared,
/// transition 0x60000004, save flag +2), 3 -> 0x12 (screen kind 3, story
/// flag 0x10, save flag +2), else 0.
pub fn next_state_e69a8(m: &mut Machine, choice: u32) -> u32 {
    match choice {
        0 => 0x12,
        1 => 0x11,
        2 => {
            let ctx = context(m);
            let scene = m.lw(SCENE);
            let flags = m.lw(ctx.wrapping_add(0x964));
            let save = m.lw(SAVE);
            m.sw(ctx.wrapping_add(0x964), flags & !1);
            m.sw(scene.wrapping_add(0x94), 0x6000_0004);
            m.sb(save.wrapping_add(2), 1);
            0
        }
        3 => {
            let ctx = context(m);
            m.sh(ctx.wrapping_add(0x954), 3);
            let flags = m.lw(ctx.wrapping_add(0x96c));
            let save = m.lw(SAVE);
            m.sw(ctx.wrapping_add(0x96c), flags | 0x10);
            m.sb(save.wrapping_add(2), 1);
            0x12
        }
        _ => 0,
    }
}

/// 800E6A6C: the account panel: the three account names (grey "--" for an
/// absent account; with `exclude` 1 the current one counts as absent), at
/// x 0x51 (`column` 0) or 0x38 (`column` 1).
pub fn accounts_e6a6c(m: &mut Machine, exclude: u32, column: u32) {
    m.push_frame(0x28);
    let mut present = records::accounts_e456c(m);
    if exclude == 1 {
        let save = m.lw(SAVE);
        let account = u32::from(m.lbu(save.wrapping_add(5)));
        present &= !(1u32 << (account & 31));
    }
    text_setup(m);
    for (row, y) in [0x0d6u32, 0x10b, 0x140].into_iter().enumerate() {
        match column {
            0 => {
                call(m, TEXT_AT, [0x51, y, 0, 0]);
            }
            1 => {
                call(m, TEXT_AT, [0x38, y, 0, 0]);
            }
            _ => {}
        }
        let text = if ((present as i32) >> row) & 1 != 0 {
            call(m, TEXT_RGB, [0x80, 0x80, 0x80, 0]);
            records::account_name_e425c(m, row as u32 + 1)
        } else {
            call(m, TEXT_RGB, [0x40, 0x40, 0x40, 0]);
            NO_ACCOUNT
        };
        call(m, TEXT_PRINT, [text, 0, 0, 0]);
    }
    m.pop_frame(0x28);
}

/// 800E6C4C: the record panel of mission record `slot` (with the setting
/// of account `account`): name, unit, time, level, setting; "no data" for
/// an absent record or `none` 1.
pub fn record_e6c4c(m: &mut Machine, slot: u32, account: u32, none: u32) {
    m.push_frame(0x28);
    let present = records::records_e458c(m);
    text_setup(m);
    call(m, TEXT_RGB, [0x80, 0x80, 0x80, 0]);
    call(m, TEXT_F35B8, [3, 0, 0, 0]);
    let slot = slot & 0xff;
    if none != 1 && present & (1u32 << (slot & 31)) != 0 {
        call(m, TEXT_AT, [0x5e, 0x126, 0, 0]);
        let text = records::name_e44e0(m, slot);
        call(m, TEXT_PRINT, [text, 0, 0, 0]);
        type Value = fn(&mut Machine, u32) -> u32;
        let rows: [(u32, u32, Value); 3] = [
            (0x13d, 0x13d, records::value_e435c),
            (0x14e, 0x14e, records::value_e43b4),
            (0x160, 0x161, records::value_e4518),
        ];
        for (i, (y, value_y, value)) in rows.into_iter().enumerate() {
            call(m, TEXT_AT, [0x5e, y, 0, 0]);
            call(m, TEXT_PRINT, [LABELS[i], 0, 0, 0]);
            call(m, TEXT_AT, [0x8c, value_y, 0, 0]);
            let text = value(m, slot);
            call(m, TEXT_PRINT, [text, 0, 0, 0]);
        }
        call(m, TEXT_AT, [0x5e, 0x171, 0, 0]);
        call(m, TEXT_PRINT, [LABELS[3], 0, 0, 0]);
        call(m, TEXT_AT, [0x8c, 0x171, 0, 0]);
        let text = records::value_e45ac(m, account & 0xff);
        call(m, TEXT_PRINT, [text, 0, 0, 0]);
    } else {
        call(m, TEXT_AT, [0x8c, 0x14b, 0, 0]);
        call(m, TEXT_PRINT, [NO_RECORD, 0, 0, 0]);
    }
    m.pop_frame(0x28);
}

/// `bgez; addiu 0x1FF; sra 9`: signed division by 512.
fn div512(v: u32) -> u32 {
    let v = v as i32;
    (if v < 0 { v.wrapping_add(0x1ff) } else { v } >> 9) as u32
}

/// `srl 31; addu; sra 1`: signed division by 2.
fn half(v: u32) -> u32 {
    ((v.wrapping_add(v >> 31) as i32) >> 1) as u32
}

/// One rsin / rcos pair in the original's order: (first, second).
fn pair(m: &mut Machine, first: u32, second: u32, angle: u32) -> (u32, u32) {
    let a = call(m, first, [angle, 0, 0, 0]);
    let b = call(m, second, [angle, 0, 0, 0]);
    (a, b)
}

/// 800E6DFC: the selection marker at (`x`, `y`): two textured quads
/// (POLY_FT4, texture page 0x1F, CLUT 0x298) rotated by `angle` and by
/// `-angle`, from the primitive buffer at the fifth argument, linked at
/// `[entry]`. Returns the buffer past them.
pub fn marker_e6dfc(m: &mut Machine, x: u32, y: u32, entry: u32, angle: u32, buffer: u32) -> u32 {
    m.push_frame(0x38);
    let p = buffer;
    m.sb(p.wrapping_add(3), 9);
    m.sb(p.wrapping_add(7), 0x2c);
    m.sh(p.wrapping_add(0x16), 0x1f);
    m.sh(p.wrapping_add(0xe), 0x298);
    m.sb(p.wrapping_add(4), 0x80);
    m.sb(p.wrapping_add(5), 0x80);
    m.sb(p.wrapping_add(6), 0x80);
    m.sb(p.wrapping_add(0xd), 0xf0);
    m.sb(p.wrapping_add(0x15), 0xf0);
    m.sb(p.wrapping_add(0xc), 0x10);
    m.sb(p.wrapping_add(0x14), 0x17);
    m.sb(p.wrapping_add(0x1c), 0x10);
    m.sb(p.wrapping_add(0x1d), 0xff);
    m.sb(p.wrapping_add(0x24), 0x17);
    m.sb(p.wrapping_add(0x25), 0xff);
    let n8 = |v: u32| v.wrapping_mul(8).wrapping_neg();
    let p8 = |v: u32| v.wrapping_mul(8);
    let (c, s) = pair(m, RCOS, RSIN, angle);
    let v = x.wrapping_add(half(div4096(n8(c)).wrapping_add(div4096(n8(s)))));
    m.sh(p.wrapping_add(8), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, angle);
    let v = y.wrapping_add(div4096(p8(s)).wrapping_add(div4096(n8(c))));
    m.sh(p.wrapping_add(0xa), v as u16);
    let (c, s) = pair(m, RCOS, RSIN, angle);
    let v = x.wrapping_add(half(div512(c).wrapping_add(div4096(n8(s)))));
    m.sh(p.wrapping_add(0x10), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, angle);
    let v = y.wrapping_add(div512(s.wrapping_neg()).wrapping_add(div4096(n8(c))));
    m.sh(p.wrapping_add(0x12), v as u16);
    let (c, s) = pair(m, RCOS, RSIN, angle);
    let v = x.wrapping_add(half(div4096(n8(c)).wrapping_add(div512(s))));
    m.sh(p.wrapping_add(0x18), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, angle);
    let v = y.wrapping_add(div4096(p8(s)).wrapping_add(div512(c)));
    m.sh(p.wrapping_add(0x1a), v as u16);
    let (c, s) = pair(m, RCOS, RSIN, angle);
    let v = x.wrapping_add(half(div512(c).wrapping_add(div512(s))));
    m.sh(p.wrapping_add(0x20), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, angle);
    let v = y.wrapping_add(div512(s.wrapping_neg()).wrapping_add(div512(c)));
    let back = angle.wrapping_neg();
    let tag = m.lw(p);
    m.sh(p.wrapping_add(0x22), v as u16);
    let next = m.lw(entry);
    m.sw(p, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let q = p.wrapping_add(0x28);
    let next = m.lw(entry);
    m.sw(entry, (next & 0xff00_0000) | (p & 0x00ff_ffff));
    m.sb(q.wrapping_add(3), 9);
    m.sb(q.wrapping_add(7), 0x2c);
    m.sh(q.wrapping_add(0x16), 0x1f);
    m.sh(q.wrapping_add(0xe), 0x298);
    m.sb(q.wrapping_add(4), 0x80);
    m.sb(q.wrapping_add(5), 0x80);
    m.sb(q.wrapping_add(6), 0x80);
    m.sb(q.wrapping_add(0xd), 0xe4);
    m.sb(q.wrapping_add(0x15), 0xe4);
    m.sb(q.wrapping_add(0xc), 0);
    m.sb(q.wrapping_add(0x14), 0xd);
    m.sb(q.wrapping_add(0x1c), 0);
    m.sb(q.wrapping_add(0x1d), 0xff);
    m.sb(q.wrapping_add(0x24), 0xd);
    m.sb(q.wrapping_add(0x25), 0xff);
    let p15 = |v: u32| v.wrapping_mul(15);
    let n15 = |v: u32| v.wrapping_mul(15).wrapping_neg();
    let (c, s) = pair(m, RCOS, RSIN, back);
    let v = x.wrapping_add(half(div4096(n15(c)).wrapping_add(div4096(n15(s)))));
    m.sh(q.wrapping_add(8), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, back);
    let v = y.wrapping_add(div4096(p15(s)).wrapping_add(div4096(n15(c))));
    m.sh(q.wrapping_add(0xa), v as u16);
    let (c, s) = pair(m, RCOS, RSIN, back);
    let v = x.wrapping_add(half(div4096(p15(c)).wrapping_add(div4096(n15(s)))));
    m.sh(q.wrapping_add(0x10), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, back);
    let v = y.wrapping_add(div4096(n15(s)).wrapping_add(div4096(n15(c))));
    m.sh(q.wrapping_add(0x12), v as u16);
    let (c, s) = pair(m, RCOS, RSIN, back);
    let v = x.wrapping_add(half(div4096(n15(c)).wrapping_add(div4096(p15(s)))));
    m.sh(q.wrapping_add(0x18), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, back);
    let v = y.wrapping_add(div4096(p15(s)).wrapping_add(div4096(p15(c))));
    m.sh(q.wrapping_add(0x1a), v as u16);
    let (c, s) = pair(m, RCOS, RSIN, back);
    let v = x.wrapping_add(half(div4096(p15(c)).wrapping_add(div4096(p15(s)))));
    m.sh(q.wrapping_add(0x20), v as u16);
    let (s, c) = pair(m, RSIN, RCOS, back);
    let v = y.wrapping_add(div4096(n15(s)).wrapping_add(div4096(p15(c))));
    let tag = m.lw(q);
    m.sh(q.wrapping_add(0x22), v as u16);
    let next = m.lw(entry);
    m.sw(q, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let next = m.lw(entry);
    m.sw(entry, (next & 0xff00_0000) | (q & 0x00ff_ffff));
    m.pop_frame(0x38);
    buffer.wrapping_add(0x50)
}

/// The routines of this file, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800e_5f88 => {
            trails_e5f88(m, a0, a1);
            0
        }
        0x800e_6288 => {
            open_broadcast_e6288(m);
            0
        }
        0x800e_6334 => {
            broadcast_e6334(m);
            0
        }
        0x800e_63f4 => {
            broadcast_end_e63f4(m);
            0
        }
        0x800e_65a4 => {
            leave_e65a4(m);
            0
        }
        0x800e_65f8 => step_one_e65f8(m),
        0x800e_660c => {
            skip_e660c(m);
            0
        }
        0x800e_6658 => {
            start_e6658(m);
            0
        }
        0x800e_67ac => {
            backdrop_e67ac(m);
            0
        }
        0x800e_6954 => run_handler_e6954(m),
        0x800e_69a8 => next_state_e69a8(m, a0),
        0x800e_6a6c => {
            accounts_e6a6c(m, a0, a1);
            0
        }
        0x800e_6c4c => {
            record_e6c4c(m, a0, a1, a2);
            0
        }
        0x800e_6dfc => {
            let buffer = m.lw(m.sp.wrapping_add(0x10));
            marker_e6dfc(m, a0, a1, a2, a3, buffer)
        }
        _ => return None,
    })
}

/// A call of 800E6DFC with its fifth argument (for the menu files).
pub fn marker_call(m: &mut Machine, frame: u32, args: [u32; 4], buffer: u32) -> u32 {
    call5(m, frame, 0x800e_6dfc, args, buffer)
}
