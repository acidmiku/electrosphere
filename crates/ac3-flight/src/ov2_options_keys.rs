//! OPTIONS mode 1, KEY CONFIG: the controller settings screen of state 10.
//!
//! The screen keeps its state in the block `[0x800F6668]`:
//! - +0 sub-mode, +2 step (task machine 800E47E0, table 0x800F670C);
//! - +0xC flags word; +0x10 setting bits (1 analog, 2/4 the two analog
//!   options);
//! - +0x14..+0x22 the button assignment: one action index per button;
//! - +0x24..+0x27 analog axis actions, +0x28..+0x2B their inversions;
//! - +0x2C response (0..2), +0x2D neutral zone (0..2);
//! - +0x2E the selected row, +0x30 the controller type, +0x34 the type the
//!   screen was opened for;
//! - +0x48..+0x54 four layout tables (by type).
//!
//! The pad settings live in the game context: per port a 0x5C-byte record
//! at +0x15C (button masks by action from +0x16, analog data from +0x40),
//! with the current type at +0x124 and flags at +0x120.
use crate::{
    machine::Machine,
    ov2_briefing::{
        WORK_HIGH, add_prim, div_pow2, draw_text, external, lbu, lh, lhu, sb, sext16, sh, sound,
        work,
    },
    ov2_options::{
        CLEAR_TEXT, SCENE_ENTER, SCENE_LEAVE, SELECT_VISUAL_PAGE, SET_VISUAL_OFFSET,
        SET_VISUAL_PARAMETER, SET_VISUAL_STYLE, SHOW_TEXT, VISUAL_ADDRESS, call_pointer, context,
    },
};

/// 800F6668: pointer to the KEY CONFIG block.
pub const KEYS: u32 = 0x800f_6668;
/// 800F6664: pointer to the glyph table (12-byte records {u, v, w/4, h,
/// clut x, clut y}).
pub const GLYPHS: u32 = 0x800f_6664;
/// 800F6500: action → bit index translation (31 bytes, 0x1F = none).
pub const ACTION_BITS: u32 = 0x800f_6500;
/// 800F6518: the 15 button masks (halfwords).
pub const BUTTON_MASKS: u32 = 0x800f_6518;
/// 800F64E8: action codes by context slot (from slot 0xB).
pub const SLOT_ACTIONS: u32 = 0x800f_64e8;
/// 800F6538: the four analog axis codes (halfwords).
pub const AXIS_CODES: u32 = 0x800f_6538;
/// SDK SetDrawMode (800AAC38).
const SET_DRAW_MODE: u32 = 0x800a_ac38;

pub(crate) fn keys(m: &mut Machine) -> u32 {
    m.lw(KEYS)
}
/// Byte, halfword (signed) and word `+offset` of the KEY CONFIG block.
fn kbyte(m: &mut Machine, offset: u32) -> u32 {
    let block = keys(m);
    lbu(m, block + offset)
}
fn khalf(m: &mut Machine, offset: u32) -> u32 {
    let block = keys(m);
    lh(m, block + offset)
}
fn kword(m: &mut Machine, offset: u32) -> u32 {
    let block = keys(m);
    m.lw(block + offset)
}
fn set_kbyte(m: &mut Machine, offset: u32, value: u32) {
    let block = keys(m);
    sb(m, block + offset, value);
}
/// Word `+offset` of the game context.
fn cword(m: &mut Machine, offset: u32) -> u32 {
    let ctx = context(m);
    m.lw(ctx + offset)
}
/// Word `+0x20000 + offset` of the overlay work block.
fn work_word(m: &mut Machine, offset: u32) -> u32 {
    let high = work(m).wrapping_add(WORK_HIGH);
    m.lw(high + offset)
}
/// The work block's ordering-table slot pointer `[+0x90]`.
fn ot_slot(m: &mut Machine) -> u32 {
    let block = work(m);
    m.lw(block + 0x90)
}
/// The context's pad record of `port` (`port * 0x5C`, as compiled).
fn record_offset(port: u32) -> u32 {
    let three = (port << 1).wrapping_add(port);
    ((three << 3).wrapping_sub(port)) << 2
}

/// 800DE4F8: a button glyph: a textured sprite (SPRT, code 0x64, 0x66 when
/// `blend`) at the layout position of item `item` of the current type,
/// glyph `glyph`, and a DR_MODE for its texture page, both linked at the
/// visual block's slot. Returns the next free primitive.
pub fn glyph(m: &mut Machine, prim: u32, item: u32, glyph: u32, blend: u32) -> u32 {
    let frame = m.push_frame(0x38);
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    let mode = blend << 1;
    let layout = m.lw(block.wrapping_add(kind << 2) + 0x48);
    let entry = layout.wrapping_add(((item << 2).wrapping_add(item)) << 2);
    let table = m.lw(GLYPHS);
    let glyph = table.wrapping_add(((glyph << 1).wrapping_add(glyph)) << 2);
    let visual = external(m, VISUAL_ADDRESS, [0; 4]);
    let sprite = prim;
    let mut cursor = prim + 0x14;
    sb(m, sprite + 3, 4);
    let slot = visual.wrapping_add(8);
    sb(m, sprite + 7, 0x64);
    sb(m, sprite + 7, if blend != 0 { 0x66 } else { 0x64 });
    let x = lhu(m, entry + 0x10);
    sh(m, sprite + 8, x);
    let y = lhu(m, entry + 0x12);
    sh(m, sprite + 0xa, y);
    let w = lh(m, glyph + 4) << 2;
    sh(m, sprite + 0x10, w);
    let h = lhu(m, glyph + 6);
    sh(m, sprite + 0x12, h);
    let u = lh(m, glyph);
    let u = u.wrapping_sub(div_pow2(u, 6) << 6);
    sb(m, sprite + 0xc, ((u << 16) as i32 >> 14) as u32);
    let v = lh(m, glyph + 2);
    sb(m, sprite + 0xd, v.wrapping_sub(div_pow2(v, 8) << 8));
    let mode_prim = cursor;
    cursor += 0xc;
    sb(m, sprite + 4, 0x80);
    sb(m, sprite + 5, 0x80);
    sb(m, sprite + 6, 0x80);
    let clut_y = lhu(m, glyph + 0xa) << 6;
    let clut_x = (lhu(m, glyph + 8) >> 4) & 0x3f;
    sh(m, sprite + 0xe, clut_y | clut_x);
    add_prim(m, slot, sprite);
    let abr = (mode & 3) << 5;
    let page_y = lhu(m, glyph + 2);
    let page_x = lhu(m, glyph);
    m.sw(frame + 0x10, 0);
    let page = abr
        | (((page_y & 0x100) << 16) as i32 >> 20) as u32
        | ((page_x & 0x3ff) >> 6)
        | ((page_y & 0x200) << 2);
    external(m, SET_DRAW_MODE, [mode_prim, 0, 0, page]);
    add_prim(m, slot, mode_prim);
    m.pop_frame(0x38);
    cursor
}

/// The button assignment bytes, analog actions and settings of a preset,
/// in the original store order: (offset from the block's +0x14, value).
fn store_bytes(m: &mut Machine, base: u32, bytes: &[(u32, u32)]) {
    for &(offset, value) in bytes {
        sb(m, base + offset, value);
    }
}

/// Mark the settings changed (+0x10 |= 6).
fn mark_changed(m: &mut Machine) {
    let block = keys(m);
    let bits = m.lw(block + 0x10);
    m.sw(block + 0x10, bits | 6);
}

/// 800DE73C: preset A.
pub fn preset_a(m: &mut Machine) {
    let base = keys(m) + 0x14;
    store_bytes(
        m,
        base,
        &[
            (0, 4),
            (1, 7),
            (3, 0xb),
            (5, 9),
            (6, 0x14),
            (7, 5),
            (8, 6),
            (9, 0xe),
            (0xb, 0xc),
            (0xa, 0x10),
            (0xc, 0xd),
            (0xd, 0x12),
            (2, 8),
            (4, 0xa),
            (0xe, 0x13),
        ],
    );
    let base = keys(m) + 0x24;
    store_bytes(m, base, &[(0, 0), (1, 2), (2, 8), (3, 0xa)]);
    let base = keys(m) + 0x28;
    store_bytes(m, base, &[(0, 1), (1, 0), (2, 0), (3, 0)]);
    mark_changed(m);
}

/// 800DE818: preset B.
pub fn preset_b(m: &mut Machine) {
    let base = keys(m) + 0x14;
    store_bytes(
        m,
        base,
        &[
            (0, 0x14),
            (1, 0xe),
            (2, 1),
            (3, 5),
            (4, 4),
            (8, 0xd),
            (9, 0x10),
            (0xa, 7),
            (5, 0),
            (6, 0x14),
            (7, 0x14),
            (0xb, 0xc),
            (0xc, 6),
            (0xd, 0x14),
            (0xe, 0x14),
        ],
    );
    let base = keys(m) + 0x24;
    store_bytes(m, base, &[(0, 0xc), (1, 0xc), (2, 0xc), (3, 2)]);
    let base = keys(m) + 0x28;
    store_bytes(m, base, &[(0, 0), (1, 0), (2, 0), (3, 0)]);
    mark_changed(m);
}

/// 800DE8E0: preset C.
pub fn preset_c(m: &mut Machine) {
    let base = keys(m) + 0x14;
    store_bytes(
        m,
        base,
        &[
            (0, 4),
            (1, 7),
            (2, 1),
            (3, 3),
            (4, 2),
            (7, 5),
            (8, 6),
            (9, 0xe),
            (0xb, 0xc),
            (0xa, 0x10),
            (0xc, 0xd),
            (5, 0),
            (6, 0x14),
            (0xd, 0x14),
            (0xe, 0x13),
        ],
    );
    mark_changed(m);
}

/// 800DE978: preset D.
pub fn preset_d(m: &mut Machine) {
    let base = keys(m) + 0x14;
    store_bytes(
        m,
        base,
        &[
            (0, 0x14),
            (2, 8),
            (3, 0xb),
            (4, 0xa),
            (5, 9),
            (9, 0x10),
            (0xb, 0xe),
            (0xa, 0xd),
            (0xc, 0xc),
            (1, 0x14),
            (6, 0x14),
            (7, 0x14),
            (8, 0x14),
            (0xd, 0x14),
            (0xe, 0x13),
        ],
    );
    let base = keys(m) + 0x24;
    store_bytes(m, base, &[(0, 6), (1, 4), (2, 0), (3, 2)]);
    let base = keys(m) + 0x28;
    store_bytes(m, base, &[(0, 0), (1, 0), (2, 1), (3, 0)]);
    mark_changed(m);
}

/// 800DEA48: the index of the lowest of bits 0..5 set in `bits`, else -1.
pub fn type_index(bits: u32) -> u32 {
    (0..6)
        .find(|&k| bits & (1 << k) != 0)
        .unwrap_or(0xffff_ffff)
}

/// 800DEAB0: load the screen's settings from the context's record of the
/// current controller type (+0x124): the button assignment (actions by
/// mask, 0x14 = none), the analog actions and inversions, the neutral zone
/// (+0x10 bits), analog mode (+0x120 bit 2), the response (from +3 of the
/// analog data, /19 for type 4, else /16) and the two analog options
/// (+0x1B4 bits 3/4).
pub fn load_settings(m: &mut Machine) {
    let ctx = context(m);
    let port = type_index(m.lw(ctx + 0x124));
    let block = keys(m);
    let buttons = block + 0x14;
    for k in (0..0xf).rev() {
        sb(m, block + 0x14 + k, 0x14);
    }
    let offset = record_offset(port);
    for slot in 0xb..0x1f {
        let action = SLOT_ACTIONS.wrapping_add(slot).wrapping_sub(0xb);
        let field = (slot << 1).wrapping_add(offset);
        for k in 0..0xf {
            let ctx = context(m);
            let mask = lh(m, BUTTON_MASKS + k * 2);
            let bits = lhu(m, ctx.wrapping_add(field) + 0x15c);
            if mask & bits != 0 {
                let code = lbu(m, action);
                sb(m, buttons + k, code);
            }
        }
    }
    let ctx = context(m);
    let analog = ctx.wrapping_add(offset) + 0x19c;
    let block = keys(m);
    let axes = block + 0x24;
    let inverted = block + 0x28;
    for k in (0..4).rev() {
        sb(m, block + 0x24 + k, 0xc);
    }
    for action in 0..6u32 {
        let code = action << 1;
        for k in 0..4u32 {
            let value = lh(m, AXIS_CODES + k * 2);
            let current = lbu(m, analog.wrapping_add(action) + 0x12);
            if value != current {
                continue;
            }
            sb(m, axes + k, code);
            let flag = match code {
                0 => Some(lhu(m, analog + 0x10) & 4 == 0),
                6 => Some(lhu(m, analog + 0x10) & 8 == 0),
                8 => Some(lhu(m, analog + 0x10) & 0x10 != 0),
                _ => None,
            };
            if let Some(set) = flag {
                sb(m, inverted + k, u32::from(set));
            }
        }
    }
    let bits = lhu(m, analog + 0x10);
    let zone = if bits & 2 != 0 {
        0
    } else if bits & 1 != 0 {
        2
    } else {
        1
    };
    let block = keys(m);
    sb(m, block + 0x2d, zone);
    let ctx = context(m);
    if m.lw(ctx + 0x120) & 4 != 0 {
        let block = keys(m);
        let flags = m.lw(block + 0x10);
        m.sw(block + 0x10, flags | 1);
    }
    if (port as i32) < 0 {
        return;
    }
    let ctx = context(m);
    let analog = ctx.wrapping_add(record_offset(port)) + 0x19c;
    let response = if m.lw(ctx + 0x124) == 4 {
        let half = lbu(m, analog + 3) >> 1;
        let high = ((u64::from(half) * 0x6bca_1af3) >> 32) as u32;
        high >> 3
    } else {
        lbu(m, analog + 3) >> 4
    };
    let block = keys(m);
    sb(m, block + 0x2c, response.wrapping_sub(1));
    let ctx = context(m);
    let options = m.lw(ctx.wrapping_add(record_offset(port)) + 0x1b4);
    if options & 8 != 0 {
        let block = keys(m);
        let flags = m.lw(block + 0x10);
        m.sw(block + 0x10, flags | 2);
    }
    if options & 0x10 != 0 {
        let block = keys(m);
        let flags = m.lw(block + 0x10);
        m.sw(block + 0x10, flags | 4);
    }
}

