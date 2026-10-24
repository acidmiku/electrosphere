//! JP-only resident routines of part 3 of the resident wave (class `new` in
//! `jp_pal_map.tsv`, REVIEW.md §6, and the two near routines whose PAL
//! counterparts were never translated), translated from the JP
//! instructions and claimed in [`dispatch`]. Calls to other routines go
//! through `dispatch::call` by JP address unless a public translation is
//! named.
use crate::{
    arena,
    common::{CONTEXT, GAMEPLAY},
    dispatch, loading,
    machine::{Machine, SmallData},
    sdk,
};

/// `[gp+0x38]`: CD stream state (+0 flags: bit 31 open, 30 paused, 6
/// pause/resume request).
const CD_STREAM: u32 = 0x38;

fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    dispatch::call(m, target, args)
}

// ---- 80025FE8: shutdown ----------------------------------------------------------------

/// 80025FE8 (JP; PAL 80025B40, near, untranslated in PAL): shut the
/// machine down: DrawSync(0), 800A2338(0), 800A2F50, 80027064, 800AD510,
/// 800A7E18(3), 800A239C, then print "bye bye.." (80010000). JP drops PAL's
/// 8009E238 and ResetRCnt(0xF2000001) and adds the message.
pub fn shutdown_25fe8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    sdk::draw_sync(m, 0);
    call(m, 0x800a_2338, [0, 0, 0, 0]);
    call(m, 0x800a_2f50, [0, 0, 0, 0]);
    call(m, 0x8002_7064, [0, 0, 0, 0]);
    call(m, 0x800a_d510, [0, 0, 0, 0]);
    call(m, 0x800a_7e18, [3, 0, 0, 0]);
    call(m, 0x800a_239c, [0, 0, 0, 0]);
    let v0 = call(m, 0x800a_dfb8, [0x8001_0000, 0, 0, 0]);
    m.pop_frame(0x18);
    v0
}

// ---- CD stream -----------------------------------------------------------------------

/// 8002ABE0 (JP only; replaces PAL 8002A76C): with a stream open
/// (`[gp+0x38]` bit 31), request a pause (`pause` nonzero, unless already
/// paused: bit 30) or a resume (when paused) by setting request bit 0x40.
/// Unlike PAL, a resume no longer sets a restart time. Returns the previous
/// paused bit, or -1 without a stream.
pub fn stream_pause(m: &mut Machine, pause: u32) -> u32 {
    let stream = m.gw(CD_STREAM);
    let word = m.lw(stream);
    let paused = (word >> 30) & 1;
    if word & 0x8000_0000 == 0 {
        return u32::MAX;
    }
    let request = if pause != 0 {
        word & 0x4000_0000 == 0
    } else {
        word & 0x4000_0000 != 0
    };
    if request {
        m.sw(stream, word | 0x40);
    }
    paused
}

// ---- sprites ---------------------------------------------------------------------------

/// 80034360 (JP only; `x`, `y`, `slot`, `prim`, stack: `uv` at sp+0x10,
/// `grey` at sp+0x14): fill the SPRT `prim` at (`x`, `y`) with the grey
/// level (code 0x66), its uv/clut and size words from `uv[0..2]`, and link
/// it into ordering-table slot `slot` (tag = `[slot] | 0x04000000`, slot =
/// the packet's low 24 bits). Returns the next primitive (`prim + 0x14`).
pub fn sprite_34360(
    m: &mut Machine,
    x: u32,
    y: u32,
    slot: u32,
    prim: u32,
    uv: u32,
    grey: u32,
) -> u32 {
    m.sh(prim + 8, x as u16);
    m.sh(prim + 0xa, y as u16);
    let color = grey | (grey << 8) | (grey << 16) | 0x6600_0000;
    m.sw(prim + 4, color);
    let word = m.lw(uv);
    m.sw(prim + 0xc, word);
    let word = m.lw(uv.wrapping_add(4));
    m.sw(prim + 0x10, word);
    let tag = m.lw(slot) | 0x0400_0000;
    m.sw(prim, tag);
    m.sw(slot, (prim << 8) >> 8);
    prim.wrapping_add(0x14)
}

// ---- mission index -------------------------------------------------------------------

/// 80036130 (JP only): advance the context's mission index (byte +0x3A6)
/// modulo 37 and load its two bytes from the 4-byte records at 0x800B0BD0
/// (+2 into halfword +0x38C, +3 into halfword +0x38E). Returns the +3
/// byte.
pub fn next_mission_36130(m: &mut Machine) -> u32 {
    let context = m.gw(CONTEXT);
    let index = m.lbu(context + 0x3a6).wrapping_add(1);
    m.sb(context + 0x3a6, index);
    let context = m.gw(CONTEXT);
    let n = u32::from(m.lbu(context + 0x3a6));
    // multu 0xBACF914D; mfhi; (n - hi) >> 1; + hi; >> 5: n / 37
    let high = ((u64::from(n) * 0xbacf_914d) >> 32) as u32;
    let quotient = high.wrapping_add(n.wrapping_sub(high) >> 1) >> 5;
    let product = (quotient << 3)
        .wrapping_add(quotient)
        .wrapping_shl(2)
        .wrapping_add(quotient);
    m.sb(context + 0x3a6, n.wrapping_sub(product) as u8);
    let context = m.gw(CONTEXT);
    let index = u32::from(m.lbu(context + 0x3a6));
    let record = (index << 2).wrapping_add(0x800b_0bd0);
    let value = m.lbu(record + 2);
    m.sh(context + 0x38c, u16::from(value));
    let value = m.lbu(record + 3);
    m.sh(context + 0x38e, u16::from(value));
    u32::from(value)
}

/// 80038078 (JP only; `row`, `column`): context byte +0x3A8 = byte
/// `column + 2 * row` of the table at 0x800B0E94. Returns it.
pub fn select_38078(m: &mut Machine, row: u32, column: u32) -> u32 {
    let at = column.wrapping_add(row << 1).wrapping_add(0x800b_0e94);
    let context = m.gw(CONTEXT);
    let value = m.lbu(at);
    m.sb(context + 0x3a8, value);
    u32::from(value)
}

// ---- HUD labels ------------------------------------------------------------------------

