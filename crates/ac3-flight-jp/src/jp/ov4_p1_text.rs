//! JP overlay 4, part 1: the terminal of state 8 (800CDEFC..800CE9B3): a
//! 44x25 character screen in the block `[0x800D1544]`, its cursor, the
//! "purge" messages and the script that types them, translated from the JP
//! instructions.
//!
//! The block (0x464 bytes): +4 the 44x25 characters (one byte each, row
//! by row), +0x450 the cursor column, +0x451 its row, +0x452 the cursor
//! mode (0 none, 1 blinking block, 2 spinner), +0x453 the blink counter,
//! +0x454/+0x458 two 0x65F0-byte packet areas, +0x45C the next packet,
//! +0x460 the script delay, +0x462 the script position.
use super::ov3_p1::{add_prim, call, framed, lbu, lh, lhu, sb, sh, slt, sra};
use super::ov4_p1::{MODULE, terminal};
use crate::common::{CONTEXT, loaded_module};
use crate::machine::{Machine, SmallData};

/// Columns and rows of the screen.
const COLUMNS: u32 = 0x2c;
const ROWS: u32 = 0x19;

/// 8002DBE0 (resident, `script`): play sound effect `a0`.
const SOUND: u32 = 0x8002_dbe0;
/// 800ADD18 / 800ADD88: memset, strcat.
const MEMSET: u32 = 0x800a_dd18;
const STRCAT: u32 = 0x800a_dd88;
/// 8002C6A8 (resident, `shared`): reset the CD queue; 8002C8D8 (resident,
/// `script`): queue a stream request.
const RESET_CD_QUEUE: u32 = 0x8002_c6a8;
const QUEUE_REQUEST: u32 = 0x8002_c8d8;

/// 800CDEFC (JP; PAL 800CCC94, exact): clear the screen: cursor home, the
/// 0x113 words of characters zeroed. Returns 0.
pub fn clear(m: &mut Machine) -> u32 {
    let t = terminal(m);
    sb(m, t.wrapping_add(0x451), 0);
    sb(m, t.wrapping_add(0x450), 0);
    let t = terminal(m);
    for i in 0..0x113u32 {
        m.sw(t.wrapping_add(4 + 4 * i), 0);
    }
    0
}

/// 800CDF38 (JP, new): scroll the screen up a row (24 rows of 11 words
/// copied up, the last row cleared). Returns 0.
pub fn scroll(m: &mut Machine) -> u32 {
    let t = terminal(m);
    let mut to = t.wrapping_add(4);
    let mut from = t.wrapping_add(0x30);
    for _ in 0..0x108 {
        let word = m.lw(from);
        from = from.wrapping_add(4);
        m.sw(to, word);
        to = to.wrapping_add(4);
    }
    for _ in 0..0xb {
        m.sw(to, 0);
        to = to.wrapping_add(4);
    }
    0
}

/// 800CDF88 (JP; PAL 800DE2F4 in overlay 2, changed): a new line: column
/// 0, the next row (row 24 scrolls instead). Returns the new row, or 0
/// after a scroll.
pub fn newline(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let t = terminal(m);
        sb(m, t.wrapping_add(0x450), 0);
        let t = terminal(m);
        let row = lbu(m, t.wrapping_add(0x451));
        if row < 0x18 {
            sb(m, t.wrapping_add(0x451), row + 1);
            row + 1
        } else {
            scroll(m)
        }
    })
}

/// 800CDFDC (JP; PAL 800D0578 in overlay 1, changed): a carriage return
/// (column 0). Returns the block pointer.
pub fn carriage_return(m: &mut Machine) -> u32 {
    let t = terminal(m);
    sb(m, t.wrapping_add(0x450), 0);
    t
}