/// 800DEE34: store the screen's settings into the context's record of
/// `port`: button masks by action, the analog axes and their inversion
/// bits, the neutral zone, analog mode, the two analog options and the
/// response thresholds; type 4 also copies three bytes to +0x310, the
/// other types the four axis entries to +0x19C.
pub fn store_settings(m: &mut Machine, port: u32) {
    let offset = record_offset(port);
    let block = keys(m);
    let ctx = context(m);
    let buttons = block + 0x14;
    let mut field = offset + 0x16;
    for _ in 0xb..0x20 {
        sh(m, ctx.wrapping_add(field) + 0x15c, 0);
        field = field.wrapping_add(2);
    }
    for k in 0..0xf {
        let action = lbu(m, buttons + k);
        let bit = lbu(m, ACTION_BITS.wrapping_add(action));
        if bit != 0x1f {
            let mask = lhu(m, BUTTON_MASKS + k * 2);
            sh(
                m,
                ctx.wrapping_add((bit << 1).wrapping_add(offset)) + 0x15c,
                mask,
            );
        }
    }
    let ctx = context(m);
    let record = ctx.wrapping_add(offset) + 0x15c;
    let analog = record + 0x40;
    let block = keys(m);
    let axes = block + 0x24;
    let mut inverted = block + 0x28;
    for k in (0..6).rev() {
        sb(m, record + 0x45 + k - 5 + 0x12, 0xff);
    }
    for k in 0..4u32 {
        let entry = axes + k;
        let code = lbu(m, entry);
        let action = code >> 1;
        if action < 6 {
            let value = lbu(m, AXIS_CODES + k * 2);
            sb(m, analog.wrapping_add(action) + 0x12, value);
            let bits = lhu(m, analog + 0x10);
            let updated = match lbu(m, entry) {
                0 => Some(if lbu(m, inverted) == 0 {
                    bits | 4
                } else {
                    bits & 0xfffb
                }),
                6 => Some(if lbu(m, inverted) == 0 {
                    bits | 8
                } else {
                    bits & 0xfff7
                }),
                8 => Some(if lbu(m, inverted) == 1 {
                    bits | 0x10
                } else {
                    bits & 0xffef
                }),
                _ => None,
            };
            if let Some(bits) = updated {
                sh(m, analog + 0x10, bits);
            }
        }
        inverted = inverted.wrapping_add(1);
    }
    let block = keys(m);
    let bits = lhu(m, analog + 0x10) & 0xfffc;
    sh(m, analog + 0x10, bits);
    match lbu(m, block + 0x2d) {
        0 => sh(m, analog + 0x10, bits | 2),
        2 => sh(m, analog + 0x10, bits | 1),
        _ => {}
    }
    let ctx = context(m);
    let flags = m.lw(ctx + 0x120) & !4;
    m.sw(ctx + 0x120, flags);
    let block = keys(m);
    if m.lw(block + 0x10) & 1 != 0 {
        m.sw(ctx + 0x120, flags | 4);
    }
    if (port as i32) >= 0 {
        let ctx = context(m);
        let record = ctx.wrapping_add(offset) + 0x15c;
        let options = record + 0x58;
        let block = keys(m);
        let word = m.lw(record + 0x58);
        let word = if m.lw(block + 0x10) & 2 != 0 {
            word | 8
        } else {
            word & !8
        };
        m.sw(record + 0x58, word);
        let block = keys(m);
        let word = m.lw(options);
        let word = if m.lw(block + 0x10) & 4 != 0 {
            word | 0x10
        } else {
            word & !0x10
        };
        m.sw(options, word);
        let ctx = context(m);
        let record = ctx.wrapping_add(offset) + 0x15c;
        let block = keys(m);
        let value = match lbu(m, block + 0x2c) {
            0 => Some(if port != 2 { 0x10 } else { 0x26 }),
            1 => Some(if port != 2 { 0x20 } else { 0x4c }),
            2 => Some(if port != 2 { 0x30 } else { 0x72 }),
            _ => None,
        };
        if let Some(value) = value {
            for k in (0..4u32).rev() {
                sb(m, record + 0x43 + k * 4, value);
            }
        }
    }
    if port == 2 || port == 0 {
        return;
    }
    if port == 4 {
        let block = keys(m);
        let ctx = context(m);
        let target = ctx + 0x30c;
        let a = lbu(m, block + 0x5d);
        sb(m, target + 4, a);
        let ctx = context(m);
        let b = lbu(m, ctx + 0x64);
        sb(m, target + 5, b);
        let block = keys(m);
        let c = lbu(m, block + 0x59);
        sb(m, target + 6, c);
        return;
    }
    let ctx = context(m);
    let mut entry = ctx.wrapping_add(offset) + 0x19c;
    for k in 0..4u32 {
        let block = keys(m);
        let a = lbu(m, block.wrapping_add(k) + 0x5c);
        sb(m, entry, a);
        let ctx = context(m);
        let b = lbu(m, ctx.wrapping_add(k << 1) + 0x62);
        sb(m, entry + 1, b);
        let block = keys(m);
        let c = lbu(m, block.wrapping_add(k) + 0x58);
        sb(m, entry + 2, c);
        entry = entry.wrapping_add(4);
    }
}

/// 800DF39C: store the settings for the screen's controller type (type 0:
/// ports 1 and 2; 1: port 4; 2: port 0; 3: port 3).
pub fn store_for_type(m: &mut Machine) {
    m.push_frame(0x18);
    match kbyte(m, 0x30) {
        0 => {
            store_settings(m, 1);
            store_settings(m, 2);
        }
        1 => store_settings(m, 4),
        2 => store_settings(m, 0),
        3 => store_settings(m, 3),
        _ => {}
    }
    m.pop_frame(0x18);
}

/// 800DF424: the MAIN tab's row styles and help text by the selected row
/// (+0x2E) and the flags (+0xC bit 2 analog, 0x10/0x20 the options).
pub fn main_rows(m: &mut Machine) {
    m.push_frame(0x18);
    let block = keys(m);
    if lh(m, block + 0x2e) != 0 {
        external(m, CLEAR_TEXT, [0; 4]);
    }
    let block = keys(m);
    if lh(m, block + 0x2e) == 1 {
        style(m, 7, 1);
        external(m, SHOW_TEXT, [0x38, 1, 0, 0]);
    } else {
        style(m, 7, 2);
    }
    let block = keys(m);
    if m.lw(block + 0xc) & 4 != 0 {
        if lh(m, block + 0x2e) == 2 {
            style(m, 1, 1);
            let block = keys(m);
            let text = if m.lw(block + 0xc) & 0x20 != 0 {
                style(m, 2, 2);
                style(m, 3, 4);
                0x3a
            } else {
                style(m, 2, 4);
                style(m, 3, 2);
                0x39
            };
            external(m, SHOW_TEXT, [text, 1, 0, 0]);
        } else {
            style(m, 1, 2);
            let block = keys(m);
            if m.lw(block + 0xc) & 0x20 != 0 {
                style(m, 2, 2);
                style(m, 3, 1);
            } else {
                style(m, 2, 1);
                style(m, 3, 2);
            }
        }
    } else {
        style(m, 1, 3);
        style(m, 2, 3);
        style(m, 3, 3);
    }
    let block = keys(m);
    if m.lw(block + 0xc) & 0x10 != 0 {
        style(m, 5, 2);
        let block = keys(m);
        if lh(m, block + 0x2e) == 3 {
            style(m, 6, 4);
            external(m, SHOW_TEXT, [0x3b, 1, 0, 0]);
        } else {
            style(m, 6, 1);
        }
    } else if lh(m, block + 0x2e) == 3 {
        style(m, 5, 4);
        external(m, SHOW_TEXT, [0x36, 1, 0, 0]);
        style(m, 6, 2);
    } else {
        style(m, 5, 1);
        style(m, 6, 2);
    }
    m.pop_frame(0x18);
}

fn style(m: &mut Machine, slot: u32, value: u32) {
    external(m, SET_VISUAL_STYLE, [slot, value, 0, 0]);
}

// ---- the analog page (sub-modes of 800E2390, table 0x800F669C) ----------------------

/// 800F6540: the number of layout items by controller type.
pub const ITEM_COUNTS: u32 = 0x800f_6540;

/// The help line of a choice: `0x3E + choice` for choices 0..5, else 0x7C.
fn choice_text(m: &mut Machine) {
    let block = keys(m);
    let text = if lbu(m, block + 0x3a) < 6 {
        lbu(m, block + 0x3a).wrapping_add(0x3e)
    } else {
        0x7c
    };
    external(m, SHOW_TEXT, [text, 1, 0, 0]);
}

/// The item entry (+0 button index) of layout item `offset / 0x14` of the
/// current type.
fn layout_button(m: &mut Machine, offset: u32) -> u32 {
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    let layout = m.lw(block.wrapping_add(kind << 2) + 0x48);
    m.lw(offset.wrapping_add(layout))
}

/// 800DF650: the analog axis row (+0x38 the axis, +0x3A its action): the
/// action's style and text, the axis highlight (+0xC bits 0x80/0x40 by
/// axis pair), then the input: TRIANGLE leaves (0x40, sound 0x3B), START
/// toggles (1, sound 0x3D, +0xC &= ~0x10), CROSS selects (4, sound 0x3C),
/// DOWN/UP move between the axes (2 below the last, 1 above the first;
/// sound 0x3D). Type 1 has a single axis; type 2 none (0).
pub fn axis_row(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = axis_row_body(m);
    m.pop_frame(0x18);
    result
}

fn axis_row_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    let flags = m.lw(block + 0xc);
    let axis = lbu(m, block + 0x38);
    m.sw(block + 0xc, flags & !0xc0);
    let action = lbu(m, block.wrapping_add(axis) + 0x24) >> 1;
    sb(m, block + 0x3a, action);
    let action = kbyte(m, 0x3a);
    style(m, action.wrapping_add(0x1f), 4);
    external(m, CLEAR_TEXT, [0; 4]);
    choice_text(m);
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    match kind {
        1 => {
            let flags = m.lw(block + 0xc) | 0x80;
            let ctx = context(m);
            m.sw(block + 0xc, flags);
            let buttons = m.lw(ctx + 0x12c);
            if buttons & 0x20 != 0 {
                m.sw(block + 0xc, flags & !0xc0);
                sound(m, 0x3b);
                0x40
            } else if buttons & 0x40 != 0 {
                sound(m, 0x3d);
                let block = keys(m);
                let flags = m.lw(block + 0xc) & !0x10;
                m.sw(block + 0xc, flags);
                1
            } else if buttons & 0x10 != 0 {
                sound(m, 0x3c);
                4
            } else {
                let held = m.lw(ctx + 0x130);
                if held & 2 != 0 {
                    sound(m, 0x3d);
                    2
                } else if held & 1 != 0 {
                    sound(m, 0x3d);
                    1
                } else {
                    0
                }
            }
        }
        0 | 3 => {
            let axis = lbu(m, block + 0x38);
            if axis < 2 {
                let flags = m.lw(block + 0xc) | 0x80;
                m.sw(block + 0xc, flags);
            } else if (axis.wrapping_sub(2) & 0xff) < 2 {
                let flags = m.lw(block + 0xc) | 0x40;
                m.sw(block + 0xc, flags);
            }
            let ctx = context(m);
            let buttons = m.lw(ctx + 0x12c);
            if buttons & 0x20 != 0 {
                let block = keys(m);
                let flags = m.lw(block + 0xc) & !0xc0;
                m.sw(block + 0xc, flags);
                sound(m, 0x3b);
                0x40
            } else if buttons & 0x40 != 0 {
                sound(m, 0x3d);
                let block = keys(m);
                let flags = m.lw(block + 0xc) & !0x10;
                m.sw(block + 0xc, flags);
                1
            } else if buttons & 0x10 != 0 {
                sound(m, 0x3c);
                4
            } else {
                let held = m.lw(ctx + 0x130);
                if held & 2 != 0 {
                    sound(m, 0x3d);
                    let block = keys(m);
                    let axis = lbu(m, block + 0x38);
                    if axis < 3 {
                        sb(m, block + 0x38, axis + 1);
                        0
                    } else {
                        2
                    }
                } else if held & 1 != 0 {
                    sound(m, 0x3d);
                    let block = keys(m);
                    let axis = lbu(m, block + 0x38);
                    if axis == 0 {
                        1
                    } else {
                        sb(m, block + 0x38, axis - 1);
                        0
                    }
                } else {
                    0
                }
            }
        }
        _ => 0,
    }
}