/// 80040900 (JP only; `label`, `x`, `y`): when label `label`'s 4-byte
/// record at 0x800B1234 (u, v, w, h) has a width, append a green SPRT (0, 0x83,
/// 0) at (`x + 2`, `y`) with its uv and size to the sprite list of the
/// packet set `[gp+0x444]` (count +0x10, sprites at `[gp+0x448]` + 0x1008,
/// 20 bytes each). Returns the new count (0 for an empty label).
pub fn label_40900(m: &mut Machine, label: u32, x: u32, y: u32) -> u32 {
    let record = (label << 2).wrapping_add(0x800b_1234);
    let width = m.lbu(record + 2);
    if width == 0 {
        return 0;
    }
    let set = m.gw(0x444);
    let count = m.lw(set + 0x10);
    let offset = (count << 2)
        .wrapping_add(count)
        .wrapping_shl(2)
        .wrapping_add(0x1008);
    let sprite = m.gw(0x448).wrapping_add(offset);
    m.sb(sprite + 5, 0x83);
    m.sb(sprite + 4, 0);
    m.sb(sprite + 6, 0);
    m.sh(sprite + 8, x.wrapping_add(2) as u16);
    m.sh(sprite + 0xa, y as u16);
    let u = m.lbu(record);
    m.sb(sprite + 0xc, u);
    let v = m.lbu(record + 1);
    m.sb(sprite + 0xd, v);
    let w = m.lbu(record + 2);
    m.sh(sprite + 0x10, u16::from(w));
    let h = m.lbu(record + 3);
    let set = m.gw(0x444);
    m.sh(sprite + 0x12, u16::from(h));
    let count = m.lw(set + 0x10).wrapping_add(1);
    m.sw(set + 0x10, count);
    count
}

// ---- 80048A40 / 80048BDC ---------------------------------------------------------------

/// 80048A40 (JP only; arguments passed through): context byte +0x3C3 =
/// 80054DBC(a0..a3) (the context pointer is loaded before the call),
/// halfword +0x3BA = `[[gp+0x340]+0xC]`. Returns that halfword.
pub fn context_48a40(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let block = m.gw(CONTEXT).wrapping_add(0x3b8);
    let value = call(m, 0x8005_4dbc, args);
    m.sb(block + 0xb, value as u8);
    let record = m.gw(0x340);
    let half = m.lhu(record + 0xc);
    m.sh(block + 2, half);
    m.pop_frame(0x18);
    u32::from(half)
}

/// 80048BDC (JP only): reset the record at 0x800C7190: word +0 = 0,
/// halfword +4 = 1, +6 = -1, bytes +8/+9 = 0xFF, +0xA = 0. Returns -1.
pub fn reset_48bdc(m: &mut Machine) -> u32 {
    const RECORD: u32 = 0x800c_7190;
    m.sh(RECORD + 4, 1);
    m.sw(RECORD, 0);
    m.sb(RECORD + 9, 0xff);
    m.sh(RECORD + 6, 0xffff);
    m.sb(RECORD + 8, 0xff);
    m.sb(RECORD + 0xa, 0);
    u32::MAX
}

/// 8004925C (JP only; `side`, `unit`, `kind`): an availability bit of the
/// unit records at 0x800B17C4. Units 0x1F, 0x20, 0x21, 0x26, 0x27, 0x2F,
/// 0x30, 0x35 and 0x3A are records 0..8 (any other unit: 0); side 1 tests
/// bits 0x10000000/0x100000/0x1000/0x10 and side 4 bits 0x1000000/0x10000/
/// 0x100/1 for kinds 6..9 (other sides and kinds: 0).
pub fn unit_flag_4925c(m: &mut Machine, side: u32, unit: u32, kind: u32) -> u32 {
    if (unit as i32) < 0x1f {
        return 0;
    }
    let Some(record) = [0x1f, 0x20, 0x21, 0x26, 0x27, 0x2f, 0x30, 0x35, 0x3a]
        .iter()
        .position(|&u| u == unit)
    else {
        return 0;
    };
    let masks = match side {
        1 => [0x1000_0000, 0x0010_0000, 0x1000, 0x10],
        4 => [0x0100_0000, 0x0001_0000, 0x100, 1],
        _ => return 0,
    };
    let Some(&mask) = kind.checked_sub(6).and_then(|k| masks.get(k as usize)) else {
        return 0;
    };
    let word = m.lw(0x800b_17c4 + ((record as u32) << 2));
    word & mask
}

/// 80049438 (JP only): the per-frame radio cue of the record at 0x800C7190
/// (+4 state, +6 timer, +8 voice channel, +9 voice, +0xA portrait index).
/// State 1 with the timer running is cut (-1) when 8005650C is set or
/// 8006CB30 has any of bits 0x7000; state 2 ends once 8005650C is set
/// (8002DDA8(channel), 8004197C(0)) or the voice stopped (8002DD7C 0 or no
/// channel; 8004197C(1)): state 1, timer -1, channel 0xFF. A timer at 0 in
/// state 1 with gameplay flag 4 starts the voice (8002DB4C(+9)) and its
/// portrait (80041920(0x800B17E8[+0xA], 0x10C, 0x1E)): state 2, timer -1.
/// At 12 in state 1 without 8005650C it plays sound 0x35. A running timer
/// counts down. Returns the timer's last value (or the decremented one).
pub fn radio_cue_49438(m: &mut Machine) -> u32 {
    const CUE: u32 = 0x800c_7190;
    m.push_frame(0x18);
    if m.lh(CUE + 4) == 1 && m.lh(CUE + 6) >= 0 {
        let cut = call(m, 0x8005_650c, [0; 4]) != 0 || call(m, 0x8006_cb30, [0; 4]) & 0x7000 != 0;
        if cut {
            m.sh(CUE + 6, 0xffff);
        }
    }
    if m.lh(CUE + 4) == 2 {
        let ended = if call(m, 0x8005_650c, [0; 4]) != 0 {
            let channel = u32::from(m.lbu(CUE + 8));
            call(m, 0x8002_dda8, [channel, 0, 0, 0]);
            Some(0)
        } else {
            let channel = u32::from(m.lbu(CUE + 8));
            if call(m, 0x8002_dd7c, [channel, 0, 0, 0]) == 0 || m.lbu(CUE + 8) == 0xff {
                Some(1)
            } else {
                None
            }
        };
        if let Some(argument) = ended {
            call(m, 0x8004_197c, [argument, 0, 0, 0]);
            m.sh(CUE + 4, 1);
            m.sh(CUE + 6, 0xffff);
            m.sb(CUE + 8, 0xff);
        }
    }
    if m.lh(CUE + 6) == 0 {
        let gameplay = m.gw(crate::common::GAMEPLAY);
        if m.lw(gameplay + 4) & 4 != 0 && m.lh(CUE + 4) == 1 {
            let voice = u32::from(m.lbu(CUE + 9));
            let channel = call(m, 0x8002_db4c, [voice, 0, 0, 0]);
            m.sb(CUE + 8, channel as u8);
            let index = u32::from(m.lbu(CUE + 0xa));
            let portrait = u32::from(m.lbu(index.wrapping_add(0x800b_17e8)));
            call(m, 0x8004_1920, [portrait, 0x10c, 0x1e, 0]);
            m.sh(CUE + 4, 2);
            m.sh(CUE + 6, 0xffff);
        }
    }
    if m.lh(CUE + 6) == 0xc && m.lh(CUE + 4) == 1 && call(m, 0x8005_650c, [0; 4]) == 0 {
        call(m, 0x8002_db4c, [0x35, 0, 0, 0]);
    }
    let timer = m.lh(CUE + 6);
    let raw = u32::from(m.lhu(CUE + 6));
    let v0 = if timer < 0 {
        timer as i32 as u32
    } else {
        let next = raw.wrapping_sub(1);
        m.sh(CUE + 6, next as u16);
        next
    };
    m.pop_frame(0x18);
    v0
}