/// 800CDFEC (JP, new): move the cursor back a character (column 0: the
/// last column of the row before; at the home position the column stays
/// 0). Returns the new column or row, or -1 at the home position.
pub fn back(m: &mut Machine) -> u32 {
    let t = terminal(m);
    let column = lbu(m, t.wrapping_add(0x450));
    if column != 0 {
        let column = column.wrapping_sub(1);
        sb(m, t.wrapping_add(0x450), column);
        return column;
    }
    sb(m, t.wrapping_add(0x450), 0x2b);
    let t = terminal(m);
    let row = lbu(m, t.wrapping_add(0x451));
    let row = row.wrapping_sub(1);
    if row != u32::MAX {
        sb(m, t.wrapping_add(0x451), row);
    } else {
        sb(m, t.wrapping_add(0x450), 0);
    }
    row
}

/// 800CE040 (JP; PAL 800D1FF0 in overlay 1, changed): delete the
/// character before the cursor ([`back`], a space, [`back`]). Returns the
/// last [`back`]'s result.
pub fn delete(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        back(m);
        put_char(m, 0x20);
        back(m)
    })
}

/// 800CE070 (JP, new; no caller): place the cursor at column `x`, row
/// `y` (clamped to the screen). Returns the block pointer.
pub fn locate(m: &mut Machine, x: u32, y: u32) -> u32 {
    let mut x = if slt(x, 0) { 0 } else { x };
    if !slt(x, COLUMNS) {
        x = 0x2b;
    }
    let mut y = if slt(y, 0) { 0 } else { y };
    if !slt(y, ROWS) {
        y = 0x18;
    }
    let t = terminal(m);
    sb(m, t.wrapping_add(0x450), x);
    let t = terminal(m);
    sb(m, t.wrapping_add(0x451), y);
    t
}

/// The jump table 0x800CD65C of [`control`] (codes 1..13): its targets.
const CONTROL_DONE: u32 = 0x800c_e144;

/// 800CE0C0 (JP, new): a control code: 1 and 2 set the cursor mode, 8
/// deletes ([`delete`]), 10 is a new line, 13 a carriage return; the others
/// do nothing (returning the jump target 0x800CE144, the original's `v0`).
pub fn control(m: &mut Machine, code: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let index = code.wrapping_sub(1);
        if index >= 0xd {
            return 0;
        }
        match index {
            0 | 1 => {
                let t = terminal(m);
                sb(m, t.wrapping_add(0x452), index + 1);
                index + 1
            }
            7 => delete(m),
            9 => newline(m),
            12 => carriage_return(m),
            _ => CONTROL_DONE,
        }
    })
}

/// Store the printable character `ch` at the cursor and advance it (a new
/// line after the last column). Returns the original's `v0`: 1, or the
/// new line's result.
fn type_char(m: &mut Machine, ch: u32) -> u32 {
    let t = terminal(m);
    let row = lbu(m, t.wrapping_add(0x451));
    let column = lbu(m, t.wrapping_add(0x450));
    let at = row.wrapping_mul(COLUMNS).wrapping_add(column);
    sb(m, t.wrapping_add(at).wrapping_add(4), ch);
    let t = terminal(m);
    let column = lbu(m, t.wrapping_add(0x450)).wrapping_add(1);
    sb(m, t.wrapping_add(0x450), column);
    if (column & 0xff) < COLUMNS {
        1
    } else {
        newline(m)
    }
}

/// 800CE154 (JP, new): type the character `ch` (codes below 0x20 are
/// controls, [`control`]); a printable one restarts the blink counter.
/// Both play sound 0x3F, except the code 0. Returns the sound's result
/// (the control's result for the code 0).
pub fn put_char(m: &mut Machine, ch: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        if slt(ch, 0x20) {
            let v0 = control(m, ch);
            if ch == 0 {
                return v0;
            }
        } else {
            type_char(m, ch);
            let t = terminal(m);
            sb(m, t.wrapping_add(0x453), 0);
        }
        call(m, SOUND, [0x3f, 0, 0, 0])
    })
}