/// 800DF93C: open the action list for the selected axis: +0x3A = its
/// action; the list animation timer +0x6C (10 when re-entered with +8 bit
/// 0, else 0 with the four list slots' parameters cleared). Returns 0x10.
pub fn action_list_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    let axis = lbu(m, block + 0x38);
    let action = lbu(m, block.wrapping_add(axis) + 0x24) >> 1;
    sb(m, block + 0x3a, action);
    let block = keys(m);
    if m.lw(block + 8) & 1 != 0 {
        m.sw(block + 0x6c, 0xa);
    } else {
        m.sw(block + 0x6c, 0);
        for slot in 1..5 {
            external(m, SET_VISUAL_PARAMETER, [slot, 0, 0, 0]);
        }
    }
    m.pop_frame(0x18);
    0x10
}

/// The list slots of the highlighted axis pair (+0xC bit 0x40: slots 1/3,
/// 0x80: slots 2/4) take parameter `value` (the second one re-read from
/// +0x6C when `reread`).
fn list_slots(m: &mut Machine, flags: u32, value: u32, reread: bool) {
    let pair = if flags & 0x40 != 0 {
        Some((1, 3))
    } else if flags & 0x80 != 0 {
        Some((2, 4))
    } else {
        None
    };
    if let Some((first, second)) = pair {
        external(m, SET_VISUAL_PARAMETER, [first, value, 0, 0]);
        let value = if reread { kword(m, 0x6c) } else { value };
        external(m, SET_VISUAL_PARAMETER, [second, value, 0, 0]);
    }
}

/// 800DF9D0: grow the action list (timer +0x6C up to 10). Returns 0 while
/// growing, 0x10 when open.
pub fn action_list_grow(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    let timer = m.lw(block + 0x6c);
    let result = if (timer as i32) < 0xa {
        let timer = timer.wrapping_add(1);
        let flags = m.lw(block + 0xc);
        m.sw(block + 0x6c, timer);
        list_slots(m, flags, timer, true);
        0
    } else {
        let flags = m.lw(block + 0xc);
        m.sw(block + 0x6c, 0xa);
        list_slots(m, flags, 0xa, false);
        0x10
    };
    m.pop_frame(0x18);
    result
}

/// 800DFABC: the action list: styles of the seven actions (assigned ones
/// highlighted), the choice +0x3A (style 4) and its text; TRIANGLE closes
/// (0x10, sound 0x3B, +8 &= ~1); START does nothing; DOWN/UP cycle the
/// choice 0..6 (sound 0x3D); CROSS (sound 0x3C) assigns it to the axis:
/// choices 0, 3 and 4 swap with the axis's previous action (+8 |= 1, the
/// old action saved at +0x3C), the others clear it from the other axes
/// (0xC) and from the buttons (0x14). Returns 0x10 when closing, else 0.
pub fn action_list(m: &mut Machine) -> u32 {
    m.push_frame(0x30);
    let result = action_list_body(m);
    m.pop_frame(0x30);
    result
}

fn action_list_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    let axes = block + 0x24;
    let buttons = block + 0x14;
    for k in 0..7 {
        style(m, k + 0x1f, 2);
    }
    for k in 0..4 {
        let block = keys(m);
        let action = lbu(m, block.wrapping_add(k) + 0x24);
        if (action as i32) < 0xc {
            style(m, (action >> 1) + 0x1f, 1);
        }
    }
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    if lbu(m, ITEM_COUNTS.wrapping_add(kind)) != 0 {
        let mut item = 0u32;
        let mut offset = 0u32;
        loop {
            let button = layout_button(m, offset);
            let block = keys(m);
            let action = lbu(m, block.wrapping_add(button) + 0x14);
            if (action as i32) < 8 {
                style(m, (action >> 1) + 0x1f, 1);
            }
            let block = keys(m);
            let kind = lbu(m, block + 0x30);
            let count = lbu(m, ITEM_COUNTS.wrapping_add(kind));
            item = item.wrapping_add(1);
            offset = offset.wrapping_add(0x14);
            if (item as i32) >= (count as i32) {
                break;
            }
        }
    }
    let choice = kbyte(m, 0x3a);
    style(m, choice.wrapping_add(0x1f), 4);
    external(m, CLEAR_TEXT, [0; 4]);
    choice_text(m);
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    if pressed & 0x20 != 0 {
        let block = keys(m);
        let flags = m.lw(block + 8) & !1;
        m.sw(block + 8, flags);
        sound(m, 0x3b);
        return 0x10;
    }
    if pressed & 0x40 != 0 {
        return 0;
    }
    let held = m.lw(ctx + 0x130);
    if held & 2 != 0 {
        sound(m, 0x3d);
        let block = keys(m);
        let choice = lbu(m, block + 0x3a);
        sb(m, block + 0x3a, if choice < 6 { choice + 1 } else { 0 });
        return 0;
    }
    if held & 1 != 0 {
        sound(m, 0x3d);
        let block = keys(m);
        let choice = lbu(m, block + 0x3a);
        sb(m, block + 0x3a, if choice != 0 { choice - 1 } else { 6 });
        return 0;
    }
    if pressed & 0x10 == 0 {
        return 0;
    }
    sound(m, 0x3c);
    let block = keys(m);
    let choice = lbu(m, block + 0x3a);
    if choice == 0 || choice == 3 || choice == 4 {
        let block = keys(m);
        let flags = m.lw(block + 8) | 1;
        let axis = lbu(m, block + 0x38);
        m.sw(block + 8, flags);
        let old = lbu(m, axes.wrapping_add(axis));
        sb(m, block + 0x3c, old);
    } else {
        for k in 0..4 {
            let block = keys(m);
            let entry = axes + k;
            let choice = lbu(m, block + 0x3a);
            if lbu(m, entry) == choice << 1 {
                sb(m, entry, 0xc);
            }
        }
        let block = keys(m);
        let kind = lbu(m, block + 0x30);
        if lbu(m, ITEM_COUNTS.wrapping_add(kind)) != 0 {
            let mut item = 0u32;
            let mut offset = 0u32;
            loop {
                let block = keys(m);
                let code = lbu(m, block + 0x3a) << 1;
                if (code as i32) < 8 {
                    let button = layout_button(m, offset);
                    let entry = buttons.wrapping_add(button);
                    if lbu(m, entry) == code {
                        sb(m, entry, 0x14);
                    }
                    let button = layout_button(m, offset);
                    let block = keys(m);
                    let code = (lbu(m, block + 0x3a) << 1).wrapping_add(1);
                    let entry = buttons.wrapping_add(button);
                    if lbu(m, entry) == code {
                        sb(m, entry, 0x14);
                    }
                }
                let block = keys(m);
                let kind = lbu(m, block + 0x30);
                let count = lbu(m, ITEM_COUNTS.wrapping_add(kind));
                item = item.wrapping_add(1);
                offset = offset.wrapping_add(0x14);
                if (item as i32) >= (count as i32) {
                    break;
                }
            }
        }
        let block = keys(m);
        let flags = m.lw(block + 8) & !1;
        m.sw(block + 8, flags);
    }
    let block = keys(m);
    let axis = lbu(m, block + 0x38);
    let choice = lbu(m, block + 0x3a);
    sb(m, axes.wrapping_add(axis), choice << 1);
    0x10
}