// ---- 80034930 / 80035184 / 8003545C: menu screens ----------------------------------

/// 800346C0 (`slot`, `prim`, `mode`): a draw-mode packet; returns the next
/// primitive.
fn draw_mode(m: &mut Machine, slot: u32, prim: u32, mode: u32) -> u32 {
    call(m, 0x8003_46c0, [slot, prim, mode, 0])
}

/// 800343C8 (`slot`, `prim`, `rect`, `kind`): a frame of the halfword
/// rectangle at `rect`; returns the next primitive.
fn menu_frame(m: &mut Machine, slot: u32, prim: u32, rect: u32, kind: u32) -> u32 {
    call(m, 0x8003_43c8, [slot, prim, rect, kind])
}

/// Copy the 8-byte rectangle at `from` to the frame local `to`
/// (`lwl`/`lwr`, `swl`/`swr` pairs: a byte copy).
fn copy_rect(m: &mut Machine, from: u32, to: u32) {
    for i in 0..8 {
        let b = m.lbu(from + i);
        m.sb(to + i, b);
    }
}

/// One menu item: the sprites `(dx, uv)` at (`x + dx`, `y + [row] + 5`)
/// with the grey level 0x60 when `selected` (else 0x40), then the draw
/// mode 0x35 when selected (else 0x55). `rect` is the menu rectangle (x,
/// y halfwords). Returns the next primitive.
fn menu_item(
    m: &mut Machine,
    slot: u32,
    mut prim: u32,
    rect: u32,
    row: u32,
    selected: bool,
    sprites: &[(u32, u32)],
) -> u32 {
    let grey = if selected { 0x60 } else { 0x40 };
    for &(dx, uv) in sprites {
        let x = m.lh(rect) as i32 as u32;
        let y = m.lh(rect + 2) as i32 as u32;
        let offset = m.lw(row);
        prim = sprite_34360(
            m,
            x.wrapping_add(dx),
            y.wrapping_add(offset.wrapping_add(5)),
            slot,
            prim,
            uv,
            grey,
        );
    }
    draw_mode(m, slot, prim, if selected { 0x35 } else { 0x55 })
}

/// The menu tail: the cursor frame at the selected row (the rectangle's y
/// moved by the row's halfword), the draw mode 0x55, the outer frame
/// `outer`, the title sprite `title` at (0x7D, `title_y`) at grey 0xC0 and
/// the draw mode 0x15.
fn menu_tail(
    m: &mut Machine,
    slot: u32,
    prim: u32,
    (rect, outer): (u32, u32),
    row: u32,
    (title, title_y): (u32, u32),
) -> u32 {
    let y = m.lhu(rect + 2);
    let step = m.lhu(row);
    m.sh(rect + 2, y.wrapping_add(step));
    let prim = menu_frame(m, slot, prim, rect, 1);
    let prim = draw_mode(m, slot, prim, 0x55);
    let prim = menu_frame(m, slot, prim, outer, 2);
    let prim = sprite_34360(m, 0x7d, title_y, slot, prim, title, 0xc0);
    draw_mode(m, slot, prim, 0x15)
}

/// 80034930 (JP only; `slot`, `prim`): the five-row menu (selected row
/// halfword gameplay +0x29C; row offsets 800B0AE0; the rectangles
/// 800BCBC8 and 800BCBC0 copied to the frame): row 0 one sprite, row 1
/// three (the third by context byte +0x39D), row 2 two (the second by
/// context byte +0x39E), row 3 three, row 4 two, then the cursor, frames
/// and title (800B0A58 at y 0x4A). Returns the next primitive.
pub fn menu_34930(m: &mut Machine, slot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x40);
    let outer = frame + 0x18;
    let rect = frame + 0x20;
    copy_rect(m, 0x800b_cbc0, outer);
    copy_rect(m, 0x800b_cbc8, rect);
    let game = m.gw(GAMEPLAY);
    let selection = game + 0x29c;
    let rows = 0x800b_0ae0;
    let is = |m: &mut Machine, row: i16| m.lh(selection) == row;
    let selected = is(m, 0);
    let prim = menu_item(m, slot, prim, rect, rows, selected, &[(6, 0x800b_0a60)]);
    let selected = is(m, 1);
    let ctx = m.gw(CONTEXT);
    let third = 0x800b_0a70 + (u32::from(m.lbu(ctx + 0x39d)) << 3);
    let sprites = [(6, 0x800b_0a98), (0x33, 0x800b_0a90), (0x71, third)];
    let prim = menu_item(m, slot, prim, rect, rows + 4, selected, &sprites);
    let selected = is(m, 2);
    let ctx = m.gw(CONTEXT);
    let second = if m.lbu(ctx + 0x39e) != 0 {
        0x800b_0a70
    } else {
        0x800b_0a78
    };
    let sprites = [(6, 0x800b_0a68), (0x72, second)];
    let prim = menu_item(m, slot, prim, rect, rows + 8, selected, &sprites);
    let selected = is(m, 3);
    let sprites = [(6, 0x800b_0aa0), (0x19, 0x800b_0aa8), (0x50, 0x800b_0a88)];
    let prim = menu_item(m, slot, prim, rect, rows + 0xc, selected, &sprites);
    let selected = is(m, 4);
    let sprites = [(6, 0x800b_0a80), (0x50, 0x800b_0a88)];
    let prim = menu_item(m, slot, prim, rect, rows + 0x10, selected, &sprites);
    let row = rows.wrapping_add((m.lh(selection) as i32 as u32) << 2);
    let prim = menu_tail(m, slot, prim, (rect, outer), row, (0x800b_0a58, 0x4a));
    m.pop_frame(0x40);
    prim
}