/// The loop of [`put_string`] (and of [`purge`]): type the bytes from
/// `text` up to the zero, controls through [`control`], without sound.
/// Returns the last character's result (`v0` when the string is empty:
/// the caller's, 0 here).
fn type_string(m: &mut Machine, text: u32, v0: u32) -> u32 {
    let mut v0 = v0;
    let mut at = text;
    let mut ch = lbu(m, at);
    at = at.wrapping_add(1);
    while ch != 0 {
        v0 = if slt(ch, 0x20) {
            control(m, ch)
        } else {
            type_char(m, ch)
        };
        ch = lbu(m, at);
        at = at.wrapping_add(1);
    }
    v0
}

/// 800CE218 (JP, new): type the string `text` (no sound). Returns the last
/// character's result (0 for an empty string, where the original returns
/// its caller's `v0`; no caller uses it).
pub fn put_string(m: &mut Machine, text: u32) -> u32 {
    framed(m, 0x20, |m, _| type_string(m, text, 0))
}

/// 0x800CD690: "\n"; 0x800D0FC4: the names of the subsystems (pointers,
/// a zero after the last); the suffixes by index range.
const NEWLINE: u32 = 0x800c_d690;
const NAMES: u32 = 0x800d_0fc4;
const ESPECIAL: u32 = 0x800c_d694;
const SPECIAL: u32 = 0x800c_d6a4;
const PARAMETER: u32 = 0x800c_d6b4;
const SUBSYSTEM: u32 = 0x800c_d6c0;
const PURGED: u32 = 0x800c_d6cc;

/// 800CE2E4 (JP, new): type "\n<name of subsystem `index`><suffix>
/// purged" (":especial data" below 10, ":special data" 10..82,
/// ":parameter" 83..103, ":subsystem" from 104), built in the frame.
/// Returns whether it was the last name.
pub fn purge(m: &mut Machine, index: u32) -> u32 {
    framed(m, 0x120, |m, sp| {
        let text = sp.wrapping_add(0x10);
        let first = m.lb(NEWLINE) as u8;
        let second = m.lb(NEWLINE + 1) as u8;
        m.sb(text, first);
        m.sb(text.wrapping_add(1), second);
        call(m, MEMSET, [sp.wrapping_add(0x12), 0, 0xfe, 0]);
        let name = m.lw((index << 2).wrapping_add(NAMES));
        call(m, STRCAT, [text, name, 0, 0]);
        if index < 0xa {
            call(m, STRCAT, [text, ESPECIAL, 0, 0]);
        }
        if index.wrapping_sub(0xa) < 0x49 {
            call(m, STRCAT, [text, SPECIAL, 0, 0]);
        }
        if index.wrapping_sub(0x53) < 0x15 {
            call(m, STRCAT, [text, PARAMETER, 0, 0]);
        }
        if !slt(index, 0x68) {
            call(m, STRCAT, [text, SUBSYSTEM, 0, 0]);
        }
        let v0 = call(m, STRCAT, [text, PURGED, 0, 0]);
        type_string(m, text, v0);
        let next = m.lw((index.wrapping_add(1) << 2).wrapping_add(NAMES));
        u32::from(next == 0)
    })
}

/// 800CE498 (JP, new; `entry` a script entry): type the character +3 of
/// the player's name (context +0x948) with sound 0x3F, unless it is a
/// control or the end. Returns the sound's result (1 otherwise).
pub fn name_char(m: &mut Machine, entry: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = m.gw(CONTEXT);
        let index = lbu(m, entry.wrapping_add(3));
        let ch = lbu(m, context.wrapping_add(index).wrapping_add(0x948));
        if ch == 0 || ch < 0x20 {
            return 1;
        }
        type_char(m, ch);
        let t = terminal(m);
        sb(m, t.wrapping_add(0x453), 0);
        call(m, SOUND, [0x3f, 0, 0, 0])
    })
}

/// 0x800CD6D4: the spinner characters `|`, `/`, `-`, `\`.
const SPINNER: u32 = 0x800c_d6d4;