/// 800DFF58: shrink the action list (timer +0x6C down to 0), unless a swap
/// is pending (+8 bit 0: 4). Returns 0 while shrinking, 8 when closed.
pub fn action_list_shrink(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    let result = if m.lw(block + 8) & 1 != 0 {
        4
    } else {
        let timer = m.lw(block + 0x6c);
        if (timer as i32) > 0 {
            let timer = timer.wrapping_sub(1);
            let flags = m.lw(block + 0xc);
            m.sw(block + 0x6c, timer);
            list_slots(m, flags, timer, true);
            0
        } else {
            let flags = m.lw(block + 0xc);
            m.sw(block + 0x6c, 0);
            list_slots(m, flags, 0, false);
            8
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800E0054: the help line of the inversion choice (+0x40) of axis +0x38
/// with action +0x3A: `6 * action + 3 * inversion + 0x46` for type bit 4
/// of +0x124, else `6 * action + (axis & 1) + 0x44 + 3 * inversion`.
pub fn inversion_text(m: &mut Machine) {
    m.push_frame(0x20);
    let block = keys(m);
    let axis = lbu(m, block + 0x38);
    let action = lbu(m, block + 0x3a);
    let inverted = lbu(m, block.wrapping_add(axis) + 0x28);
    external(m, CLEAR_TEXT, [0; 4]);
    let ctx = context(m);
    let six = ((action << 1).wrapping_add(action)) << 1;
    let three = (inverted << 1).wrapping_add(inverted);
    let text = if m.lw(ctx + 0x124) & 0x10 != 0 {
        six.wrapping_add(three.wrapping_add(0x46))
    } else {
        six.wrapping_add(axis.wrapping_sub(axis & 0xfe).wrapping_add(0x44))
            .wrapping_add(three)
    };
    external(m, SHOW_TEXT, [text, 1, 0, 0]);
    m.pop_frame(0x20);
}

/// 800E0108: open the inversion choice: +0x40 = the axis's inversion.
/// Returns 0x10.
pub fn inversion_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    let axis = lbu(m, block + 0x38);
    let inverted = lbu(m, block.wrapping_add(axis) + 0x28);
    m.sw(block + 0x40, inverted);
    inversion_text(m);
    m.pop_frame(0x18);
    0x10
}

/// Clear `code`/`code + 1` (when `code` < 8) from the buttons of every
/// layout item (0x14 = none), as 800DFABC and 800E0140 compile it.
fn clear_buttons(m: &mut Machine, buttons: u32) {
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    if lbu(m, ITEM_COUNTS.wrapping_add(kind)) == 0 {
        return;
    }
    let mut item = 0u32;
    let mut offset = 0u32;
    loop {
        let block = keys(m);
        let code = lbu(m, block + 0x3a) << 1;
        if (code as i32) < 8 {
            let button = layout_button(m, offset);
            let entry = buttons.wrapping_add(button);
            if lbu(m, entry) == code {
                sb(m, entry, 0x14);
            }
            let button = layout_button(m, offset);
            let block = keys(m);
            let code = (lbu(m, block + 0x3a) << 1).wrapping_add(1);
            let entry = buttons.wrapping_add(button);
            if lbu(m, entry) == code {
                sb(m, entry, 0x14);
            }
        }
        let block = keys(m);
        let kind = lbu(m, block + 0x30);
        let count = lbu(m, ITEM_COUNTS.wrapping_add(kind));
        item = item.wrapping_add(1);
        offset = offset.wrapping_add(0x14);
        if (item as i32) >= (count as i32) {
            break;
        }
    }
}

/// 800E0140: the inversion choice of the axis (+0x28 + axis): TRIANGLE
/// restores the inversion (+0x40) and the action (+0x3C) (sound 0x3C,
/// returns 8); START does nothing; UP/DOWN on an even axis, LEFT/RIGHT on
/// an odd one toggle it (sound 0x3D); CROSS (sound 0x3C) commits: the
/// action leaves the other axes and the buttons, the axis takes it
/// (returns 0x10).
pub fn inversion_choice(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let result = inversion_choice_body(m);
    m.pop_frame(0x28);
    result
}

fn inversion_choice_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    let axis = lbu(m, block + 0x38);
    let buttons = block + 0x14;
    let entry = block.wrapping_add(axis.wrapping_add(0x24));
    let inverted = block.wrapping_add(axis.wrapping_add(0x28));
    inversion_text(m);
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    if pressed & 0x20 != 0 {
        let old = kbyte(m, 0x40);
        sb(m, inverted, old);
        let action = kbyte(m, 0x3c);
        sb(m, entry, action);
        sound(m, 0x3c);
        return 8;
    }
    if pressed & 0x40 != 0 {
        return 0;
    }
    if m.lw(ctx + 0x130) & 3 != 0 && kbyte(m, 0x38) & 1 == 0 {
        sound(m, 0x3d);
        let value = lbu(m, inverted);
        sb(m, inverted, 1u32.wrapping_sub(value));
        return 0;
    }
    let ctx = context(m);
    if m.lw(ctx + 0x130) & 0xc != 0 {
        let odd = kbyte(m, 0x38) & 1;
        if odd == 1 {
            sound(m, 0x3d);
            let value = lbu(m, inverted);
            sb(m, inverted, odd.wrapping_sub(value));
            return 0;
        }
    }
    let ctx = context(m);
    if m.lw(ctx + 0x12c) & 0x10 == 0 {
        return 0;
    }
    sound(m, 0x3c);
    let block = keys(m);
    let old = lbu(m, block + 0x3c);
    sb(m, entry, old);
    for k in 0..4 {
        let block = keys(m);
        let axis_entry = block.wrapping_add(k);
        let choice = lbu(m, block + 0x3a);
        if lbu(m, axis_entry + 0x24) == choice << 1 {
            sb(m, axis_entry + 0x24, 0xc);
        }
    }
    clear_buttons(m, buttons);
    let choice = kbyte(m, 0x3a);
    sb(m, entry, choice << 1);
    0x10
}

/// 800E0440: shrink the list after the inversion choice (timer +0x6C down
/// to 0). Returns 0 while shrinking, then 0x20 with +8 bit 0 cleared.
pub fn inversion_close(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    let timer = m.lw(block + 0x6c);
    let result = if (timer as i32) > 0 {
        let timer = timer.wrapping_sub(1);
        let flags = m.lw(block + 0xc);
        m.sw(block + 0x6c, timer);
        list_slots(m, flags, timer, true);
        0
    } else {
        let flags = m.lw(block + 0xc);
        m.sw(block + 0x6c, 0);
        list_slots(m, flags, 0, false);
        let block = keys(m);
        let flags = m.lw(block + 8) & !1;
        m.sw(block + 8, flags);
        0x20
    };
    m.pop_frame(0x18);
    result
}

/// 800E0538: the help line `0x68 + item`, with items 0xE/0xF shown as
/// 0x15/0x16 in analog mode (+0x10 bit 2) and item 0x13 as 0x17 with
/// option bit 1.
pub fn item_text(m: &mut Machine, item: u32) {
    m.push_frame(0x18);
    external(m, CLEAR_TEXT, [0; 4]);
    let mut item = item;
    if kword(m, 0x10) & 4 != 0 {
        if item == 0xe {
            item = 0x15;
        } else if item == 0xf {
            item = 0x16;
        }
    }
    if kword(m, 0x10) & 2 != 0 && item == 0x13 {
        item = 0x17;
    }
    external(m, SHOW_TEXT, [item.wrapping_add(0x68), 1, 0, 0]);
    m.pop_frame(0x18);
}

/// One tab-arrow bounce of 800E05D0: the halfword at `+field` moves by one
/// towards +-10 while flag `up` (from -10 upwards) or `down` (from +10
/// downwards) is set, shown on slot `near` (positive side) or `far`
/// (magnitude of the negative side); at the end the flag clears.
fn bounce(m: &mut Machine, field: u32, up: u32, down: u32, near: u32, far: u32) {
    let block = keys(m);
    let flags = m.lw(block + 0xc);
    let value = lh(m, block + field);
    let raw = lhu(m, block + field);
    let step = if flags & up != 0 {
        if (value as i32) < 0 {
            Some((raw.wrapping_add(1), far))
        } else if (value as i32) < 10 {
            Some((raw.wrapping_add(1), near))
        } else {
            m.sw(block + 0xc, flags & !up);
            None
        }
    } else if flags & down != 0 {
        if (value as i32) > 0 {
            Some((raw.wrapping_sub(1), near))
        } else if (value as i32) < -9 {
            m.sw(block + 0xc, flags & !down);
            None
        } else {
            Some((raw.wrapping_sub(1), far))
        }
    } else {
        None
    };
    if let Some((value, slot)) = step {
        sh(m, block + field, value);
        let value = sext16(value);
        let value = if slot == far {
            if (value as i32) < 0 {
                0u32.wrapping_sub(value)
            } else {
                value
            }
        } else {
            value
        };
        external(m, SET_VISUAL_PARAMETER, [slot, value, 0, 0]);
    }
}

/// 800E05D0: start a tab-arrow bounce for the pressed bits (0x100/0x200
/// the left arrow at +0x70 from -10/+10, 0x400/0x800 the right one at
/// +0x72) and advance both bounces (slots 7/8 and 9/10).
pub fn tab_arrows(m: &mut Machine, bits: u32) {
    m.push_frame(0x18);
    let block = keys(m);
    let start = if bits & 0x100 != 0 {
        Some((0x70, 0xfff6, !0x200, 0x100))
    } else if bits & 0x200 != 0 {
        Some((0x70, 0xa, !0x100, 0x200))
    } else if bits & 0x400 != 0 {
        Some((0x72, 0xfff6, !0x800, 0x400))
    } else if bits & 0x800 != 0 {
        Some((0x72, 0xa, !0x400, 0x800))
    } else {
        None
    };
    if let Some((field, value, keep, set)) = start {
        sh(m, block + field, value);
        let flags = (m.lw(block + 0xc) & keep) | set;
        m.sw(block + 0xc, flags);
    }
    bounce(m, 0x70, 0x100, 0x200, 7, 8);
    bounce(m, 0x72, 0x400, 0x800, 9, 0xa);
    m.pop_frame(0x18);
}

// ---- the button page (sub-modes of 800E2AB4, table 0x800F66CC) ----------------------

/// 800E0810: the button grid: the selected button +0x39 moves to the
/// layout's neighbor for the pressed direction (UP/DOWN/LEFT/RIGHT: the
/// entry's +4/+6/+8/+0xA, sound 0x3D; 0x16/0x17 leave the grid: 1/2;
/// bits 0x100..0x800 bounce the tab arrows); TRIANGLE leaves (0x40, sound
/// 0x3B), START toggles (1, sound 0x3D), CROSS selects (4, sound 0x3C).
/// Otherwise shows the button's help and clears slots 0x5B/0x5C; returns 0.
pub fn button_grid(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = button_grid_body(m);
    m.pop_frame(0x18);
    result
}

fn button_grid_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    let mut entry = m.lw(block.wrapping_add(kind << 2) + 0x48);
    let count = lbu(m, ITEM_COUNTS.wrapping_add(kind));
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    if count != 0 {
        let selected = lbu(m, block + 0x39);
        let mut item = entry;
        let mut k = 0u32;
        loop {
            if m.lw(item) == selected {
                entry = item;
            }
            k = k.wrapping_add(1);
            item = item.wrapping_add(0x14);
            if (k as i32) >= (count as i32) {
                break;
            }
        }
    }
    if pressed & 0x20 != 0 {
        sound(m, 0x3b);
        return 0x40;
    }
    if pressed & 0x40 != 0 {
        sound(m, 0x3d);
        let block = keys(m);
        let flags = m.lw(block + 0xc) & !0x10;
        m.sw(block + 0xc, flags);
        return 1;
    }
    if pressed & 0x10 != 0 {
        sound(m, 0x3c);
        return 4;
    }
    let mut next = 0x15;
    let neighbor = if pressed & 1 != 0 {
        Some(4)
    } else if pressed & 2 != 0 {
        Some(6)
    } else if pressed & 4 != 0 {
        Some(8)
    } else if pressed & 8 != 0 {
        Some(0xa)
    } else {
        None
    };
    if let Some(offset) = neighbor {
        next = lhu(m, entry + offset);
        sound(m, 0x3d);
    }
    tab_arrows(m, sext16(next));
    match next & 0x1f {
        0x16 => return 1,
        0x17 => return 2,
        0x15 => {}
        button => {
            let block = keys(m);
            sb(m, block + 0x39, button);
        }
    }
    let block = keys(m);
    let selected = lbu(m, block + 0x39);
    let action = lbu(m, block.wrapping_add(selected) + 0x14);
    item_text(m, action);
    style(m, 0x5b, 0);
    style(m, 0x5c, 0);
    0
}

/// 800E09D4: open the action list of the selected button (+0x3B = its
/// action; for type 3 the shoulder buttons 2..5 use the right-hand list,
/// +0xC bit 0x1000, slot 0x5C, else slot 0x5B), timer +0x6C = 0, sound
/// 100. With +8 bit 1 (re-entered) nothing but 0x10.
pub fn button_list_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    let selected = lbu(m, block + 0x39);
    let action = lbu(m, block.wrapping_add(selected) + 0x14);
    sb(m, block + 0x3b, action);
    let block = keys(m);
    if m.lw(block + 8) & 2 == 0 {
        let right = lbu(m, block + 0x30) == 3 && lbu(m, block + 0x39).wrapping_sub(2) < 4;
        if right {
            let flags = m.lw(block + 0xc) | 0x1000;
            m.sw(block + 0xc, flags);
            style(m, 0x5c, 1);
        } else {
            let block = keys(m);
            let flags = m.lw(block + 0xc) & !0x1000;
            m.sw(block + 0xc, flags);
            style(m, 0x5b, 1);
        }
        let block = keys(m);
        m.sw(block + 0x6c, 0);
        sound(m, 0x64);
    }
    m.pop_frame(0x18);
    0x10
}

/// The two list slots of the button list: (2, 5) for the right-hand list
/// (+0xC bit 0x1000), else (1, 4).
fn button_list_slots(m: &mut Machine) -> (u32, u32) {
    if kword(m, 0xc) & 0x1000 != 0 {
        (2, 5)
    } else {
        (1, 4)
    }
}

/// 800E0A9C: grow the button list (timer +0x6C to 10); a re-entry (+8
/// bit 1) clears the bit and returns 0x10 at once. Returns 0 while growing.
pub fn button_list_grow(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let block = keys(m);
    let flags = m.lw(block + 8);
    let result = if flags & 2 != 0 {
        m.sw(block + 8, flags & !2);
        0x10
    } else {
        let (first, second) = button_list_slots(m);
        let block = keys(m);
        let timer = m.lw(block + 0x6c);
        if (timer as i32) >= 0xa {
            let flags = m.lw(block + 8) & !2;
            m.sw(block + 8, flags);
            external(m, SET_VISUAL_PARAMETER, [first, timer, 0, 0]);
            let timer = kword(m, 0x6c);
            external(m, SET_VISUAL_PARAMETER, [second, timer, 0, 0]);
            0x10
        } else {
            external(m, SET_VISUAL_PARAMETER, [first, timer, 0, 0]);
            let timer = kword(m, 0x6c);
            external(m, SET_VISUAL_PARAMETER, [second, timer, 0, 0]);
            let block = keys(m);
            let timer = m.lw(block + 0x6c).wrapping_add(1);
            m.sw(block + 0x6c, timer);
            0
        }
    };
    m.pop_frame(0x20);
    result
}

/// 800E0B94: the button action list (held input): the styles of the 21
/// actions (assigned ones and the analog axes' pairs highlighted, the
/// choice +0x3B style 4) and its help; TRIANGLE closes (sounds 0x3B and
/// 0x65, returns 0x10); START nothing; CROSS (sound 0x3C) on the special
/// actions 0xE/0xF/0x13 asks for confirmation (+8 |= 2, returns 4), else
/// assigns the action to the button, clearing it from the others (0x14)
/// and its pair from the analog axes (0xC) (sound 0x65, returns 0x10);
/// DOWN/UP step through 0..0xA and 0xB..0x14, RIGHT/LEFT jump between the
/// two columns (sound 0x3D).
pub fn button_list(m: &mut Machine) -> u32 {
    m.push_frame(0x30);
    let result = button_list_body(m);
    m.pop_frame(0x30);
    result
}

fn button_list_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    let ctx = context(m);
    let buttons = block + 0x14;
    let axes = block + 0x24;
    let held = m.lw(ctx + 0x130);
    for k in 0..0x15 {
        style(m, k + 0x33, 2);
    }
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    if lbu(m, ITEM_COUNTS.wrapping_add(kind)) != 0 {
        let mut item = 0u32;
        let mut offset = 0u32;
        loop {
            let button = layout_button(m, offset);
            let block = keys(m);
            let action = lbu(m, block.wrapping_add(button) + 0x14);
            if action != 0x14 {
                style(m, action + 0x33, 1);
            }
            let block = keys(m);
            let kind = lbu(m, block + 0x30);
            let count = lbu(m, ITEM_COUNTS.wrapping_add(kind));
            item = item.wrapping_add(1);
            offset = offset.wrapping_add(0x14);
            if (item as i32) >= (count as i32) {
                break;
            }
        }
    }
    for k in 0..4 {
        let block = keys(m);
        let action = lbu(m, block.wrapping_add(k) + 0x24);
        if (action as i32) < 8 {
            style(m, action + 0x33, 1);
            style(m, action + 0x34, 1);
        }
    }
    let choice = kbyte(m, 0x3b);
    style(m, choice + 0x33, 4);
    let choice = kbyte(m, 0x3b);
    item_text(m, choice);
    if held & 0x20 != 0 {
        sound(m, 0x3b);
        sound(m, 0x65);
        return 0x10;
    }
    if held & 0x40 != 0 {
        return 0;
    }
    if held & 0x10 != 0 {
        sound(m, 0x3c);
        let block = keys(m);
        let choice = lbu(m, block + 0x3b);
        if choice == 0xe || choice == 0x13 || choice == 0xf {
            let block = keys(m);
            let flags = m.lw(block + 8) | 2;
            m.sw(block + 8, flags);
            return 4;
        }
        sound(m, 0x65);
        assign_choice(m, buttons, axes);
        return 0x10;
    }
    if held & 2 != 0 {
        sound(m, 0x3d);
        let block = keys(m);
        let choice = lbu(m, block + 0x3b);
        let next = match choice {
            0xa => 0,
            0x14 => 0xb,
            _ => choice + 1,
        };
        sb(m, block + 0x3b, next);
        return 0;
    }
    if held & 1 != 0 {
        sound(m, 0x3d);
        let block = keys(m);
        let choice = lbu(m, block + 0x3b);
        let next = match choice {
            0 => 0xa,
            0xb => 0x14,
            _ => choice.wrapping_sub(1),
        };
        sb(m, block + 0x3b, next);
        return 0;
    }
    if held & 8 != 0 {
        let block = keys(m);
        let choice = lbu(m, block + 0x3b);
        if choice < 0xa {
            sb(m, block + 0x3b, choice + 0xb);
        } else if choice == 0xa {
            sb(m, block + 0x3b, 0x14);
        } else {
            return 0;
        }
        sound(m, 0x3d);
        return 0;
    }
    if held & 4 != 0 {
        let block = keys(m);
        let choice = lbu(m, block + 0x3b);
        if choice >= 0xb {
            sb(m, block + 0x3b, choice - 0xb);
            sound(m, 0x3d);
        }
    }
    0
}