/// 80035184 (JP only; `slot`, `prim`): the two-row menu (selected row
/// halfword gameplay +0x29E; row offsets 800B0AF4; the rectangles
/// 800BCBD8 and 800BCBD0 copied to the frame), then the cursor, frames and
/// title (800B0AC8 at y 0x5A). Returns the next primitive.
pub fn menu_35184(m: &mut Machine, slot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x38);
    let outer = frame + 0x18;
    let rect = frame + 0x20;
    copy_rect(m, 0x800b_cbd0, outer);
    copy_rect(m, 0x800b_cbd8, rect);
    let game = m.gw(GAMEPLAY);
    let selection = game + 0x29e;
    let rows = 0x800b_0af4;
    let selected = m.lh(selection) == 0;
    let prim = menu_item(m, slot, prim, rect, rows, selected, &[(0x3a, 0x800b_0ab8)]);
    let selected = m.lh(selection) == 1;
    let prim = menu_item(
        m,
        slot,
        prim,
        rect,
        rows + 4,
        selected,
        &[(0x40, 0x800b_0ac0)],
    );
    let row = rows.wrapping_add((m.lh(selection) as i32 as u32) << 2);
    let prim = menu_tail(m, slot, prim, (rect, outer), row, (0x800b_0ac8, 0x5a));
    m.pop_frame(0x38);
    prim
}

/// 8003545C (JP only; `slot`, `prim`): the pulsing prompt: two sprites
/// (800B0AD0 at (0x10, 0x1C), 800B0AD8 at (0x2D, 0x1C)) at the grey level
/// `9 * ((sin(clock) >> 3) + 0x800) >> 8` of the context clock +0x28
/// (sine table 800104AC, 32 steps), between draw modes 0x35; from frame
/// 0x5B (`[gameplay+0xC]`) the text 80023610 fades in at (0x30, 0xD4)
/// through 8003563C (color `min(4 * (frame - 0x5A), 0x40) << 8`). Returns
/// the next primitive.
pub fn prompt_3545c(m: &mut Machine, slot: u32, prim: u32) -> u32 {
    let frame = m.push_frame(0x28);
    let ctx = m.gw(CONTEXT);
    let clock = m.lw(ctx + 0x28);
    let sine = m.lhu(0x8001_04ac + ((clock << 10) & 0x7c00));
    let level = (i32::from(sine as i16) >> 3).wrapping_add(0x800);
    let grey = (level.wrapping_mul(9) >> 8) as u32;
    let prim = draw_mode(m, slot, prim, 0x35);
    let prim = sprite_34360(m, 0x10, 0x1c, slot, prim, 0x800b_0ad0, grey);
    let prim = sprite_34360(m, 0x2d, 0x1c, slot, prim, 0x800b_0ad8, grey);
    let mut prim = draw_mode(m, slot, prim, 0x35);
    let game = m.gw(GAMEPLAY);
    let count = m.lw(game + 0xc) as i32;
    if count >= 0x5b {
        let fade = (count.wrapping_sub(0x5a) << 2).min(0x40) as u32;
        m.sw(frame + 0x10, fade << 8);
        m.sw(frame + 0x14, 0x8002_3610);
        prim = call(m, 0x8003_563c, [slot, prim, 0x30, 0xd4]);
    }
    m.pop_frame(0x28);
    prim
}

// ---- 80048C0C: radio cues --------------------------------------------------------------

/// 800C7190: the radio cue record (+0 flag word of the cues given, +4
/// halfword: cues enabled when 1, +6 halfword argument, +9 cue message
/// byte, +0xA byte argument).
const CUE: u32 = 0x800c_7190;
/// 800B1520: 11-byte cue tables by mission (+0..+4 the messages of cue
/// kinds 0..2, +0..+6 the kind-3 columns, +9/+0xA the values of kinds 4/5).
const CUE_TABLES: u32 = 0x800b_1520;
/// 800B1518: the kind-3 cue table column of each argument 0..6.
const CUE_COLUMNS: u32 = 0x800b_1518;
/// 800B17C0: the messages of the kind-3 cues by column.
const CUE_MESSAGES: u32 = 0x800b_17c0;

/// 80048C0C (JP only; `kind`, `arg`, `value`, `message`): give radio cue
/// `kind` once per mission when cues are on (gameplay +4 bit 2 and
/// 800C7194 == 1): its message byte, `arg` and the cue value go to 800C7190
/// +9, +6 and +0xA, and its bit to the flag word +0. Kinds 0..2 take two,
/// two and one messages of the mission's cue table (bits 0x10/0x20,
/// 0x40/0x80, 0x100; messages 0x68..0x6C, value the table byte); kind 3 a
/// table column by `value` 0..6 that must hold `value` (bit `1 << column`,
/// message from 800B17C0); kinds 4/5 a `value` equal to table +9/+0xA
/// (0x200, 0x400; 0x75, 0x76); kinds 6..9 a unit available by 8004925C
/// (`value`, mission, kind), `value` 1 or 4 (bits 0x800..0x40000, messages
/// 0x78..0x7F); kinds 0, 4..6 and 9 only on frames `& 7 == 0`, kinds 1..3
/// and 7 `& 3`, kind 8 `& 0xF`. Kinds from 10 store `message` and `value`
/// without a flag. Returns 1 when a cue is given, else 0.
pub fn radio_cue_48c0c(m: &mut Machine, kind: u32, arg: u32, value: u32, message: u32) -> u32 {
    m.push_frame(0x20);
    let v0 = radio_cue_body(m, kind, arg, value, message);
    m.pop_frame(0x20);
    v0
}