/// 800CE554 (JP, new; `ot`, `packets`, `x`, `y`, the colour on the
/// stack): the screen as one 6x16 SPRT per character above 0x20 (glyphs
/// 16 a row at 0,0 of CLUT 0x1858, 25 rows of 44 from (x, y)), the cursor
/// (mode 1 a block blinking every 16 frames, mode 2 the spinner) and a
/// DR_TPAGE 6. Returns the next packet.
pub fn draw(m: &mut Machine, ot: u32, packets: u32, x: u32, y: u32) -> u32 {
    let color = m.lw(m.sp.wrapping_add(0x10));
    framed(m, 0x28, |m, sp| {
        let t = terminal(m);
        let mut chars = t.wrapping_add(4);
        let code = color | 0x6600_0000;
        let mut p = packets;
        let mut row_y = y;
        loop {
            let mut column_x = x;
            let end = x.wrapping_add(0x108);
            loop {
                let ch = lbu(m, chars);
                chars = chars.wrapping_add(1);
                if !slt(ch, 0x21) {
                    let glyph = ch.wrapping_sub(0x20);
                    let uv = ((glyph & 0xf) << 3) | (sra(glyph, 4) << 12) | 0x1858_0000;
                    m.sw(p.wrapping_add(4), code);
                    m.sw(p.wrapping_add(0xc), uv);
                    m.sw(p.wrapping_add(0x10), 0x0010_0006);
                    sh(m, p.wrapping_add(8), column_x);
                    sh(m, p.wrapping_add(0xa), row_y);
                    let tag = m.lw(ot) | 0x0400_0000;
                    m.sw(p, tag);
                    m.sw(ot, (p << 8) >> 8);
                    p = p.wrapping_add(0x14);
                }
                column_x = column_x.wrapping_add(6);
                if !slt(column_x, end) {
                    break;
                }
            }
            row_y = row_y.wrapping_add(0x10);
            if !slt(row_y, y.wrapping_add(0x190)) {
                break;
            }
        }
        let t = terminal(m);
        let blink = lbu(m, t.wrapping_add(0x453)).wrapping_add(1);
        sb(m, t.wrapping_add(0x453), blink);
        let t = terminal(m);
        if lbu(m, t.wrapping_add(0x452)) != 0 {
            let t = terminal(m);
            for i in 0..4u32 {
                let word = m.lw(SPINNER + 4 * i);
                m.sw(sp.wrapping_add(4 * i), word);
            }
            let cursor = match lbu(m, t.wrapping_add(0x452)) {
                1 => {
                    if lbu(m, t.wrapping_add(0x453)) & 0x10 == 0 {
                        0x7f
                    } else {
                        0x20
                    }
                }
                2 => {
                    let phase = (lbu(m, t.wrapping_add(0x453)) >> 1) & 0xc;
                    m.lw(sp.wrapping_add(phase))
                }
                _ => 0x20,
            };
            let t = terminal(m);
            let glyph = cursor.wrapping_sub(0x20);
            let row = lbu(m, t.wrapping_add(0x451));
            let column = lbu(m, t.wrapping_add(0x450));
            let cursor_y = y.wrapping_add(row << 4);
            let cursor_x = x.wrapping_add(column.wrapping_mul(6));
            if glyph != 0 {
                m.sw(p.wrapping_add(4), color | 0x6600_0000);
                let uv = ((glyph & 0xf) << 3) | (sra(glyph, 4) << 12) | 0x1858_0000;
                m.sw(p.wrapping_add(0xc), uv);
                sh(m, p.wrapping_add(8), cursor_x);
                sh(m, p.wrapping_add(0xa), cursor_y);
                m.sw(p.wrapping_add(0x10), 0x0010_0006);
                let tag = m.lw(ot) | 0x0400_0000;
                m.sw(p, tag);
                m.sw(ot, (p << 8) >> 8);
                p = p.wrapping_add(0x14);
            }
        }
        sb(m, p.wrapping_add(3), 1);
        m.sw(p.wrapping_add(4), 0xe100_0006);
        add_prim(m, ot, p);
        p.wrapping_add(8)
    })
}