/// 800E0F8C: shrink the button list (timer +0x6C to 0). Returns 0 while
/// shrinking, then 8.
pub fn button_list_shrink(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let (first, second) = button_list_slots(m);
    let block = keys(m);
    let timer = m.lw(block + 0x6c);
    let result = if (timer as i32) <= 0 {
        external(m, SET_VISUAL_PARAMETER, [first, timer, 0, 0]);
        let timer = kword(m, 0x6c);
        external(m, SET_VISUAL_PARAMETER, [second, timer, 0, 0]);
        8
    } else {
        let timer = timer.wrapping_sub(1);
        m.sw(block + 0x6c, timer);
        external(m, SET_VISUAL_PARAMETER, [first, timer, 0, 0]);
        let timer = kword(m, 0x6c);
        external(m, SET_VISUAL_PARAMETER, [second, timer, 0, 0]);
        0
    };
    m.pop_frame(0x20);
    result
}

/// 800E1040: open the confirmation for a special action (0xE/0xF: help
/// 0x8B, 0x13: 0x8D): timer +0x74 and answer +0x44 cleared, slots 3/6
/// hidden, the answer's styles (0x51/0x52), sound 100. Returns 0x10.
pub fn confirm_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    m.sw(block + 0x74, 0);
    m.sw(block + 0x44, 0);
    external(m, SET_VISUAL_PARAMETER, [3, 0, 0, 0]);
    external(m, SET_VISUAL_PARAMETER, [6, 0, 0, 0]);
    external(m, CLEAR_TEXT, [0; 4]);
    let choice = kbyte(m, 0x3b);
    if choice.wrapping_sub(0xe) < 2 {
        external(m, SHOW_TEXT, [0x8b, 1, 0, 0]);
    } else if choice == 0x13 {
        external(m, SHOW_TEXT, [0x8d, 1, 0, 0]);
    }
    answer_styles(m);
    sound(m, 0x64);
    m.pop_frame(0x18);
    0x10
}

/// The YES/NO styles of the confirmation answer +0x44 (0 or 1).
fn answer_styles(m: &mut Machine) {
    match kword(m, 0x44) {
        0 => {
            style(m, 0x51, 1);
            style(m, 0x52, 2);
        }
        1 => {
            style(m, 0x51, 2);
            style(m, 0x52, 1);
        }
        _ => {}
    }
}

/// 800E1128: grow the confirmation (timer +0x74 to 10, slots 3/6). Returns
/// 0 while growing, then 0x10.
pub fn confirm_grow(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let timer = kword(m, 0x74);
    external(m, SET_VISUAL_PARAMETER, [3, timer, 0, 0]);
    let again = kword(m, 0x74);
    external(m, SET_VISUAL_PARAMETER, [6, again, 0, 0]);
    let result = if (timer as i32) >= 0xa {
        0x10
    } else {
        let block = keys(m);
        let timer = m.lw(block + 0x74).wrapping_add(1);
        m.sw(block + 0x74, timer);
        0
    };
    m.pop_frame(0x18);
    result
}

/// Assign the choice +0x3B to the selected button +0x39: it leaves the
/// other buttons (0x14 = none) and its pair leaves the analog axes (0xC),
/// as 800E0B94 and 800E1518 compile it.
fn assign_choice(m: &mut Machine, buttons: u32, axes: u32) {
    for k in 0..0xf {
        let entry = buttons + k;
        let block = keys(m);
        if lbu(m, entry) == lbu(m, block + 0x3b) {
            sb(m, entry, 0x14);
        }
    }
    for k in 0..4 {
        let entry = axes + k;
        let block = keys(m);
        let pair = lbu(m, block + 0x3b) & 0xfe;
        let action = lbu(m, entry);
        if action == pair && action < 8 {
            sb(m, entry, 0xc);
        }
    }
    let block = keys(m);
    let selected = lbu(m, block + 0x39);
    let choice = lbu(m, block + 0x3b);
    sb(m, buttons.wrapping_add(selected), choice);
}

/// 800CB1DC / 800CB1E4: the x positions (less 0x79) of YES and NO by
/// language (five bytes each, copied to the stack frame).
const YES_X: u32 = 0x800c_b1dc;
const NO_X: u32 = 0x800c_b1e4;
/// ov2core: string `index` of a text table (800D0B28).
const TEXT_STRING: u32 = 0x800d_0b28;

/// 800E11B8: the confirmation answer +0x44 (0 YES, 1 NO). TRIANGLE cancels
/// (answer 2, timer +0x74 = 10, sounds 0x3B and 0x65, returns 0x10); CROSS
/// (timer 10, sounds 0x3C and 0x65) applies the answer to the analog option
/// of the choice (0xE/0xF: +0x10 bit 4, 0x13: bit 2; YES sets, NO clears)
/// and returns 0x10; DOWN/UP pick NO/YES (sound 0x3D). Otherwise (and after
/// START) draws strings 0x2A/0x2B in fonts 0xC (chosen) and 0xE at the
/// language's x positions, with the answer styles; returns 0.
pub fn confirm_answer(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x50);
    let result = confirm_answer_body(m, frame);
    m.pop_frame(0x50);
    result
}

fn confirm_answer_body(m: &mut Machine, frame: u32) -> u32 {
    let ctx = context(m);
    let pressed = m.lw(ctx + 0x12c);
    if pressed & 0x20 != 0 {
        sound(m, 0x3b);
        sound(m, 0x65);
        let block = keys(m);
        m.sw(block + 0x44, 2);
        m.sw(block + 0x74, 0xa);
        return 0x10;
    }
    if pressed & 0x40 == 0 {
        if pressed & 0x10 != 0 {
            let block = keys(m);
            m.sw(block + 0x74, 0xa);
            sound(m, 0x3c);
            sound(m, 0x65);
            let block = keys(m);
            let choice = lbu(m, block + 0x3b);
            let bit = match choice {
                0xe | 0xf => 4,
                0x13 => 2,
                _ => return 0x10,
            };
            match m.lw(block + 0x44) {
                0 => {
                    let bits = m.lw(block + 0x10) | bit;
                    m.sw(block + 0x10, bits);
                }
                1 => {
                    let bits = m.lw(block + 0x10) & !bit;
                    m.sw(block + 0x10, bits);
                }
                _ => {}
            }
            return 0x10;
        }
        if pressed & 2 != 0 {
            let block = keys(m);
            if m.lw(block + 0x44) == 0 {
                m.sw(block + 0x44, 1);
                sound(m, 0x3d);
            }
        } else if pressed & 1 != 0 {
            let block = keys(m);
            if m.lw(block + 0x44) == 1 {
                m.sw(block + 0x44, 0);
                sound(m, 0x3d);
            }
        }
    }
    for k in 0..5 {
        let x = lbu(m, YES_X + k);
        sb(m, frame + 0x20 + k, x);
    }
    let ctx = context(m);
    for k in 0..5 {
        let x = lbu(m, NO_X + k);
        sb(m, frame + 0x28 + k, x);
    }
    let language = m.lw(ctx + 0x38c);
    let yes_x = lbu(m, frame.wrapping_add(language).wrapping_add(0x20)) + 0x79;
    let no_x = lbu(m, frame.wrapping_add(language).wrapping_add(0x28)) + 0x79;
    let (yes_font, no_font) = if kword(m, 0x44) == 0 {
        (0xc, 0xe)
    } else {
        (0xe, 0xc)
    };
    let table = work_word(m, 0xb4);
    let yes = external(m, TEXT_STRING, [0x2a, table, 0, 0]);
    draw_text(m, frame, yes_font, yes, yes_x, 0x126);
    let table = work_word(m, 0xb4);
    let no = external(m, TEXT_STRING, [0x2b, table, 0, 0]);
    draw_text(m, frame, no_font, no, no_x, 0x143);
    answer_styles(m);
    0
}

/// 800E1518: close the confirmation (timer +0x74 down to 0 on slots 3/6
/// and, after an answer, the button list's slots). An answer (YES or NO)
/// then clears +8 bit 1 and assigns the choice (returns 0x20); a cancel
/// returns 8. Returns 0 while closing.
pub fn confirm_close(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let result = confirm_close_body(m);
    m.pop_frame(0x28);
    result
}

fn confirm_close_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    let buttons = block + 0x14;
    let axes = block + 0x24;
    let (first, second) = button_list_slots(m);
    let block = keys(m);
    let answer = m.lw(block + 0x44);
    if answer < 2 {
        let timer = m.lw(block + 0x74);
        if (timer as i32) > 0 {
            external(m, SET_VISUAL_PARAMETER, [3, timer, 0, 0]);
            for slot in [6, first, second] {
                let timer = kword(m, 0x74);
                external(m, SET_VISUAL_PARAMETER, [slot, timer, 0, 0]);
            }
            let block = keys(m);
            let timer = m.lw(block + 0x74).wrapping_sub(1);
            m.sw(block + 0x74, timer);
            return 0;
        }
        let flags = m.lw(block + 8) & !2;
        m.sw(block + 8, flags);
        external(m, SET_VISUAL_PARAMETER, [3, timer, 0, 0]);
        for slot in [6, first, second] {
            let timer = kword(m, 0x74);
            external(m, SET_VISUAL_PARAMETER, [slot, timer, 0, 0]);
        }
        assign_choice(m, buttons, axes);
        return 0x20;
    }
    if answer != 2 {
        return 0;
    }
    let timer = m.lw(block + 0x74);
    external(m, SET_VISUAL_PARAMETER, [3, timer, 0, 0]);
    let again = kword(m, 0x74);
    external(m, SET_VISUAL_PARAMETER, [6, again, 0, 0]);
    if (timer as i32) <= 0 {
        return 8;
    }
    let block = keys(m);
    let timer = m.lw(block + 0x74).wrapping_sub(1);
    m.sw(block + 0x74, timer);
    0
}

// ---- the KEY CONFIG task machine (800E47E0, table 0x800F670C) -----------------------

/// 800E1764 (group 0, step 0): select visual page 0x10, clear +0xC, styles
/// 1..12 = 3, then check the pad (context +0x156): disconnected or
/// unsupported (+0x156 bit 1 or type bit 0x20: help 0x88/0x8A after a
/// clear, returns 1; other +0x156 states: help 0x89, returns 0); a
/// supported pad returns 2.
pub fn pad_check(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    external(m, CLEAR_TEXT, [0; 4]);
    external(m, SELECT_VISUAL_PAGE, [0x10, 0, 0, 0]);
    let block = keys(m);
    m.sw(block + 0xc, 0);
    for slot in 1..=0xc {
        style(m, slot, 3);
    }
    let ctx = context(m);
    let state = lh(m, ctx + 0x156);
    let bits = lhu(m, ctx + 0x156);
    let result = if state != 0 {
        if bits & 2 != 0 {
            external(m, CLEAR_TEXT, [0; 4]);
            external(m, SHOW_TEXT, [0x88, 1, 0, 0]);
            1
        } else {
            external(m, SHOW_TEXT, [0x89, 1, 0, 0]);
            0
        }
    } else if m.lw(ctx + 0x124) & 0x20 != 0 {
        external(m, CLEAR_TEXT, [0; 4]);
        external(m, SHOW_TEXT, [0x8a, 1, 0, 0]);
        1
    } else {
        2
    };
    m.pop_frame(0x18);
    result
}

/// 800E182C (group 0, step 1): 4 (restart) once context +0x156 has any of
/// bits 0, 1 or 3, else 0.
pub fn pad_wait(m: &mut Machine) -> u32 {
    let ctx = context(m);
    u32::from(lhu(m, ctx + 0x156) & 0xb != 0) << 2
}

/// 800E1850 (group 1, step 0): the controller type +0x30 from the context
/// type bits +0x124 (4 or 2: 0, 0x10: 1, 1: 2, 8: 3; the last set bit
/// wins), +0x34 = the type bits, the settings loaded (800DEAB0), +0xC low
/// bits = 8 (| 7 for types 0, 1 and 3). Returns 1, or 4 for type bit 0x20.
pub fn init_type(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let ctx = context(m);
    let result = if m.lw(ctx + 0x124) & 0x20 != 0 {
        4
    } else {
        for (bit, kind) in [(4, 0), (2, 0), (0x10, 1), (1, 2), (8, 3)] {
            let ctx = context(m);
            if m.lw(ctx + 0x124) & bit != 0 {
                let block = keys(m);
                sb(m, block + 0x30, kind);
            }
        }
        let ctx = context(m);
        let block = keys(m);
        let types = m.lw(ctx + 0x124);
        m.sw(block + 0x34, types);
        load_settings(m);
        let block = keys(m);
        let flags = (m.lw(block + 0xc) & !0xf) | 8;
        let kind = lbu(m, block + 0x30);
        m.sw(block + 0xc, flags);
        if matches!(kind, 0 | 1 | 3) {
            let block = keys(m);
            let flags = m.lw(block + 0xc) | 7;
            m.sw(block + 0xc, flags);
        }
        1
    };
    m.pop_frame(0x18);
    result
}

/// ov2air: upload image `index` of an archive (800CE978).
const UPLOAD_IMAGE: u32 = 0x800c_e978;