/// Body of [`radio_cue_48c0c`] in its frame.
fn radio_cue_body(m: &mut Machine, kind: u32, arg: u32, value: u32, message: u32) -> u32 {
    let game = m.gw(GAMEPLAY);
    let ctx = m.gw(CONTEXT);
    let enabled = m.lw(game + 4);
    let mission = m.lh(ctx + 0x38c) as i32 as u32;
    if enabled & 4 == 0 || m.lh(CUE + 4) != 1 {
        return 0;
    }
    let row = mission.wrapping_mul(11).wrapping_add(CUE_TABLES);
    let quiet = |m: &mut Machine, mask: u32| {
        let game = m.gw(GAMEPLAY);
        m.lw(game + 0xc) & mask != 0
    };
    // a cue of the flag word: `bit` clear, set it
    let take = |m: &mut Machine, bit: u32| {
        let flags = m.lw(CUE);
        if flags & bit != 0 {
            return false;
        }
        m.sw(CUE, flags | bit);
        true
    };
    let byte = value & 0xff;
    let given = match kind {
        0 | 1 => {
            if quiet(m, if kind == 0 { 7 } else { 3 }) {
                return 0;
            }
            let (first, second) = if kind == 0 {
                (0x10, 0x20)
            } else {
                (0x40, 0x80)
            };
            let flags = m.lw(CUE);
            let (bit, column, text) = if flags & first == 0 {
                (first, kind * 2, 0x68 + kind * 2)
            } else if flags & second == 0 {
                (second, kind * 2 + 1, 0x69 + kind * 2)
            } else {
                return 0;
            };
            let entry = u32::from(m.lbu(row + column));
            if entry == 0 {
                return 0;
            }
            m.sw(CUE, flags | bit);
            (text, entry)
        }
        2 => {
            if quiet(m, 3) || m.lw(CUE) & 0x100 != 0 {
                return 0;
            }
            let entry = u32::from(m.lbu(row + 4));
            if entry == 0 {
                return 0;
            }
            let flags = m.lw(CUE);
            m.sw(CUE, flags | 0x100);
            (0x6c, entry)
        }
        3 => {
            if quiet(m, 3) || byte >= 7 {
                return 0;
            }
            let column = u32::from(m.lbu(CUE_COLUMNS + byte));
            if byte != u32::from(m.lbu(column.wrapping_add(row))) || column == 0xff {
                return 0;
            }
            if !take(m, 1 << (column & 31)) {
                return 0;
            }
            (u32::from(m.lbu(CUE_MESSAGES + column)), value)
        }
        4 | 5 => {
            if quiet(m, 7) {
                return 0;
            }
            let (bit, column, text) = if kind == 4 {
                (0x200, 9, 0x75)
            } else {
                (0x400, 0xa, 0x76)
            };
            if m.lw(CUE) & bit != 0 || byte != u32::from(m.lbu(row + column)) {
                return 0;
            }
            take(m, bit);
            (text, value)
        }
        6..=9 => {
            let mask = [7, 3, 0xf, 7][(kind - 6) as usize];
            if quiet(m, mask) || unit_flag_4925c(m, byte, mission, kind) == 0 {
                return 0;
            }
            let shift = (kind - 6) * 2;
            let (bit, text) = match byte {
                1 => (0x800 << shift, 0x78 + shift),
                4 => (0x1000 << shift, 0x79 + shift),
                _ => return 0,
            };
            if !take(m, bit) {
                return 0;
            }
            (text, value)
        }
        _ => (message, value),
    };
    let (text, value) = given;
    m.sb(CUE + 9, text as u8);
    m.sh(CUE + 6, arg as u16);
    m.sb(CUE + 0xa, value as u8);
    1
}

// ---- 8004CEB0 / 8004D038: banners ------------------------------------------------------

/// `[gp+0x478]`: the banner block (+8: 32 glyph records of 8 bytes, +0
/// state, +1 glyph, +2 frame, +4/+6 x/y; +0x110 glyph count; +0x114 and
/// +0x118 the banner's byte and halfword; +0x11C current banner, 0xFF idle).
const BANNER: u32 = 0x478;
/// 800B1A10: 28-byte banner records (+0 flags: 1 sound, 2 stream; +1 byte;
/// +2 halfword; +4 line count; +8 lines of 8 bytes: x, y halfwords, text
/// pointer; +0x18 the sound id byte or stream index word).
const BANNERS: u32 = 0x800b_1a10;
/// 800B19B0: the 12-byte stream records of the banners.
const BANNER_STREAMS: u32 = 0x800b_19b0;
/// 800B1914: 4-byte glyph records by glyph index (+2 width).
const BANNER_GLYPHS: u32 = 0x800b_1914;

/// 8004CEB0 (JP only; `index`; replaces PAL 8004B760 in 80040A24): start
/// banner `index` unless it is 0xFF or the current one: `[gp+0x478]`
/// +0x11C = `index`, +0x114 = record byte +1, +0x118 = record halfword +2,
/// glyph count +0x110 = 0; flag 1 starts sound `[+0x18]` (8002F19C, 0x1E),
/// else flag 2 stops the stream (8002C71C(0)) and starts stream record
/// `[+0x18]` of 800B19B0 (8002C8D8); the record's lines become glyph
/// records (8004D038); the unused records up to 32 get state 0, the used
/// ones state 2.
pub fn banner_4ceb0(m: &mut Machine, index: u32) {
    m.push_frame(0x20);
    banner_body(m, index);
    m.pop_frame(0x20);
}