/// 0x800D14F8: the stream request of the script's opcode 5.
const SCRIPT_STREAM: u32 = 0x800d_14f8;
/// `[0x800D1520]`: the next subsystem to purge.
const PURGE_INDEX: u32 = 0x800d_1520;

/// 800CE808 (JP, new; `script`, `strings`): one step of the terminal
/// script (4-byte entries: opcode, delay, character, string): after the
/// delay (+0x460) run the entry at +0x462: 0 the end (returns 1), 1 type a
/// character, 2 type string `strings[+3]`, 3 type a name character, 4
/// purge the next two subsystems (the entry repeats until the last name),
/// 5 reset the CD queue and request stream 0x800D14F8; then (unless it
/// repeats) the next entry, and its delay. Returns 0 otherwise.
pub fn script(m: &mut Machine, script: u32, strings: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let t = terminal(m);
        let delay = lh(m, t.wrapping_add(0x460));
        let left = lhu(m, t.wrapping_add(0x460)).wrapping_sub(1);
        if delay != 0 {
            sh(m, t.wrapping_add(0x460), left);
            if left << 16 != 0 {
                return 0;
            }
        }
        let t = terminal(m);
        let position = lh(m, t.wrapping_add(0x462));
        let mut entry = script.wrapping_add(position << 2);
        let opcode = lbu(m, entry);
        let advance = |m: &mut Machine, entry: &mut u32| {
            let t = terminal(m);
            let position = lhu(m, t.wrapping_add(0x462)).wrapping_add(1);
            *entry = entry.wrapping_add(4);
            sh(m, t.wrapping_add(0x462), position);
        };
        let opcode = if opcode < 6 {
            match opcode {
                0 => return 1,
                1 => {
                    let ch = lbu(m, entry.wrapping_add(2));
                    put_char(m, ch);
                }
                2 => {
                    let index = lbu(m, entry.wrapping_add(3));
                    let text = m.lw((index << 2).wrapping_add(strings));
                    put_string(m, text);
                }
                3 => {
                    name_char(m, entry);
                }
                4 => {
                    let index = m.lw(PURGE_INDEX);
                    let mut last = purge(m, index);
                    if last == 0 {
                        let index = m.lw(PURGE_INDEX).wrapping_add(1);
                        m.sw(PURGE_INDEX, index);
                        last = purge(m, index);
                        if last == 0 {
                            let index = m.lw(PURGE_INDEX).wrapping_add(1);
                            m.sw(PURGE_INDEX, index);
                        }
                    }
                    if last != 0 {
                        advance(m, &mut entry);
                    }
                }
                _ => {
                    call(m, RESET_CD_QUEUE, [0, 0, 0, 0]);
                    call(m, QUEUE_REQUEST, [SCRIPT_STREAM, 0, 0, 0]);
                }
            }
            lbu(m, entry)
        } else {
            opcode
        };
        if opcode != 4 {
            advance(m, &mut entry);
        }
        let t = terminal(m);
        let delay = lbu(m, entry.wrapping_add(1));
        sh(m, t.wrapping_add(0x460), delay);
        0
    })
}

/// The routines of this file by JP address, while module 4 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800c_defc => clear(m),
        0x800c_df38 => scroll(m),
        0x800c_df88 => newline(m),
        0x800c_dfdc => carriage_return(m),
        0x800c_dfec => back(m),
        0x800c_e040 => delete(m),
        0x800c_e070 => locate(m, a0, a1),
        0x800c_e0c0 => control(m, a0),
        0x800c_e154 => put_char(m, a0),
        0x800c_e218 => put_string(m, a0),
        0x800c_e2e4 => purge(m, a0),
        0x800c_e498 => name_char(m, a0),
        0x800c_e554 => draw(m, a0, a1, a2, a3),
        0x800c_e808 => script(m, a0, a1),
        _ => return None,
    })
}