/// 800E19C4 (group 1, step 1): upload the controller picture (image
/// `type` of the work block's archive +0x200A0, mode 0x5528, synchronous)
/// and set style 0x79 to the type. Returns 2.
pub fn show_type(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x20);
    let kind = kbyte(m, 0x30);
    m.sw(frame + 0x10, 1);
    let archive = work_word(m, 0xa0);
    external(m, UPLOAD_IMAGE, [archive, kind, 0x5528, 0]);
    let kind = kbyte(m, 0x30);
    style(m, 0x79, kind);
    m.pop_frame(0x20);
    2
}

/// One option row of 800E1A2C: the slot of the current `value` takes style
/// 4 and shows its help when the row +0x2E is `row` (else style 1); the
/// other slots take style 2. Values beyond the slots style nothing.
fn option_row(m: &mut Machine, row: u32, value: u32, slots: &[u32], texts: &[u32]) {
    if value as usize >= slots.len() {
        return;
    }
    for (k, (&slot, &text)) in slots.iter().zip(texts).enumerate() {
        if k as u32 != value {
            style(m, slot, 2);
        } else if khalf(m, 0x2e) == row {
            style(m, slot, 4);
            external(m, SHOW_TEXT, [text, 1, 0, 0]);
        } else {
            style(m, slot, 1);
        }
    }
}

/// 800E1A2C: the MAIN page rows (+0x2E: 1 the page tab, 2 the analog
/// switch +0x10 bit 0 (slots 2/3), 3 the neutral zone +0x2D (slots 5..7,
/// label 4; with +0xC bit 0), 4 the response +0x2C (slots 9..11, label 8;
/// with +0xC bit 1), 5 EXIT (slot 0xC)); disabled rows take style 3. The
/// selected row shows its help.
pub fn settings_rows(m: &mut Machine) {
    m.push_frame(0x18);
    external(m, CLEAR_TEXT, [0; 4]);
    if khalf(m, 0x2e) == 1 {
        style(m, 1, 4);
        external(m, SHOW_TEXT, [0x2d, 1, 0, 0]);
    } else {
        style(m, 1, 1);
    }
    let off = u32::from(kword(m, 0x10) & 1 == 0);
    option_row(m, 2, off, &[2, 3], &[0x2e, 0x2f]);
    if kword(m, 0xc) & 1 != 0 {
        let value = kbyte(m, 0x2d);
        option_row(m, 3, value, &[5, 6, 7], &[0x32, 0x31, 0x30]);
        let label = if khalf(m, 0x2e) == 3 { 1 } else { 2 };
        style(m, 4, label);
    } else {
        for slot in [5, 6, 7, 4] {
            style(m, slot, 3);
        }
    }
    if kword(m, 0xc) & 2 != 0 {
        let value = kbyte(m, 0x2c);
        option_row(m, 4, value, &[9, 0xa, 0xb], &[0x33, 0x34, 0x35]);
        let label = if khalf(m, 0x2e) == 4 { 1 } else { 2 };
        style(m, 8, label);
    } else {
        for slot in [9, 0xa, 0xb, 8] {
            style(m, slot, 3);
        }
    }
    if khalf(m, 0x2e) == 5 {
        style(m, 0xc, 1);
        external(m, SHOW_TEXT, [0x36, 1, 0, 0]);
    } else {
        style(m, 0xc, 2);
    }
    m.pop_frame(0x18);
}

/// 800E1EA0 (group 2, step 0): row +0x2E = 1, visual page 0x11. Returns 1.
pub fn settings_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    sh(m, block + 0x2e, 1);
    external(m, SELECT_VISUAL_PAGE, [0x11, 0, 0, 0]);
    m.pop_frame(0x18);
    1
}

/// Skip row `row` while +0xC lacks `bit`: the row +0x2E moves on by `step`.
fn skip_row(m: &mut Machine, bit: u32, row: u32, step: u32) {
    let block = keys(m);
    if m.lw(block + 0xc) & bit == 0 && lh(m, block + 0x2e) == row {
        let value = lhu(m, block + 0x2e).wrapping_add(step);
        sh(m, block + 0x2e, value);
    }
}

/// Move the row +0x2E by `step` (stored before sound 0x3D).
fn move_row(m: &mut Machine, step: u32) {
    let block = keys(m);
    let row = lhu(m, block + 0x2e).wrapping_add(step);
    sh(m, block + 0x2e, row);
    sound(m, 0x3d);
}

/// 800E1ED0 (group 2, step 1): the MAIN page input. TRIANGLE leaves (0x10,
/// sound 0x3B); START jumps to EXIT (row 5, sound 0x3D); CROSS on EXIT
/// returns 0x20 (sound 0x3C); DOWN/UP move through rows 1..5 (wrapping,
/// skipping disabled rows 3/4); LEFT/RIGHT on row 1 switch the page
/// (sound 0x2E, returns 2), on row 2 turn the analog switch on/off, on rows
/// 3/4 change the neutral zone / response within 0..2 (sound 0x2E). Then
/// redraws the rows (800E1A2C) and returns 0.
pub fn settings_input(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let result = settings_input_body(m);
    m.pop_frame(0x20);
    result
}

fn settings_input_body(m: &mut Machine) -> u32 {
    let pressed = cword(m, 0x12c);
    if pressed & 0x20 != 0 {
        sound(m, 0x3b);
        return 0x10;
    }
    if pressed & 0x40 != 0 && khalf(m, 0x2e) != 5 {
        sound(m, 0x3d);
        let block = keys(m);
        sh(m, block + 0x2e, 5);
        settings_rows(m);
        return 0;
    }
    if cword(m, 0x12c) & 0x10 != 0 && khalf(m, 0x2e) == 5 {
        sound(m, 0x3c);
        return 0x20;
    }
    let pressed = cword(m, 0x12c);
    if pressed & 2 != 0 {
        move_row(m, 1);
        skip_row(m, 1, 3, 1);
        skip_row(m, 2, 4, 1);
        let block = keys(m);
        if (lh(m, block + 0x2e) as i32) >= 6 {
            sh(m, block + 0x2e, 1);
        }
    } else if pressed & 1 != 0 {
        move_row(m, u32::MAX);
        skip_row(m, 2, 4, u32::MAX);
        skip_row(m, 1, 3, u32::MAX);
        let block = keys(m);
        if (lh(m, block + 0x2e) as i32) <= 0 {
            sh(m, block + 0x2e, 5);
        }
    } else if pressed & 0xc != 0 {
        let right = pressed & 8 != 0;
        let block = keys(m);
        match lh(m, block + 0x2e) {
            1 => {
                sound(m, 0x2e);
                return 2;
            }
            2 => {
                let bits = m.lw(block + 0x10);
                if (bits & 1 != 0) == right {
                    m.sw(block + 0x10, if right { bits & !1 } else { bits | 1 });
                    sound(m, 0x2e);
                }
            }
            row @ (3 | 4) => {
                let field = block + if row == 3 { 0x2d } else { 0x2c };
                let value = lbu(m, field);
                if right && value < 2 {
                    sb(m, field, value + 1);
                    sound(m, 0x2e);
                } else if !right && value != 0 {
                    sb(m, field, value - 1);
                    sound(m, 0x2e);
                }
            }
            _ => {}
        }
    }
    settings_rows(m);
    0
}

/// 800F666C: the four presets (800DE73C..800DE978) by controller type.
pub const PRESETS: u32 = 0x800f_666c;
/// 800F669C: the analog page's sub-mode lists (axis row, action list,
/// inversion choice).
pub const ANALOG_MODES: u32 = 0x800f_669c;
/// 800F66CC: the button page's sub-mode lists (button grid, action list,
/// confirmation).
pub const BUTTON_MODES: u32 = 0x800f_66cc;
/// 800F670C: the KEY CONFIG task lists (pad check, type, MAIN, analog and
/// button pages, calibration, store).
pub const TASKS: u32 = 0x800f_670c;

/// `jalr` through a two-level table: `table[group][step]` with the group
/// and step halfwords at `+field` and `+field + 2` of the KEY CONFIG block.
fn run_table(m: &mut Machine, table: u32, field: u32) -> u32 {
    let block = keys(m);
    let group = lh(m, block + field);
    let list = m.lw(table.wrapping_add(group << 2));
    let step = lh(m, block + field + 2);
    let routine = m.lw(list.wrapping_add(step << 2));
    call_pointer(m, routine, [block, 0, 0, 0])
}

/// The list panel offset: slot 2 at x = +0x28 (+0xC bit 0x80), -0x28 (bit
/// 0x40) or 0.
fn list_offset(m: &mut Machine) {
    let flags = kword(m, 0xc);
    let x = if flags & 0x80 != 0 {
        0x28
    } else if flags & 0x40 != 0 {
        0xffff_ffd8
    } else {
        0
    };
    external(m, SET_VISUAL_OFFSET, [2, x, 0, 0]);
}

/// A sub-mode result of the analog and button pages: 1 leaves the grid
/// down (row 3), 2 up (row 1) (both re-centre slot 2); 4 the next list
/// (+4 + 1, step 0), 0x10 the next step, 8 the previous list, 0x20 back to
/// list 0.
fn sub_mode_result(m: &mut Machine, result: u32) {
    let block = keys(m);
    if result & 3 != 0 {
        sh(m, block + 0x2e, if result & 1 != 0 { 3 } else { 1 });
        external(m, SET_VISUAL_OFFSET, [2, 0, 0, 0]);
    } else if result & 4 != 0 {
        let list = lhu(m, block + 4);
        sh(m, block + 6, 0);
        sh(m, block + 4, list.wrapping_add(1));
    } else if result & 0x10 != 0 {
        let step = lhu(m, block + 6);
        sh(m, block + 6, step.wrapping_add(1));
    } else if result & 8 != 0 {
        let list = lhu(m, block + 4);
        sh(m, block + 6, 0);
        sh(m, block + 4, list.wrapping_sub(1));
    } else if result & 0x20 != 0 {
        sh(m, block + 6, 0);
        sh(m, block + 4, 0);
    }
}

/// 800E2298 (group 3, step 0): open the analog page (type 2 has none:
/// returns 2): +0xC bit 0x20 cleared, visual page 0x13/0x15/0x17 by type,
/// slot 2 centred, list slots 1..4 cleared, sub-mode 0/0. Returns 1.
pub fn analog_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    let result = if lbu(m, block + 0x30) == 2 {
        2
    } else {
        let flags = m.lw(block + 0xc) & !0x20;
        let kind = lbu(m, block + 0x30);
        m.sw(block + 0xc, flags);
        let page = match kind {
            0 => Some(0x13),
            1 => Some(0x15),
            3 => Some(0x17),
            _ => None,
        };
        if let Some(page) = page {
            external(m, SELECT_VISUAL_PAGE, [page, 0, 0, 0]);
        }
        external(m, SET_VISUAL_OFFSET, [2, 0, 0, 0]);
        for slot in [1, 3, 2, 4] {
            external(m, SET_VISUAL_PARAMETER, [slot, 0, 0, 0]);
        }
        let block = keys(m);
        sh(m, block + 6, 0);
        sh(m, block + 4, 0);
        1
    };
    m.pop_frame(0x18);
    result
}

/// Call the preset of the controller type (table 0x800F666C).
fn apply_preset(m: &mut Machine) {
    let kind = kbyte(m, 0x30);
    let preset = m.lw(PRESETS.wrapping_add(kind << 2));
    let ctx = context(m);
    call_pointer(m, preset, [ctx, 0, 0, 0]);
}

/// 800E2390 (group 3, step 1): the analog page. The MAIN rows (800DF424);
/// on row 0 the axis sub-modes (table 0x800F669C) run (0x40 leaves with
/// 0x10); on the other rows: START jumps to row 3 (+0xC bit 0x10 cleared,
/// sound 0x3D); CROSS on row 3 applies the preset when +0xC bit 0x10 is set
/// (sound 0x3C), else returns 0x40; TRIANGLE leaves (0x10, sound 0x3B);
/// held DOWN/UP move through rows 0..3 (row 2 only with +0xC bit 2; row 0
/// enters the axes at +0x38 = 0, or 3 for type 1 / from above); RIGHT on
/// row 2 returns 2, on row 3 sets bit 0x10; LEFT on row 1 returns 0x40, on
/// row 3 clears bit 0x10 (sound 0x2E). Then the axis styles; returns 0.
pub fn analog_page(m: &mut Machine) -> u32 {
    m.push_frame(0x38);
    let result = analog_page_body(m);
    m.pop_frame(0x38);
    result
}

fn analog_page_body(m: &mut Machine) -> u32 {
    main_rows(m);
    let row = khalf(m, 0x2e);
    if row == 0 {
        let result = run_table(m, ANALOG_MODES, 4);
        list_offset(m);
        if result & 0x40 != 0 {
            return 0x10;
        }
        sub_mode_result(m, result);
    } else if cword(m, 0x12c) & 0x40 != 0 && row != 3 {
        sound(m, 0x3d);
        let block = keys(m);
        let flags = m.lw(block + 0xc);
        sh(m, block + 0x2e, 3);
        m.sw(block + 0xc, flags & !0x10);
    } else {
        let ctx = context(m);
        let pressed = m.lw(ctx + 0x12c);
        if pressed & 0x10 != 0 {
            if khalf(m, 0x2e) == 3 {
                sound(m, 0x3c);
                if kword(m, 0xc) & 0x10 == 0 {
                    return 0x40;
                }
                apply_preset(m);
            }
        } else if pressed & 0x20 != 0 {
            sound(m, 0x3b);
            return 0x10;
        } else {
            let held = m.lw(ctx + 0x130);
            if let Some(result) = analog_rows(m, held, pressed) {
                return result;
            }
        }
    }
    analog_styles(m);
    0
}