/// Body of [`banner_4ceb0`] in its frame.
fn banner_body(m: &mut Machine, index: u32) {
    if index == 0xff {
        return;
    }
    let block = m.gw(BANNER);
    if m.lw(block + 0x11c) == index {
        return;
    }
    let entry = index.wrapping_mul(28).wrapping_add(BANNERS);
    m.sw(block + 0x11c, index);
    let byte = u32::from(m.lbu(entry + 1));
    m.sw(block + 0x114, byte);
    let half = m.lh(entry + 2) as i32 as u32;
    m.sw(block + 0x110, 0);
    m.sw(block + 0x118, half);
    let flags = m.lbu(entry);
    if flags & 1 != 0 {
        let sound = u32::from(m.lbu(entry + 0x18));
        call(m, 0x8002_f19c, [sound, 0x1e, 0, 0]);
    } else if flags & 2 != 0 {
        call(m, 0x8002_c71c, [0, 0, 0, 0]);
        let stream = m.lw(entry + 0x18);
        let record = stream.wrapping_mul(12).wrapping_add(BANNER_STREAMS);
        call(m, 0x8002_c8d8, [record, 0, 0, 0]);
    }
    let mut line = 0_u32;
    while (line as i32) < m.lw(entry + 4) as i32 {
        text_4d038(m, entry + 8 + (line << 3));
        line += 1;
    }
    let block = m.gw(BANNER);
    let mut glyph = m.lw(block + 0x110);
    while (glyph as i32) < 0x20 {
        let block = m.gw(BANNER);
        m.sb(block.wrapping_add(glyph << 3).wrapping_add(8), 0);
        glyph = glyph.wrapping_add(1);
    }
    let mut glyph = 0_u32;
    loop {
        let block = m.gw(BANNER);
        if (glyph as i32) >= m.lw(block + 0x110) as i32 {
            break;
        }
        m.sb(block.wrapping_add(glyph << 3).wrapping_add(8), 2);
        glyph += 1;
    }
}

/// 8004D038 (JP only; `line`: x, y halfwords and a text pointer): append
/// the glyph records of one banner line to `[gp+0x478]` (state 0, glyph,
/// frame 0, x, y): each character's glyph (8004D124) advances x by its
/// width (800B1914 +2) - 1, a space (glyph 0x50) by 10; the line ends at
/// its zero byte or an unknown character.
pub fn text_4d038(m: &mut Machine, line: u32) {
    m.push_frame(0x28);
    let mut x = u32::from(m.lhu(line));
    let y = m.lhu(line + 2);
    let mut at = 0_u32;
    loop {
        let string = m.lw(line + 4);
        let c = u32::from(m.lbu(string.wrapping_add(at)));
        if c == 0 {
            break;
        }
        at += 1;
        let glyph = glyph_index_4d124(c);
        if glyph == 0x50 {
            x = x.wrapping_add(10);
            continue;
        }
        if glyph == u32::MAX {
            break;
        }
        let block = m.gw(BANNER);
        let count = m.lw(block + 0x110);
        m.sw(block + 0x110, count.wrapping_add(1));
        let record = block.wrapping_add((count << 3).wrapping_add(8));
        m.sb(record, 0);
        m.sb(record + 1, glyph as u8);
        m.sh(record + 2, 0);
        m.sh(record + 4, x as u16);
        m.sh(record + 6, y);
        let width = u32::from(m.lbu(BANNER_GLYPHS.wrapping_add(glyph << 2).wrapping_add(2)));
        x = width.wrapping_add(x).wrapping_sub(1);
    }
    m.pop_frame(0x28);
}

// ---- 8004D124: glyph index -------------------------------------------------------------

/// 8004D124 (JP only; `c`): the glyph index of character `c`: 'A'..'Z' 0..25,
/// '0'..'9' 0x1D..0x26, '.' 0x1A, '!' 0x1B, '?' 0x1C, ' ' 0x50, else -1.
pub fn glyph_index_4d124(c: u32) -> u32 {
    if c.wrapping_sub(0x41) & 0xff < 0x1a {
        return (c & 0xff).wrapping_sub(0x41);
    }
    let byte = c & 0xff;
    if c.wrapping_sub(0x30) & 0xff < 0xa {
        return byte.wrapping_sub(0x13);
    }
    match byte {
        0x2e => 0x1a,
        0x21 => 0x1b,
        0x3f => 0x1c,
        0x20 => 0x50,
        _ => u32::MAX,
    }
}

// ---- radio messages (gp+0x4A8/0x4B4/0x4BC) -------------------------------------------

/// `[gp+0x4BC]`: the 0x10-byte radio script block: +0 script pointer
/// (4-byte entries {time halfword, kind byte, value byte}, -1 time ends),
/// +4 time counter, +6 last voice, +7 its channel (0xFF: none), +8/+9
/// two bytes, +0xC portrait.
const SCRIPT: u32 = 0x4bc;
/// 800BCE8E: the radio portrait byte.
const PORTRAIT: u32 = 0x800b_ce8e;

/// The portrait call 80041920(portrait, 0x52 or 0x10C by context byte
/// +0x39C, 0x1E).
fn show_portrait(m: &mut Machine) {
    let context = m.gw(CONTEXT);
    let x = if m.lbu(context + 0x39c) == 1 {
        0x52
    } else {
        0x10c
    };
    let portrait = u32::from(m.lbu(PORTRAIT));
    call(m, 0x8004_1920, [portrait, x, 0x1e, 0]);
}