/// The row input of 800E2390 (held DOWN/UP, pressed RIGHT/LEFT); `Some`
/// for a page result.
fn analog_rows(m: &mut Machine, held: u32, pressed: u32) -> Option<u32> {
    if held & 2 != 0 {
        move_row(m, 1);
        skip_row(m, 4, 2, 1);
        let block = keys(m);
        if (lh(m, block + 0x2e) as i32) >= 4 {
            sh(m, block + 0x2e, 0);
        }
        let block = keys(m);
        if lh(m, block + 0x2e) == 0 {
            let axis = if lbu(m, block + 0x30) == 1 { 3 } else { 0 };
            sb(m, block + 0x38, axis);
        }
    } else if held & 1 != 0 {
        move_row(m, u32::MAX);
        skip_row(m, 4, 2, u32::MAX);
        let block = keys(m);
        if (lh(m, block + 0x2e) as i32) < 0 {
            sh(m, block + 0x2e, 3);
        }
        let block = keys(m);
        if lh(m, block + 0x2e) == 0 {
            sb(m, block + 0x38, 3);
        }
    } else if pressed & 8 != 0 {
        let block = keys(m);
        match lh(m, block + 0x2e) {
            2 => {
                sound(m, 0x2e);
                return Some(2);
            }
            3 => {
                let flags = m.lw(block + 0xc);
                if flags & 0x10 == 0 {
                    m.sw(block + 0xc, flags | 0x10);
                    sound(m, 0x2e);
                }
            }
            _ => {}
        }
    } else if pressed & 4 != 0 {
        let block = keys(m);
        match lh(m, block + 0x2e) {
            1 => {
                sound(m, 0x2e);
                return Some(0x40);
            }
            3 => {
                let flags = m.lw(block + 0xc);
                if flags & 0x10 != 0 {
                    m.sw(block + 0xc, flags & !0x10);
                    sound(m, 0x2e);
                }
            }
            _ => {}
        }
    }
    None
}

/// The axis styles of 800E2390: per axis, slots 0x15 + 2k / 0x16 + 2k show
/// the action with its inversion (+ 0x1F for the axis chosen on row 0,
/// else + 1), slots 0xB + 2k / 0xC + 2k style 1 (chosen) or 2.
fn analog_styles(m: &mut Machine) {
    for axis in 0..4u32 {
        let block = keys(m);
        let (offset, frame) = if lh(m, block + 0x2e) == 0 && lbu(m, block + 0x38) == axis {
            (0x1f, 1)
        } else {
            (1, 2)
        };
        let entry = keys(m) + axis;
        let value = lbu(m, entry + 0x24) + lbu(m, entry + 0x28);
        style(m, 0x15 + 2 * axis, value.wrapping_add(offset));
        let entry = keys(m) + axis;
        let value = (lbu(m, entry + 0x24) + 1).wrapping_sub(lbu(m, entry + 0x28));
        style(m, 0x16 + 2 * axis, value.wrapping_add(offset));
        style(m, 0xb + 2 * axis, frame);
        style(m, 0xc + 2 * axis, frame);
    }
}

/// 800E2970 (group 4, step 0): open the button page: visual page
/// 0x12/0x14/0x18/0x16 by type, sub-mode 0/0, selected button +0x39 = the
/// layout's first, slot parameters 1..10 (7 and 9 at 10), +0xC bit 0x20.
/// Returns 1.
pub fn buttons_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let page = match kbyte(m, 0x30) {
        0 => Some(0x12),
        1 => Some(0x14),
        2 => Some(0x18),
        3 => Some(0x16),
        _ => None,
    };
    if let Some(page) = page {
        external(m, SELECT_VISUAL_PAGE, [page, 0, 0, 0]);
    }
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    sh(m, block + 6, 0);
    sh(m, block + 4, 0);
    let layout = m.lw(block.wrapping_add(kind << 2) + 0x48);
    let first = lbu(m, layout);
    sb(m, block + 0x39, first);
    for slot in 1..=0xa {
        let value = if slot == 7 || slot == 9 { 0xa } else { 0 };
        external(m, SET_VISUAL_PARAMETER, [slot, value, 0, 0]);
    }
    let block = keys(m);
    let flags = m.lw(block + 0xc) | 0x20;
    m.sw(block + 0xc, flags);
    m.pop_frame(0x18);
    1
}

/// The glyph of an action as 800E2AB4 shows it: 0xE/0xF become 0x15/0x16
/// with +0x10 bit 2, 0x13 becomes 0x17 with bit 1.
fn shown_action(m: &mut Machine, action: u32) -> u32 {
    let bits = kword(m, 0x10);
    if action == 0xe && bits & 4 != 0 {
        0x15
    } else if action == 0xf && bits & 4 != 0 {
        0x16
    } else if action == 0x13 && bits & 2 != 0 {
        0x17
    } else {
        action
    }
}

/// The button glyphs of 800E2AB4: per layout item, the button's style
/// (0xB + button: 1 for the selected one when `highlight`, else 2) and the
/// glyph of its action (800DE4F8, blended when selected) at the work
/// block's primitive cursor.
fn button_glyphs(m: &mut Machine, layout: u32, highlight: bool) {
    let kind = kbyte(m, 0x30);
    if lbu(m, ITEM_COUNTS.wrapping_add(kind)) == 0 {
        return;
    }
    let mut entry = layout;
    let mut item = 0u32;
    loop {
        let button = m.lw(entry);
        let block = keys(m);
        let action = lbu(m, block.wrapping_add(button) + 0x14);
        let action = shown_action(m, action);
        let selected = highlight && button == kbyte(m, 0x39);
        style(m, button.wrapping_add(0xb), if selected { 1 } else { 2 });
        let high = work(m).wrapping_add(WORK_HIGH);
        let cursor = m.lw(high + 0x94);
        let next = glyph(m, cursor, item, action, u32::from(selected));
        let high = work(m).wrapping_add(WORK_HIGH);
        m.sw(high + 0x94, next);
        entry = entry.wrapping_add(0x14);
        let kind = kbyte(m, 0x30);
        let count = lbu(m, ITEM_COUNTS.wrapping_add(kind));
        item = item.wrapping_add(1);
        if (item as i32) >= (count as i32) {
            break;
        }
    }
}

/// The layout item of the selected button +0x39 (the last match, else 0).
fn selected_item(m: &mut Machine) -> u32 {
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    let count = lbu(m, ITEM_COUNTS.wrapping_add(kind));
    let mut found = 0;
    if count != 0 {
        let selected = lbu(m, block + 0x39);
        let mut entry = m.lw(block.wrapping_add(kind << 2) + 0x48);
        let mut item = 0u32;
        loop {
            if m.lw(entry) == selected {
                found = item;
            }
            item = item.wrapping_add(1);
            entry = entry.wrapping_add(0x14);
            if (item as i32) >= (count as i32) {
                break;
            }
        }
    }
    found
}

/// The neighbor halfword at `+offset` of layout item `item`.
fn item_neighbor(m: &mut Machine, item: u32, offset: u32) -> u32 {
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    let layout = m.lw(block.wrapping_add(kind << 2) + 0x48);
    lhu(m, layout.wrapping_add(item.wrapping_mul(0x14)) + offset)
}

/// 800E2AB4 (group 4, step 1): the button page. The MAIN rows (800DF424);
/// on row 0 the button sub-modes (table 0x800F66CC) run before the glyphs
/// are drawn (0x40 leaves with 0x10); on the other rows the glyphs, then:
/// TRIANGLE leaves (0x10); START jumps to row 3; CROSS on row 3 applies
/// the preset (+0xC bit 0x10) or returns 0x40; DOWN/UP move through rows
/// 1..3 and enter the grid at the selected button's down (+0xC) / up
/// (+0xE) neighbor; RIGHT/LEFT on row 3 set/clear bit 0x10, LEFT on row 1
/// returns 0x40 and on row 2 0x80 (sound 0x2E). The next button moves the
/// tab arrows and the selection (0x16/0x17 leave: 1/2). Returns 0.
pub fn button_page(m: &mut Machine) -> u32 {
    m.push_frame(0x38);
    let result = button_page_body(m);
    m.pop_frame(0x38);
    result
}

fn button_page_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    let kind = lbu(m, block + 0x30);
    let layout = m.lw(block.wrapping_add(kind << 2) + 0x48);
    main_rows(m);
    if khalf(m, 0x2e) == 0 {
        let result = run_table(m, BUTTON_MODES, 4);
        button_glyphs(m, layout, true);
        list_offset(m);
        if result & 0x40 != 0 {
            return 0x10;
        }
        sub_mode_result(m, result);
        return 0;
    }
    button_glyphs(m, layout, false);
    let pressed = cword(m, 0x12c);
    if pressed & 0x20 != 0 {
        sound(m, 0x3b);
        return 0x10;
    }
    let mut next = 0x15;
    if pressed & 0x40 != 0 && khalf(m, 0x2e) != 3 {
        sound(m, 0x3d);
        let block = keys(m);
        let flags = m.lw(block + 0xc);
        sh(m, block + 0x2e, 3);
        m.sw(block + 0xc, flags & !0x10);
    } else {
        let pressed = cword(m, 0x12c);
        if pressed & 0x10 != 0 {
            if khalf(m, 0x2e) == 3 {
                sound(m, 0x3c);
                if kword(m, 0xc) & 0x10 == 0 {
                    return 0x40;
                }
                apply_preset(m);
            }
        } else if pressed & 2 != 0 {
            move_row(m, 1);
            skip_row(m, 4, 2, 1);
            if (khalf(m, 0x2e) as i32) >= 4 {
                let item = selected_item(m);
                next = item_neighbor(m, item, 0xc);
                let block = keys(m);
                sh(m, block + 0x2e, 0);
            }
        } else if pressed & 1 != 0 {
            move_row(m, u32::MAX);
            skip_row(m, 4, 2, u32::MAX);
            if khalf(m, 0x2e) == 0 {
                let item = selected_item(m);
                next = item_neighbor(m, item, 0xe);
            }
        } else if pressed & 0xc != 0 {
            let right = pressed & 8 != 0;
            let block = keys(m);
            match lh(m, block + 0x2e) {
                1 if !right => {
                    sound(m, 0x2e);
                    return 0x40;
                }
                2 if !right => {
                    sound(m, 0x2e);
                    return 0x80;
                }
                3 => {
                    let flags = m.lw(block + 0xc);
                    if (flags & 0x10 != 0) != right {
                        m.sw(block + 0xc, flags ^ 0x10);
                        sound(m, 0x2e);
                    }
                }
                _ => {}
            }
        }
    }
    tab_arrows(m, sext16(next));
    match next & 0x1f {
        0x16 => 1,
        0x17 => 2,
        0x15 => 0,
        button => {
            let block = keys(m);
            sb(m, block + 0x39, button);
            0
        }
    }
}

// ---- calibration (group 5) -------------------------------------------------------------

/// The compiled `(value - 0x80) * 70 / 128` (signed division by shift).
fn scale70(value: u32) -> u32 {
    div_pow2(value.wrapping_sub(0x80).wrapping_mul(70), 7)
}
/// The compiled `(value - 0x80) * 35 / 128`.
fn scale35(value: u32) -> u32 {
    div_pow2(value.wrapping_sub(0x80).wrapping_mul(35), 7)
}

/// Take a primitive of `size` bytes at the work block's cursor.
fn take_prim(m: &mut Machine, size: u32) -> u32 {
    let high = work(m).wrapping_add(WORK_HIGH);
    let prim = m.lw(high + 0x94);
    m.sw(high + 0x94, prim.wrapping_add(size));
    prim
}

/// Link `prim` at the work block's ordering-table slot `[+0x90]`.
fn link(m: &mut Machine, prim: u32) {
    let slot = ot_slot(m);
    add_prim(m, slot, prim);
}

/// A Gouraud-shaded calibration primitive (POLY_G4: len 8, code 0x38, or
/// POLY_G3: len 6, code 0x30): orange (0xF0, 0x6E, 0) at the first `top`
/// vertices and dark (0x40, 0x37, 0) at the others, at `points`; linked.
fn shaded(m: &mut Machine, points: &[(u32, u32)], top: usize) {
    let quad = points.len() == 4;
    let prim = take_prim(m, if quad { 0x24 } else { 0x1c });
    sb(m, prim + 3, if quad { 8 } else { 6 });
    sb(m, prim + 7, if quad { 0x38 } else { 0x30 });
    for k in 0..points.len() {
        let color = prim + 4 + 8 * k as u32;
        let (r, g) = if k < top { (0xf0, 0x6e) } else { (0x40, 0x37) };
        sb(m, color, r);
        sb(m, color + 1, g);
        sb(m, color + 2, 0);
    }
    for (k, &(x, y)) in points.iter().enumerate() {
        sh(m, prim + 8 + 8 * k as u32, x);
        sh(m, prim + 0xa + 8 * k as u32, y);
    }
    link(m, prim);
}

/// 800E339C: the single-axis calibration (type 1, the twist axis at context
/// +0x64). Stage +0x68: 0 shows help 0x85 and records the right end at
/// +0x59 on START (axis >= 0x91, sound 0x3C); 1 shows 0x86 and records the
/// left end at +0x5D (axis < 0x70); 2 shows 0x87 and returns 1 on START.
/// Draws the two bar pairs and the axis arrows; returns 0.
pub fn single_axis_calibration(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let result = single_axis_body(m);
    m.pop_frame(0x28);
    result
}

fn single_axis_body(m: &mut Machine) -> u32 {
    let ctx = context(m);
    let axis = lh(m, ctx + 0x64);
    external(m, CLEAR_TEXT, [0; 4]);
    let (mut right, mut left) = (0, 0);
    let low = (axis as i32) < 0x80;
    match kbyte(m, 0x68) {
        0 => {
            external(m, SHOW_TEXT, [0x85, 1, 0, 0]);
            if low {
                left = scale70(axis);
            } else {
                right = scale70(axis);
            }
            if cword(m, 0x12c) & 0x40 != 0 && (axis as i32) >= 0x91 {
                sound(m, 0x3c);
                set_kbyte(m, 0x59, axis);
                set_kbyte(m, 0x68, 1);
            }
        }
        1 => {
            external(m, SHOW_TEXT, [0x86, 1, 0, 0]);
            right = scale70(kbyte(m, 0x59));
            if low {
                left = scale70(axis);
            }
            if cword(m, 0x12c) & 0x40 != 0 && (axis as i32) < 0x70 {
                sound(m, 0x3c);
                set_kbyte(m, 0x5d, axis);
                set_kbyte(m, 0x68, 2);
            }
        }
        2 => {
            external(m, SHOW_TEXT, [0x87, 1, 0, 0]);
            let block = keys(m);
            right = scale70(lbu(m, block + 0x59));
            left = scale70(lbu(m, block + 0x5d));
            if cword(m, 0x12c) & 0x40 != 0 {
                sound(m, 0x3c);
                return 1;
            }
        }
        _ => {}
    }
    let base = 0x6eu32;
    shaded(
        m,
        &[
            (0x82, base),
            (0x96, base),
            (0x82, right.wrapping_add(base)),
            (0x96, right.wrapping_add(base)),
        ],
        2,
    );
    shaded(
        m,
        &[
            (0x82, base),
            (0x96, base),
            (0x82, left.wrapping_add(base)),
            (0x96, left.wrapping_add(base)),
        ],
        2,
    );
    let up = base.wrapping_sub(left);
    shaded(m, &[(0xaa, base), (0xbe, base), (0xaa, up), (0xbe, up)], 2);
    let up = base.wrapping_sub(right);
    shaded(m, &[(0xaa, base), (0xbe, base), (0xaa, up), (0xbe, up)], 2);
    let pointer = scale70(axis);
    shaded(
        m,
        &[
            (0x82, pointer.wrapping_add(0x6e)),
            (0x78, pointer.wrapping_add(0x64)),
            (0x78, pointer.wrapping_add(0x78)),
        ],
        1,
    );
    shaded(
        m,
        &[
            (0xbe, 0x6eu32.wrapping_sub(pointer)),
            (0xc8, 0x64u32.wrapping_sub(pointer)),
            (0xc8, 0x78u32.wrapping_sub(pointer)),
        ],
        1,
    );
    0
}

/// A stick marker of 800E3ADC: a grey diamond (POLY_F4: len 5, code 0x28,
/// 0x808080) around (`x` + 0x69 + `dx`, `y` + 0x6E); linked.
fn marker(m: &mut Machine, x: u32, y: u32, dx: u32) {
    let prim = take_prim(m, 0x18);
    let cx = x.wrapping_add(0x69 + dx);
    sh(m, prim + 0xa, y.wrapping_add(0x68));
    sh(m, prim + 0xc, cx.wrapping_add(3));
    sb(m, prim + 3, 5);
    sb(m, prim + 7, 0x28);
    sb(m, prim + 4, 0x80);
    sb(m, prim + 5, 0x80);
    sb(m, prim + 6, 0x80);
    sh(m, prim + 8, cx);
    sh(m, prim + 0xe, y.wrapping_add(0x6e));
    sh(m, prim + 0x10, cx.wrapping_sub(3));
    sh(m, prim + 0x12, y.wrapping_add(0x6e));
    sh(m, prim + 0x14, cx);
    sh(m, prim + 0x16, y.wrapping_add(0x74));
    link(m, prim);
}

/// The four range triangles of one stick of 800E3ADC from apex (`x`, 0x6E):
/// the vertical extents from the max/min of axis `vertical` (140/256 + 0x28)
/// and the horizontal ones from axis `horizontal` (70/256 + `center`).
fn ranges(m: &mut Machine, x: u32, vertical: u32, horizontal: u32, center: u32) {
    let block = keys(m);
    let extent = |m: &mut Machine, offset: u32, factor: u32| {
        div_pow2(lbu(m, block + offset).wrapping_mul(factor), 8)
    };
    let top = extent(m, 0x58 + vertical, 140).wrapping_add(0x28);
    let bottom = extent(m, 0x5c + vertical, 140).wrapping_add(0x28);
    let far = extent(m, 0x58 + horizontal, 70).wrapping_add(center);
    let near = extent(m, 0x5c + horizontal, 70).wrapping_add(center);
    let apex = (x, 0x6e);
    shaded(m, &[apex, (x, bottom), (far, 0x6e)], 1);
    shaded(m, &[apex, (x, top), (far, 0x6e)], 1);
    shaded(m, &[apex, (x, top), (near, 0x6e)], 1);
    shaded(m, &[apex, (x, bottom), (near, 0x6e)], 1);
}

/// 800E3ADC: the two-stick calibration (types 0 and 3; axes at context
/// +0x62..+0x68). TRIANGLE leaves (sound 0x3B, returns 2). Draws both stick
/// markers, records each axis's max (+0x58..) and min (+0x5C..), draws the
/// range triangles, then stage +0x68 0..3 waits for the left stick up/down
/// and the right stick up/down extremes (helps 0x80..0x83) and 4 (help
/// 0x84) returns 1 on START (sound 0x3C). Returns 0 otherwise.
///
/// The help calls pass in `a1` the ordering-table slot pointer the
/// original leaves there (800D0F20 does not touch `a1`).
pub fn stick_calibration(m: &mut Machine) -> u32 {
    m.push_frame(0x30);
    let result = stick_body(m);
    m.pop_frame(0x30);
    result
}

fn stick_body(m: &mut Machine) -> u32 {
    let ctx = context(m);
    if m.lw(ctx + 0x12c) & 0x20 != 0 {
        sound(m, 0x3b);
        return 2;
    }
    let x = scale35(lh(m, ctx + 0x68));
    let y = scale70(lh(m, ctx + 0x66));
    marker(m, x, y, 0);
    let ctx = context(m);
    let x = scale35(lh(m, ctx + 0x64));
    let y = scale70(lh(m, ctx + 0x62));
    marker(m, x, y, 0x6e);
    for k in 0..4 {
        let ctx = context(m);
        let value = lh(m, ctx + 0x62 + 2 * k);
        let raw = lhu(m, ctx + 0x62 + 2 * k);
        let entry = keys(m) + k;
        if (value as i32) < (lbu(m, entry + 0x5c) as i32) {
            sb(m, entry + 0x5c, raw);
        }
        let entry = keys(m) + k;
        if (lbu(m, entry + 0x58) as i32) < (value as i32) {
            sb(m, entry + 0x58, raw);
        }
    }
    ranges(m, 0x69, 2, 3, 0x46);
    ranges(m, 0xd7, 0, 1, 0xb4);
    let leaked = ot_slot(m);
    external(m, CLEAR_TEXT, [0; 4]);
    let block = keys(m);
    let stage = lbu(m, block + 0x68);
    let (help, low, (a, b)) = match stage {
        0 => (0x80, true, (0x5c, 0x5e)),
        1 => (0x81, false, (0x58, 0x5a)),
        2 => (0x82, true, (0x5d, 0x5f)),
        3 => (0x83, false, (0x59, 0x5b)),
        4 => {
            external(m, SHOW_TEXT, [0x84, leaked, 0, 0]);
            if cword(m, 0x12c) & 0x40 != 0 {
                sound(m, 0x3c);
                return 1;
            }
            return 0;
        }
        _ => return 0,
    };
    let reached = |m: &mut Machine, offset: u32| {
        let value = lbu(m, block + offset);
        if low { value < 0x40 } else { value >= 0xc1 }
    };
    if reached(m, a) && reached(m, b) {
        sb(m, block + 0x68, stage + 1);
    }
    external(m, SHOW_TEXT, [help, leaked, 0, 0]);
    0
}

/// 800E4578 (group 5, step 0): open the calibration: stage +0x68 = 0; type
/// bits 1 or 4 (no analog axes) skip it (returns 2); else visual page
/// 0x21/0x19/0x1A by type and the four axes' max/min/... bytes reset
/// (0x88, 0x80, 0x10, 0xF0). Returns 1.
pub fn calibration_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let ctx = context(m);
    let block = keys(m);
    let types = m.lw(ctx + 0x124);
    sb(m, block + 0x68, 0);
    let result = if types & 5 != 0 {
        2
    } else {
        let page = match kbyte(m, 0x30) {
            0 => Some(0x21),
            1 => Some(0x19),
            3 => Some(0x1a),
            _ => None,
        };
        if let Some(page) = page {
            external(m, SELECT_VISUAL_PAGE, [page, 0, 0, 0]);
        }
        for k in 0..4 {
            for (offset, value) in [(0x58, 0x88), (0x5c, 0x80), (0x60, 0x10), (0x64, 0xf0)] {
                let block = keys(m);
                sb(m, block + k + offset, value);
            }
        }
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800E4688 (group 5, step 1): the calibration of the type (1: 800E339C,
/// else 800E3ADC): done → 2, left → 0x40, else 0.
pub fn calibration(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if kbyte(m, 0x30) == 1 {
        single_axis_calibration(m)
    } else {
        stick_calibration(m)
    };
    m.pop_frame(0x18);
    if result & 1 != 0 {
        2
    } else {
        (result << 5) & 0x40
    }
}

/// 800E46EC (group 6): store the settings (800DF39C) and leave (0x10).
pub fn store_and_leave(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    store_for_type(m);
    m.pop_frame(0x18);
    0x10
}

/// ov2core: start loading file `file` to `address` (800CE7E4) and poll it
/// (800CE844, 1 when done).
const LOAD_START: u32 = 0x800c_e7e4;
const LOAD_POLL: u32 = 0x800c_e844;

/// 800E470C (OPTIONS mode 1, step 0): reset the KEY CONFIG block (task
/// 0/0, flags, row 1), start loading the screen's file (work +0x200C0 to
/// 0x9000, kind 6), enter the scene (800E983C) and set the four layout
/// tables (0x800F60EC, 0x800F6218, 0x800F62E0, 0x800F63E4). Returns 2.
pub fn keys_open(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = keys(m);
    sh(m, block + 2, 0);
    sh(m, block, 0);
    m.sw(block + 0x10, 0);
    m.sw(block + 0xc, 0);
    m.sw(block + 8, 0);
    sh(m, block + 0x2e, 1);
    let file = work_word(m, 0xc0);
    external(m, LOAD_START, [file, 0x9000, 6, 0]);
    external(m, SCENE_ENTER, [0; 4]);
    let block = keys(m);
    m.sw(block + 0x48, 0x800f_60ec);
    m.sw(block + 0x4c, 0x800f_6218);
    m.sw(block + 0x50, 0x800f_62e0);
    m.sw(block + 0x54, 0x800f_63e4);
    m.pop_frame(0x18);
    2
}

/// 800E47AC (OPTIONS mode 1, step 1): 2 once the file is loaded, else 1.
pub fn keys_load(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let done = external(m, LOAD_POLL, [0; 4]);
    m.pop_frame(0x18);
    if done == 1 { 2 } else { 1 }
}

/// 800E47E0 (OPTIONS mode 1, step 2): the KEY CONFIG task machine (task
/// group +0, step +2, table 0x800F670C). From group 2 on, a pad change
/// (context +0x156 bits 0, 1, 3, or type bits other than +0x34) restarts
/// at 0/0. Task results: 1 next step, 2 next group, 0x80 previous group,
/// 0x10 leave (800E9824, returns 8), 4 restart, 0x20 group 5, 0x40 group
/// 2. Returns 1 otherwise.
pub fn keys_tick(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = keys_tick_body(m);
    m.pop_frame(0x18);
    result
}

fn keys_tick_body(m: &mut Machine) -> u32 {
    let block = keys(m);
    if (lh(m, block) as i32) >= 2 {
        let ctx = context(m);
        if lhu(m, ctx + 0x156) & 0xb != 0 {
            sh(m, block, 0);
            sh(m, block + 2, 0);
        }
        let block = keys(m);
        let ctx = context(m);
        if m.lw(block + 0x34) != m.lw(ctx + 0x124) {
            sh(m, block, 0);
            sh(m, block + 2, 0);
        }
    }
    let result = run_table(m, TASKS, 0);
    let block = keys(m);
    if result & 1 != 0 {
        let step = lhu(m, block + 2);
        sh(m, block + 2, step.wrapping_add(1));
    } else if result & 0x82 != 0 {
        let group = lhu(m, block);
        sh(m, block + 2, 0);
        let step = if result & 2 != 0 { 1 } else { u32::MAX };
        sh(m, block, group.wrapping_add(step));
    } else if result & 0x10 != 0 {
        external(m, SCENE_LEAVE, [0; 4]);
        return 8;
    } else if result & 0x64 != 0 {
        let group = if result & 4 != 0 {
            0
        } else if result & 0x20 != 0 {
            5
        } else {
            2
        };
        sh(m, block, group);
        sh(m, block + 2, 0);
    }
    1
}