/// 800565E8 (JP only; `message`): gp+0x4B4 = 1, gp+0x4A8 = 2, queue
/// message `message` of the context's mission (80056A54(+0x38C, message)),
/// then 8002CC44(0, 0, 0). Returns its result.
pub fn start_message_565e8(m: &mut Machine, message: u32) -> u32 {
    m.push_frame(0x18);
    let context = m.gw(CONTEXT);
    m.set_gw(0x4b4, 1);
    let mission = m.lh(context + 0x38c) as i32 as u32;
    m.set_gw(0x4a8, 2);
    queue_message_56a54(m, mission, message);
    let v0 = call(m, 0x8002_cc44, [0, 0, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 80056634 (JP only): the message state gp+0x4A8: 2 shows the portrait
/// and returns 1; 1 clears it, 8002CC44(0xFF, 0, 0), in gameplay mode 4
/// 8005697C(`[[gp+0x340]+0x2C]`), 8004197C(1) and returns 0; otherwise 0.
pub fn message_state_56634(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = m.gw(0x4a8);
    let v0 = if state == 2 {
        show_portrait(m);
        1
    } else if state == 1 {
        m.set_gw(0x4a8, 0);
        call(m, 0x8002_cc44, [0xff, 0, 0, 0]);
        let gameplay = m.gw(crate::common::GAMEPLAY);
        if m.lw(gameplay + 4) == 4 {
            let record = m.gw(0x340);
            let value = u32::from(m.lbu(record + 0x2c));
            call(m, 0x8005_697c, [value, 0, 0, 0]);
        }
        call(m, 0x8004_197c, [1, 0, 0, 0]);
        0
    } else {
        0
    };
    m.pop_frame(0x18);
    v0
}

/// 800566F0 (JP only): allocate the radio script block `[gp+0x4BC]` (0x10
/// bytes) and reset it (8005671C(0)). Returns 8005671C's result.
pub fn init_script_566f0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let block = arena::alloc(m, 0x10);
    m.set_gw(SCRIPT, block);
    let v0 = reset_script_5671c(m, 0);
    m.pop_frame(0x18);
    v0
}

/// 8005671C (JP only; `script`): start radio script `script`: +6/+7 =
/// 0xFF, +0 = `script`, +4 = 0, +8/+9 = 0, gp+0x4B4 = 2, +0xC = 0; then
/// 8002CC44(0, 0, 0). Returns its result.
pub fn reset_script_5671c(m: &mut Machine, script: u32) -> u32 {
    m.push_frame(0x18);
    let block = m.gw(SCRIPT);
    m.sb(block + 7, 0xff);
    m.sb(block + 6, 0xff);
    m.sw(block, script);
    m.sh(block + 4, 0);
    let block = m.gw(SCRIPT);
    m.sb(block + 9, 0);
    m.sb(block + 8, 0);
    let block = m.gw(SCRIPT);
    m.set_gw(0x4b4, 2);
    m.sw(block + 0xc, 0);
    let v0 = call(m, 0x8002_cc44, [0, 0, 0, 0]);
    m.pop_frame(0x18);
    v0
}

/// 80056778 (JP only): step the radio script. Without a script, a voice
/// still playing on channel +7 is waited for; once it ended (8002DD7C 0)
/// 8002CC44(0xFF, 0, 0), +7 = 0xFF and 8004197C(1) (returns 0). With one,
/// every entry due at the time counter +4 runs: kind 0 plays voice `value`
/// (+6 = value, +7 = 8002DB4C(value)), kinds 1/2 set byte +8/+9, kind 4
/// shows portrait `value` (800BCE8E) and sets +0xC; the -1 entry ends the
/// script (+0 = 0). Then 800BCE94/800BCE95 = +8/+9, 800BCE96 = 0, and a
/// running script advances its time counter, ending when the counter
/// passes the current entry's time. Returns 1.
pub fn step_script_56778(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let v0 = step_script_body(m);
    m.pop_frame(0x20);
    v0
}

fn step_script_body(m: &mut Machine) -> u32 {
    let block = m.gw(SCRIPT);
    let mut entry = m.lw(block);
    if entry == 0 {
        let channel = u32::from(m.lbu(block + 7));
        if channel != 0xff && call(m, 0x8002_dd7c, [channel, 0, 0, 0]) == 0 {
            call(m, 0x8002_cc44, [0xff, 0, 0, 0]);
            let block = m.gw(SCRIPT);
            m.sb(block + 7, 0xff);
            call(m, 0x8004_197c, [1, 0, 0, 0]);
        }
        return 0;
    }
    if m.lh(entry) == m.lh(block + 4) {
        let mut value_at = entry.wrapping_add(3);
        loop {
            let kind = m.lbu(value_at.wrapping_sub(1));
            if kind == 0 {
                let block = m.gw(SCRIPT);
                let value = m.lbu(value_at);
                m.sb(block + 6, value);
                let value = u32::from(m.lbu(value_at));
                value_at = value_at.wrapping_add(4);
                let channel = call(m, 0x8002_db4c, [value, 0, 0, 0]);
                let block = m.gw(SCRIPT);
                m.sb(block + 7, channel as u8);
            } else {
                if kind.wrapping_sub(1) < 2 {
                    let block = m.gw(SCRIPT);
                    let kind = u32::from(m.lbu(value_at.wrapping_sub(1)));
                    let value = m.lbu(value_at);
                    m.sb(kind.wrapping_add(block) + 7, value);
                } else if kind == 4 {
                    let value = m.lbu(value_at);
                    m.sb(PORTRAIT, value);
                    show_portrait(m);
                    let block = m.gw(SCRIPT);
                    let value = u32::from(m.lbu(value_at));
                    m.sw(block + 0xc, value);
                }
                value_at = value_at.wrapping_add(4);
            }
            let block = m.gw(SCRIPT);
            entry = entry.wrapping_add(4);
            m.sw(block, entry);
            let time = m.lh(entry);
            if time == -1 {
                m.sw(block, 0);
                break;
            }
            if time != m.lh(block + 4) {
                break;
            }
        }
    }
    let block = m.gw(SCRIPT);
    let value = m.lbu(block + 8);
    m.sb(0x800b_ce94, value);
    let value = m.lbu(block + 9);
    m.sb(0x800b_ce96, 0);
    m.sb(0x800b_ce95, value);
    if m.lw(block) == 0 {
        return 1;
    }
    let time = m.lhu(block + 4).wrapping_add(1);
    m.sh(block + 4, time);
    if i32::from(m.lh(entry)) < i32::from(time as i16) {
        m.sw(block, 0);
    }
    1
}

/// 80056A54 (JP only; `mission`, `message`): 8002C71C(0); unless `message`
/// is 0xFF, queue the 12-byte message record `message` of mission
/// `mission` (table 0x800B29F4) with 8002C8D8(record, 1, 80056A44).
/// Returns `message`.
pub fn queue_message_56a54(m: &mut Machine, mission: u32, message: u32) -> u32 {
    m.push_frame(0x20);
    call(m, 0x8002_c71c, [0, 0, 0, 0]);
    if message != 0xff {
        let table = m.lw((mission << 2).wrapping_add(0x800b_29f4));
        let record = table.wrapping_add((message << 1).wrapping_add(message) << 2);
        call(m, 0x8002_c8d8, [record, 1, 0x8005_6a44, 0]);
    }
    m.pop_frame(0x20);
    message
}

/// 80056AC8 (JP; PAL 800551CC, near, untranslated in PAL; `rect`): copy
/// the RECT of the block `[gp+0x4D4]` to `rect`: x, y from +0x18/+0x1A (or
/// +0x20/+0x22 when byte +2 is 0), w, h from +0x1C/+0x1E. Returns 1.
pub fn rect_56ac8(m: &mut Machine, rect: u32) -> u32 {
    let block = m.gw(0x4d4);
    let first = u32::from(m.lbu(block + 2) == 0) << 3;
    let at = block.wrapping_add(first);
    let x = m.lhu(at + 0x18);
    m.sh(rect, x);
    let y = m.lhu(at + 0x1a);
    m.sh(rect + 2, y);
    let w = m.lhu(block + 0x1c);
    m.sh(rect + 4, w);
    let h = m.lhu(block + 0x1e);
    m.sh(rect + 6, h);
    1
}

// ---- 8005EBB8: fade palettes -----------------------------------------------------------

/// 800BCDCC: the 1,024-color source rectangle of [`palettes_5ebb8`].
const PALETTE_SOURCE: u32 = 0x800b_cdcc;
/// 8002456C: "StoreImage is -1".
const STORE_IMAGE_FAILED: u32 = 0x8002_456c;

/// 8005EBB8 (JP only; replaces PAL 8005D490; `far`, `patches`, `count`):
/// read the 1,024-color source rectangle (800BCDCC) from VRAM, interpolate
/// it toward the far color at `far` for 16 angles (rsin of 0, 64, ... 960)
/// and upload each: even steps as a 16x64 palette at x 320..368, y
/// 384/448 (as PAL), odd steps (JP only) as four 16x16 blocks from x 0 by
/// 64, y 240 (then 496); then patch `count` 12-byte records at `patches`
/// (+6 = source color at index +0). The 0x1050-byte frame holds the RECTs
/// (+0x1018, +0x1020), the source (+0x18) and the interpolated colors
/// (+0x818).
pub fn palettes_5ebb8(m: &mut Machine, far: u32, patches: u32, count: u32) {
    let sp = m.push_frame(0x1050);
    let rect = sp + 0x1018;
    let source = sp + 0x18;
    let output = sp + 0x818;
    let upload = sp + 0x1020;
    for i in 0..8 {
        let byte = m.lbu(PALETTE_SOURCE + i);
        m.sb(rect + i, byte);
    }
    sdk::draw_sync(m, 0);
    if sdk::store_image(m, rect, source) == u32::MAX {
        sdk::printf(m, [STORE_IMAGE_FAILED, 0, 0, 0]);
    }
    sdk::draw_sync(m, 0);
    let (mut x, mut y) = (0x140u32, 0x180u32);
    let (mut block_x, mut block_y) = (0u32, 0xf0u32);
    for step in 0..16u32 {
        let level = sdk::rsin(m, step << 6);
        m.sw(sp + 0x10, level);
        loading::interpolate_palette(m, output, source, 0x400, far, level);
        if step & 1 != 0 {
            m.sh(upload, block_x as u16);
            block_x = block_x.wrapping_add(0x40);
            m.sh(upload + 2, block_y as u16);
            m.sh(upload + 4, 0x10);
            m.sh(upload + 6, 0x10);
            if (block_x as i32) >= 0x100 {
                block_x = 0;
                block_y = 0x1f0;
            }
            sdk::load_image(m, upload, output);
            for part in 1..4 {
                let left = m.lhu(upload).wrapping_add(0x10);
                m.sh(upload, left);
                sdk::load_image(m, upload, output + part * 0x200);
            }
        } else {
            m.sh(upload, x as u16);
            x = x.wrapping_add(0x10);
            m.sh(upload + 6, 0x40);
            m.sh(upload + 2, y as u16);
            m.sh(upload + 4, 0x10);
            if (x as i32) >= 0x180 {
                x = 0x140;
                y = y.wrapping_add(0x40);
            }
            sdk::load_image(m, upload, output);
        }
        sdk::draw_sync(m, 0);
    }
    let mut record = patches;
    let mut index = 0i32;
    while index < count as i32 {
        let color_index = m.lh(record) as i32 as u32;
        let color = m.lhu(source.wrapping_add(color_index << 1));
        index += 1;
        m.sh(record + 6, color);
        record = record.wrapping_add(0xc);
    }
    m.pop_frame(0x1050);
}

/// The routines of this file by JP address (a0..a3 in, v0 out; `None`: not
/// claimed). 80034360 reads its fifth and sixth arguments from the
/// caller's frame (`sp + 0x10`, `sp + 0x14`).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8002_5fe8 => shutdown_25fe8(m),
        0x8002_abe0 => stream_pause(m, a0),
        0x8003_4360 => {
            let uv = m.lw(m.sp.wrapping_add(0x10));
            let grey = m.lw(m.sp.wrapping_add(0x14));
            sprite_34360(m, a0, a1, a2, a3, uv, grey)
        }
        0x8003_4930 => menu_34930(m, a0, a1),
        0x8003_5184 => menu_35184(m, a0, a1),
        0x8003_545c => prompt_3545c(m, a0, a1),
        0x8003_6130 => next_mission_36130(m),
        0x8003_8078 => select_38078(m, a0, a1),
        0x8004_0900 => label_40900(m, a0, a1, a2),
        0x8004_8a40 => context_48a40(m, args),
        0x8004_8bdc => reset_48bdc(m),
        0x8004_8c0c => radio_cue_48c0c(m, a0, a1, a2, a3),
        0x8004_925c => unit_flag_4925c(m, a0, a1, a2),
        0x8004_9438 => radio_cue_49438(m),
        0x8004_ceb0 => {
            banner_4ceb0(m, a0);
            0
        }
        0x8004_d038 => {
            text_4d038(m, a0);
            0
        }
        0x8004_d124 => glyph_index_4d124(a0),
        0x8005_65e8 => start_message_565e8(m, a0),
        0x8005_6634 => message_state_56634(m),
        0x8005_66f0 => init_script_566f0(m),
        0x8005_671c => reset_script_5671c(m, a0),
        0x8005_6778 => step_script_56778(m),
        0x8005_6a54 => queue_message_56a54(m, a0, a1),
        0x8005_6ac8 => rect_56ac8(m, a0),
        0x8005_ebb8 => {
            palettes_5ebb8(m, a0, a1, a2);
            0
        }
        _ => return None,
    })
}
