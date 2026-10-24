//! Overlay 2 visual block and visual-script interpreter (`800F1674`) with
//! its 28 opcode handlers (table 0x800F695C), translated from the original
//! instructions.
//!
//! The visual block (`[0x800F9E14]`, see [`VISUAL`]) holds style bytes,
//! RGB records, offset pairs, animation records and a script table bound
//! from a resource (800F1A9C). [`draw`] (800F1BB8) resets the interpreter
//! scratch state at 0x1F800000 (`[0x800F9E10]`), runs the page's script
//! into the visual ordering table and appends two background-colored
//! border tiles.
//!
//! Script instructions are words: opcode, then operands; the length in words
//! comes from table 0x800F68D0 (one byte per 4-byte entry) copied to scratch
//! +0x44, plus the extra words a handler leaves at scratch +0x20. Opcodes
//! flagged in 0x800F6940 (scratch +0x60) run only while the scratch
//! condition (+0x10) equals the selector (+0xE).
//!
//! Scratch layout: +4/+6 origin, +8/+0xA animation origin, +0xC/+0xE
//! style index / selector, +0x10 condition, +0x14 RGB record, +0x18 offset
//! pair, +0x1C semi-transparency (-1 off), +0x20 extra words, +0x28 copy of
//! the selected animation record, +0x38 rectangle, +0x40 RGB, +0x44 lengths,
//! +0x60 condition flags.
use super::{
    VISUAL, VISUAL_SCRATCH, add_prim, call, framed, link_word, load_unaligned, sext16,
    store_unaligned, visual_block,
};
use crate::{
    arena,
    machine::{Fault, Machine, SmallData},
    sdk,
};

/// Opcode handlers, indexed by opcode (0 ends a script).
pub const HANDLERS: u32 = 0x800f_695c;
/// Instruction lengths in words, one byte per 4-byte entry (28 opcodes).
pub const LENGTHS: u32 = 0x800f_68d0;
/// Condition flags per opcode (28 bytes).
pub const CONDITIONAL: u32 = 0x800f_6940;
/// Interpreter scratch state.
pub const SCRATCH_STATE: u32 = 0x1f80_0000;

fn scratch(m: &mut Machine) -> u32 {
    m.lw(VISUAL_SCRATCH)
}

/// Signed `div` quotient behind the compiler's `break 7` (zero divisor, at
/// `zero_break`) and `break 6` (`0x80000000 / -1`, 0x18 bytes later).
pub(crate) fn quotient(m: &mut Machine, n: i32, d: i32, zero_break: u32) -> i32 {
    if d == 0 {
        m.raise(Fault::Trap {
            address: zero_break,
            code: 7,
        });
        return if n < 0 { 1 } else { -1 };
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: zero_break + 0x18,
            code: 6,
        });
        return i32::MIN;
    }
    n.wrapping_div(d)
}

/// Remainder of a signed division by a power of two as compiled
/// (`bgez`/`addiu` bias, `sra`/`sll`, `subu`): truncated toward zero.
fn remainder(value: i32, bits: u32) -> i32 {
    let biased = if value < 0 {
        value.wrapping_add((1 << bits) - 1)
    } else {
        value
    };
    value.wrapping_sub((biased >> bits) << bits)
}

// ---- 800F1770..800F1BA0: visual block accessors --------------------------------------------

/// 800F1770: style byte `index` (+0x4A4). Returns the byte's address minus
/// 0x4A4 (the original v0).
pub fn set_style(m: &mut Machine, index: u32, value: u32) -> u32 {
    let at = visual_block(m).wrapping_add(index);
    m.sb(at + 0x4a4, value as u8);
    at
}
/// 800F18EC: offset pair `index` (+0x298/+0x29A). Returns block +
/// index*4.
pub fn set_offset(m: &mut Machine, index: u32, x: u32, y: u32) -> u32 {
    let at = visual_block(m).wrapping_add(index << 2);
    m.sh(at + 0x298, x as u16);
    m.sh(at + 0x29a, y as u16);
    at
}
/// 800F1908: read offset pair `index` into the halfwords at `x` and `y`.
/// Returns the y halfword (zero-extended).
pub fn get_offset(m: &mut Machine, index: u32, x: u32, y: u32) -> u32 {
    let at = visual_block(m).wrapping_add(index << 2);
    let value = m.lhu(at + 0x298);
    m.sh(x, value);
    let at = visual_block(m).wrapping_add(index << 2);
    let value = m.lhu(at + 0x29a);
    m.sh(y, value);
    u32::from(value)
}
/// 800F193C: animation record `index` +0xE (the time halfword, +0x326).
/// Returns block + index*16.
pub fn set_parameter(m: &mut Machine, index: u32, value: u32) -> u32 {
    let at = visual_block(m).wrapping_add(index << 4);
    m.sh(at + 0x326, value as u16);
    at
}
/// 800F196C: the page (script index, +0x4A0). Returns the visual block.
pub fn select_page(m: &mut Machine, page: u32) -> u32 {
    let block = visual_block(m);
    m.sw(block + 0x4a0, page);
    block
}

/// 800F197C: allocate the 0x528-byte visual block and reset it: flags and
/// buffer 0, 128 style bytes 0, 128 RGB records 0x808080, 32 offset pairs 0,
/// selectors 0, the 16 animation records {index, 0...} and the 16 8-byte
/// records {index, 0, 0, 0}.
pub fn initialize(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = arena::alloc(m, 0x528);
        m.sw(VISUAL, block);
        m.sw(block + 0x90, 0);
        m.sw(block, 0);
        for index in 0..0x80u32 {
            let block = visual_block(m);
            m.sb(block.wrapping_add(index) + 0x4a4, 0);
        }
        for index in 0..0x80u32 {
            let at = visual_block(m).wrapping_add(index << 2);
            m.sb(at + 0x9a, 0x80);
            m.sb(at + 0x99, 0x80);
            m.sb(at + 0x98, 0x80);
        }
        let mut at = visual_block(m);
        for _ in 0..0x20 {
            m.sh(at + 0x29a, 0);
            m.sh(at + 0x298, 0);
            at = at.wrapping_add(4);
        }
        let mut at = visual_block(m);
        m.sw(at + 0x498, 0);
        for index in 0..0x10u16 {
            m.sh(at + 0x318, index);
            for offset in [0x326, 0x324, 0x322, 0x320, 0x31e, 0x31c, 0x31a] {
                m.sh(at + offset, 0);
            }
            at = at.wrapping_add(0x10);
        }
        let mut at = visual_block(m);
        m.sw(at + 0x49c, 0);
        for index in 0..0x10u16 {
            m.sh(at + 0x418, index);
            m.sh(at + 0x41e, 0);
            m.sh(at + 0x41c, 0);
            m.sh(at + 0x41a, 0);
            at = at.wrapping_add(8);
        }
        0
    })
}

/// 800F1A9C: bind the script table of resource data `scripts` (resident
/// 80026E98) and set flag bit 0, or unbind (null table, bit 0 clear) for 0.
/// Returns the table (80026E98's v0), or the new flags when unbinding.
pub fn bind_scripts(m: &mut Machine, scripts: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        if scripts == 0 {
            let block = visual_block(m);
            let flags = m.lw(block) & !1;
            m.sw(block + 0x524, 0);
            m.sw(block, flags);
            flags
        } else {
            let table = call(m, 0x8002_6e98, [scripts, 0, 0, 0]);
            let block = visual_block(m);
            let flags = m.lw(block);
            m.sw(block + 0x524, table);
            m.sw(block, flags | 1);
            table
        }
    })
}

/// 800F1B00: select the visual ordering table of the display buffer
/// (+0x10 + buffer*0x40, buffer stored at +0x90), clear it (16 entries)
/// and chain it under the entry at `entry` (entry 15 first). Returns the
/// link to entry 15 (`(ot + 0x3C) & 0xFFFFFF`).
pub fn prepare_ot(m: &mut Machine, entry: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let ctx = super::context(m);
        let block = visual_block(m);
        let buffer = m.lw(ctx + 4);
        m.sw(block + 0x90, buffer);
        let ot = block.wrapping_add((buffer << 6).wrapping_add(0x10));
        m.sw(block + 8, ot);
        sdk::clear_otag_r(m, ot, 0x10);
        let block = visual_block(m);
        let ot = m.lw(block + 8);
        let link = m.lw(entry);
        link_word(m, ot, link);
        let ot = m.lw(block + 8);
        link_word(m, entry, ot.wrapping_add(0x3c));
        ot.wrapping_add(0x3c) & 0x00ff_ffff
    })
}

/// 800F1BA0: the current visual ordering table.
pub fn current_ot(m: &mut Machine) -> u32 {
    let block = visual_block(m);
    m.lw(block + 8)
}

/// 800F1BB8: draw the visual layer into `packet`: reset the scratch state,
/// copy the length and condition tables, run the page script into
/// ordering-table entry 1, then add two 320x4 tiles in the display
/// background color at y = -2 and y = 0x1FF into entry 15. Returns the
/// packet pointer.
pub fn draw(m: &mut Machine, packet: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = visual_block(m);
        m.sw(VISUAL_SCRATCH, SCRATCH_STATE);
        m.sw(block + 4, packet);
        let state = SCRATCH_STATE;
        m.sw(state, 0);
        m.sh(state + 6, 0);
        m.sh(state + 4, 0);
        m.sh(state + 0xe, 0);
        m.sh(state + 0xc, 0);
        m.sw(state + 0x1c, 0xffff_ffff);
        m.sw(state + 0x10, 0);
        m.sw(state + 0x14, 0);
        m.sw(state + 0x18, 0);
        for opcode in 0..0x1cu32 {
            let length = m.lbu(LENGTHS + opcode * 4);
            let state = scratch(m);
            m.sb(state.wrapping_add(opcode) + 0x44, length);
            let flag = m.lbu(CONDITIONAL + opcode);
            let state = scratch(m);
            m.sb(state.wrapping_add(opcode) + 0x60, flag);
        }
        let block = visual_block(m);
        let ot = m.lw(block + 8);
        let page = m.lw(block + 0x4a0);
        m.sw(block + 0xc, ot.wrapping_add(4));
        interpret(m, page);
        for y in [0xfffe_u16, 0x1ff] {
            let block = visual_block(m);
            let tile = m.lw(block + 4);
            m.sw(block + 4, tile.wrapping_add(0x10));
            m.sb(tile + 3, 3);
            m.sb(tile + 7, 0x60);
            let display = m.gw(super::DISPLAY);
            let red = m.lbu(display + 0x10);
            m.sb(tile + 4, red);
            let display = m.gw(super::DISPLAY);
            let green = m.lbu(display + 0x11);
            m.sb(tile + 5, green);
            let display = m.gw(super::DISPLAY);
            let blue = m.lbu(display + 0x12);
            m.sb(tile + 6, blue);
            m.sh(tile + 8, 0);
            m.sh(tile + 0xa, y);
            m.sh(tile + 0xc, 0x140);
            m.sh(tile + 0xe, 4);
            let block = visual_block(m);
            let ot = m.lw(block + 8);
            let link = m.lw(ot + 0x3c);
            link_word(m, tile, link);
            let ot = m.lw(block + 8);
            link_word(m, ot + 0x3c, tile);
        }
        let block = visual_block(m);
        m.lw(block + 4)
    })
}

// ---- 800F1674: interpreter ----------------------------------------------------------------

/// 800F1674: run script `index` of the bound table (no-op while unbound).
/// Each instruction dispatches through [`HANDLERS`] (jalr, a0 = the
/// instruction) unless its opcode is conditional and the scratch condition
/// differs from the selector; opcode 0 ends the script.
pub fn interpret(m: &mut Machine, index: u32) {
    framed(m, 0x20, |m, _| {
        let block = visual_block(m);
        if m.lw(block) & 1 == 0 {
            return;
        }
        let table = m.lw(block + 0x524);
        let mut at = m.lw(table.wrapping_add(index << 2));
        loop {
            let state = scratch(m);
            let opcode = m.lw(at);
            let entry = state.wrapping_add(opcode);
            if m.lbu(entry.wrapping_add(0x60)) != 0 {
                let selector = sext16(m.lh(state + 0xe));
                if m.lw(state + 0x10) != selector {
                    let length = u32::from(m.lbu(entry.wrapping_add(0x44)));
                    at = at.wrapping_add(length << 2);
                    continue;
                }
            }
            let state = scratch(m);
            m.sh(state + 0x20, 0);
            let opcode = m.lw(at);
            if opcode == 0 {
                return;
            }
            let handler = m.lw(HANDLERS.wrapping_add(opcode << 2));
            call(m, handler, [at, 0, 0, 0]);
            let state = scratch(m);
            let opcode = m.lw(at);
            let length = u32::from(m.lbu(state.wrapping_add(opcode).wrapping_add(0x44)));
            let extra = sext16(m.lh(state + 0x20));
            at = at.wrapping_add(length.wrapping_add(extra) << 2);
        }
    })
}

// ---- shared pieces of the handlers ---------------------------------------------------------

/// Copy the selected animation record (`+0x318 + [+0x498]*16`) to `to`
/// with the compiled unaligned word copies (four loads, then four stores).
fn copy_record(m: &mut Machine, to: u32) {
    let block = visual_block(m);
    let selected = m.lw(block + 0x498);
    let record = block.wrapping_add(selected << 4).wrapping_add(0x318);
    let words = [0, 4, 8, 0xc].map(|offset| load_unaligned(m, record + offset));
    for (offset, word) in [0, 4, 8, 0xc].into_iter().zip(words) {
        store_unaligned(m, to + offset, word);
    }
}
/// The animation gate of opcodes 7, 17, 18, 19 and 26: an animated record
/// (+2 nonzero) hides the primitive while its time (+0xE) is before its end
/// (+0xC).
fn record_hidden(m: &mut Machine, record: u32) -> bool {
    m.lh(record + 2) != 0 && m.lh(record + 0xe) < m.lh(record + 0xc)
}
/// Copy RGB record `[scratch+0x14]` to scratch +0x40 (unaligned copy).
fn copy_color(m: &mut Machine, block: u32, state: u32) {
    let index = m.lw(state + 0x14);
    let word = load_unaligned(m, block.wrapping_add(index << 2).wrapping_add(0x98));
    store_unaligned(m, state + 0x40, word);
}
/// The packed scratch RGB (+0x40..+0x42) as a primitive color word.
fn color_word(m: &mut Machine, state: u32) -> u32 {
    let red = u32::from(m.lbu(state + 0x40));
    let green = u32::from(m.lbu(state + 0x41));
    let blue = u32::from(m.lbu(state + 0x42));
    red | (green << 8) | (blue << 16)
}
/// Offset pair `[scratch+0x18]` plus the scratch origin, as the 16-bit sums
/// the handlers keep in registers.
fn origin(m: &mut Machine, block: u32, state: u32) -> (u32, u32) {
    let index = m.lw(state + 0x18);
    let pair = block.wrapping_add(index << 2);
    let x = u32::from(m.lhu(pair + 0x298)).wrapping_add(u32::from(m.lhu(state + 4)));
    let y = u32::from(m.lhu(pair + 0x29a)).wrapping_add(u32::from(m.lhu(state + 6)));
    (x, y)
}
/// Scratch +0x1C (semi-transparency mode) is not -1.
fn semi_transparent(m: &mut Machine) -> bool {
    let state = scratch(m);
    m.lw(state + 0x1c) != 0xffff_ffff
}
/// Reserve `bytes` of the visual packet area.
fn reserve(m: &mut Machine, bytes: u32) -> u32 {
    let block = visual_block(m);
    let prim = m.lw(block + 4);
    m.sw(block + 4, prim.wrapping_add(bytes));
    prim
}
/// `addPrim` into the current bucket (`[visual+0xC]`).
fn add_to_bucket(m: &mut Machine, prim: u32) {
    let block = visual_block(m);
    let bucket = m.lw(block + 0xc);
    add_prim(m, bucket, prim);
}
/// Texture coordinates of the sprite handlers (7, 26): v = y mod 256 in
/// the high byte, u = x mod 64 scaled by the color depth (+0x12: 0 4-bit
/// ×4, 1 8-bit ×2, 2 15-bit); other depths leave +0xC unwritten.
fn sprite_uv(m: &mut Machine, prim: u32, operands: u32) {
    let depth = m.lh(operands + 0x12);
    let shift = match depth {
        0 => 14,
        1 => 15,
        2 => 16,
        _ => return,
    };
    let v = remainder(i32::from(m.lh(operands + 0xa)), 8) << 8;
    let u = remainder(i32::from(m.lh(operands + 8)), 6);
    let u = if shift == 16 { u } else { (u << 16) >> shift };
    m.sh(prim + 0xc, (v | u) as u16);
}

// ---- opcode handlers -------------------------------------------------------------------------

/// 800EFAC8 (opcode 0): nothing (scripts end before it is dispatched).
pub fn op_none() {}

/// 800EFAD0 (opcode 1): flat tile at operand x/y (+4/+6) plus origin and
/// offset pair, size +8/+0xA, clipped and animated by 800EF21C with the
/// selected record (its origin moved by scratch +8/+0xA); code 0x60 (0x62
/// semi-transparent).
pub fn op_tile(m: &mut Machine, op: u32) {
    framed(m, 0x28, |m, _| {
        let state = scratch(m);
        let block = visual_block(m);
        let rect = state + 0x38;
        let (x, y) = origin(m, block, state);
        copy_color(m, block, state);
        let operand = m.lhu(op + 4);
        m.sh(rect, (u32::from(operand)).wrapping_add(x) as u16);
        let operand = m.lhu(op + 6);
        m.sh(rect + 2, (u32::from(operand)).wrapping_add(y) as u16);
        let size = m.lw(op + 8);
        m.sw(rect + 4, size);
        tile_record(m, state);
        if clip(m, rect, state + 0x28) == 1 {
            emit_tile(m, state, rect);
        }
    })
}
/// The selected record copied to scratch +0x28 with its origin moved by
/// the scratch animation origin (+8/+0xA).
fn tile_record(m: &mut Machine, state: u32) {
    copy_record(m, state + 0x28);
    let current = scratch(m);
    let x = m.lhu(state + 0x2c).wrapping_add(m.lhu(current + 8));
    m.sh(state + 0x2c, x);
    let y = m.lhu(state + 0x2e).wrapping_add(m.lhu(current + 0xa));
    m.sh(state + 0x2e, y);
}
/// TILE (len 3, code 0x60/0x62) with the scratch color and `rect`.
fn emit_tile(m: &mut Machine, state: u32, rect: u32) {
    let tile = reserve(m, 0x10);
    let color = color_word(m, state);
    m.sb(tile + 3, 3);
    m.sw(tile + 4, color);
    m.sb(tile + 7, 0x60);
    if semi_transparent(m) {
        m.sb(tile + 7, 0x62);
    }
    let xy = m.lw(rect);
    m.sw(tile + 8, xy);
    let size = m.lw(rect + 4);
    m.sw(tile + 0xc, size);
    add_to_bucket(m, tile);
}

/// 800F0C88 (opcode 2): RGB record selector (scratch +0x14).
pub fn op_select_color(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let value = m.lw(op + 4);
    m.sw(state + 0x14, value);
}
/// 800F0C9C (opcode 3): RGB record `[op+4]` = bytes +5/+6/+7.
pub fn op_set_color(m: &mut Machine, op: u32) {
    let index = u32::from(m.lbu(op + 4));
    let block = visual_block(m);
    let green = u32::from(m.lbu(op + 6));
    let red = u32::from(m.lbu(op + 5));
    let record = (index << 2).wrapping_add(block);
    let blue = u32::from(m.lbu(op + 7));
    m.sw(record + 0x98, red | (green << 8) | (blue << 16));
}
/// 800F0CD8 (opcode 4): semi-transparency mode (scratch +0x1C).
pub fn op_blend(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let value = m.lw(op + 4);
    m.sw(state + 0x1c, value);
}
/// 800F0CEC (opcode 5): bucket = ordering table entry `[op+4]`.
pub fn op_bucket(m: &mut Machine, op: u32) {
    let block = visual_block(m);
    let entry = m.lw(op + 4);
    let ot = m.lw(block + 8);
    m.sw(block + 0xc, ot.wrapping_add(entry << 2));
}
/// 800F0D0C (opcode 6): style index (scratch +0xC word) and condition =
/// its style byte.
pub fn op_condition_word(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let value = m.lw(op + 4);
    m.sw(state + 0xc, value);
    let block = visual_block(m);
    let index = sext16(m.lh(state + 0xc));
    let style = m.lbu(block.wrapping_add(index).wrapping_add(0x4a4));
    m.sw(state + 0x10, u32::from(style));
}
/// 800EFCB8 (opcode 7): textured sprite (SPRT, len 4, code 0x64/0x66) at
/// operand x/y plus origin and offset pair, size +8/+0xA, texture x/y
/// +0xC/+0xE (u/v by depth +0x16), CLUT +0x14; hidden by the animation gate.
pub fn op_sprite(m: &mut Machine, op: u32) {
    let state = scratch(m);
    copy_record(m, state + 0x28);
    if record_hidden(m, state + 0x28) {
        return;
    }
    let block = visual_block(m);
    let (x, y) = origin(m, block, state);
    copy_color(m, block, state);
    let prim = reserve(m, 0x14);
    emit_sprite(m, state, prim, x, y, op + 4);
}
/// SPRT body shared by opcodes 7 and 26 (`operands` = the frame's record).
fn emit_sprite(m: &mut Machine, state: u32, prim: u32, x: u32, y: u32, operands: u32) {
    let color = color_word(m, state);
    m.sb(prim + 3, 4);
    m.sw(prim + 4, color);
    m.sb(prim + 7, 0x64);
    if semi_transparent(m) {
        m.sb(prim + 7, 0x66);
    }
    let px = sext16(m.lh(operands)).wrapping_add(sext16(x as u16 as i16)) & 0xffff;
    let py = sext16(m.lh(operands + 2)).wrapping_add(sext16(y as u16 as i16));
    m.sw(prim + 8, px | (py << 16));
    let height = sext16(m.lh(operands + 6));
    let width = u32::from(m.lhu(operands + 4));
    m.sw(prim + 0x10, width | (height << 16));
    sprite_uv(m, prim, operands);
    let clut = m.lhu(operands + 0x10);
    m.sh(prim + 0xe, clut);
    add_to_bucket(m, prim);
}
/// 800F04EC (opcode 8): DR_MODE with texture page `[op+4]` (SetDrawMode(p,
/// 0, 0, tpage, null)).
pub fn op_draw_mode(m: &mut Machine, op: u32) {
    framed(m, 0x28, |m, sp| {
        let prim = reserve(m, 0xc);
        m.sw(sp + 0x10, 0);
        let tpage = m.lw(op + 4);
        call(m, 0x800a_ac38, [prim, 0, 0, tpage]);
        add_to_bucket(m, prim);
    })
}
/// 800EFF74 (opcode 9): textured quad (POLY_FT4, len 9, code 0x2C/0x2E)
/// over the rectangle at operand +4..+0xB (plus origin and offset pair),
/// texture x/y +0xC/+0xE, size +0x10/+0x12, CLUT +0x14, depth +0x16; the
/// page carries the blend mode when semi-transparent. With an 8-byte record
/// selected (+0x49C) the corners are rotated by 800EF85C; clipped and
/// animated by 800EF21C when the selected record is animated.
pub fn op_quad(m: &mut Machine, op: u32) {
    framed(m, 0x38, |m, sp| {
        let state = scratch(m);
        let block = visual_block(m);
        let operands = op + 4;
        let rect = state + 0x38;
        let (sx, sy) = origin(m, block, state);
        copy_color(m, block, state);
        let record = state + 0x28;
        let scale: u32 = match m.lh(operands + 0x12) {
            0 => 4,
            1 => 2,
            2 => 1,
            _ => 4,
        };
        let value = u32::from(m.lhu(operands)).wrapping_add(sx);
        m.sh(rect, value as u16);
        let value = u32::from(m.lhu(operands + 2)).wrapping_add(sy);
        m.sh(rect + 2, value as u16);
        let size = m.lw(operands + 4);
        m.sw(rect + 4, size);
        copy_record(m, record);
        let visible = if m.lh(record + 2) != 0 {
            let current = scratch(m);
            let x = m.lhu(record + 4).wrapping_add(m.lhu(current + 8));
            m.sh(record + 4, x);
            let y = m.lhu(record + 6).wrapping_add(m.lhu(current + 0xa));
            m.sh(record + 6, y);
            clip(m, rect, record)
        } else {
            1
        };
        if visible != 1 {
            return;
        }
        let prim = reserve(m, 0x28);
        let color = color_word(m, state);
        m.sb(prim + 3, 9);
        m.sw(prim + 4, color);
        m.sb(prim + 7, 0x2c);
        let depth = u32::from(m.lhu(operands + 0x12));
        let ty = u32::from(m.lhu(operands + 0xa));
        let tx = u32::from(m.lhu(operands + 8));
        let mut page = (depth & 3) << 7;
        if semi_transparent(m) {
            m.sb(prim + 7, 0x2e);
            let current = scratch(m);
            let mode = u32::from(m.lhu(current + 0x1c));
            page |= (mode & 3) << 5;
        }
        page |= ((((ty & 0x100) << 16) as i32) >> 20) as u32;
        page |= (tx & 0x3ff) >> 6;
        page |= (ty & 0x200) << 2;
        m.sh(prim + 0x16, page as u16);
        let clut = m.lhu(operands + 0x10);
        m.sh(prim + 0xe, clut);
        let block = visual_block(m);
        let selected = m.lw(block + 0x49c);
        if selected == 0 {
            let x = sext16(m.lh(rect));
            let y = sext16(m.lh(rect + 2));
            let w = sext16(m.lh(rect + 4));
            let h = sext16(m.lh(rect + 6));
            let left = u32::from(m.lhu(rect));
            m.sw(prim + 8, (y << 16) | left);
            m.sw(prim + 0x10, (y << 16) | (x.wrapping_add(w) & 0xffff));
            m.sw(prim + 0x18, (y.wrapping_add(h) << 16) | left);
            m.sw(
                prim + 0x20,
                (y.wrapping_add(h) << 16) | (x.wrapping_add(w) & 0xffff),
            );
        } else {
            let source = block.wrapping_add(selected << 3).wrapping_add(0x418);
            let first = load_unaligned(m, source);
            let second = load_unaligned(m, source + 4);
            store_unaligned(m, sp + 0x10, first);
            store_unaligned(m, sp + 0x14, second);
            let value = u32::from(m.lhu(sp + 0x12)).wrapping_add(sx);
            m.sh(sp + 0x12, value as u16);
            let value = u32::from(m.lhu(sp + 0x14)).wrapping_add(sy);
            m.sh(sp + 0x14, value as u16);
            let block = visual_block(m);
            let mode = m.lw(block + 0x49c);
            rotate_corners(m, prim, rect, sp + 0x10, mode);
        }
        let tx = i32::from(m.lh(operands + 8));
        let ty = i32::from(m.lh(operands + 0xa));
        let u = sext16(remainder(tx, 6) as i16).wrapping_mul(scale);
        let v = (remainder(ty, 8) as u32) << 8;
        m.sh(prim + 0xc, (v | (u & 0xff)) as u16);
        let w = sext16(m.lh(operands + 0xc)).wrapping_mul(scale);
        let right = u.wrapping_sub(1).wrapping_add(w);
        m.sh(prim + 0x14, (v | (right & 0xff)) as u16);
        let h = u32::from(m.lhu(operands + 0xe));
        let bottom = (h.wrapping_add((remainder(ty, 8) - 1) as u32)) << 8;
        m.sh(prim + 0x1c, (bottom | (u & 0xff)) as u16);
        m.sh(prim + 0x24, (bottom | (right & 0xff)) as u16);
        add_to_bucket(m, prim);
    })
}
/// 800F0E30 (opcode 10): move the scratch origin by operand x/y.
pub fn op_move(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let x = m.lhu(state + 4).wrapping_add(m.lhu(op + 4));
    m.sh(state + 4, x);
    let y = m.lhu(state + 6).wrapping_add(m.lhu(op + 6));
    m.sh(state + 6, y);
}
/// 800F0F44 (opcode 11): run script `[op+4]` with the origin moved by
/// +6/+8 (the animation origin set to it when unset); with +0xA == 1 only
/// while the condition matches the selector and the selected record has
/// started.
pub fn op_call(m: &mut Machine, op: u32) {
    framed(m, 0x20, |m, _| {
        let block = visual_block(m);
        let selected = m.lw(block + 0x498);
        let operands = op + 4;
        let record = block.wrapping_add((selected << 4).wrapping_add(0x318));
        if m.lh(operands + 6) == 1 {
            let state = scratch(m);
            let selector = sext16(m.lh(state + 0xe));
            if m.lw(state + 0x10) != selector {
                return;
            }
            if selected != 0 && m.lh(record + 8) >= m.lh(record + 0xe) {
                return;
            }
        }
        let state = scratch(m);
        let x = m.lhu(state + 4).wrapping_add(m.lhu(operands + 2));
        m.sh(state + 4, x);
        let y = m.lhu(state + 6).wrapping_add(m.lhu(operands + 4));
        let animation = m.lw(state + 8);
        m.sh(state + 6, y);
        if animation == 0 {
            let x = m.lhu(state + 4);
            let y = m.lhu(state + 6);
            m.sh(state + 8, x);
            m.sh(state + 0xa, y);
        }
        let script = sext16(m.lh(operands));
        interpret(m, script);
        let state = scratch(m);
        let x = m.lhu(state + 4).wrapping_sub(m.lhu(operands + 2));
        m.sh(state + 4, x);
        let y = m.lhu(state + 6);
        let dy = m.lhu(operands + 4);
        m.sh(state + 0xa, 0);
        m.sh(state + 8, 0);
        m.sh(state + 6, y.wrapping_sub(dy));
    })
}
/// 800F0E1C (opcode 12): offset pair selector (scratch +0x18).
pub fn op_select_offset(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let value = m.lw(op + 4);
    m.sw(state + 0x18, value);
}
/// 800F0E64 (opcode 13): animation record `[op+4]` = the seven halfwords
/// +4..+0x10.
pub fn op_set_record(m: &mut Machine, op: u32) {
    let index = sext16(m.lh(op + 4));
    let first = m.lhu(op + 4);
    let block = visual_block(m);
    let record = block.wrapping_add((index << 4).wrapping_add(0x318));
    m.sh(record, first);
    for offset in [2u32, 4, 6, 8, 0xa, 0xc] {
        let value = m.lhu(op + 4 + offset);
        m.sh(record + offset, value);
    }
}
/// 800F0ED0 (opcode 14): animation record selector (+0x498); clears the
/// animation origin.
pub fn op_select_record(m: &mut Machine, op: u32) {
    let block = visual_block(m);
    let value = m.lw(op + 4);
    let state = scratch(m);
    m.sw(block + 0x498, value);
    m.sh(state + 0xa, 0);
    m.sh(state + 8, 0);
}
/// 800F0EF4 (opcode 15): 8-byte record `[op+4]` = the operand words +4/+8.
pub fn op_set_rotation(m: &mut Machine, op: u32) {
    let index = sext16(m.lh(op + 4));
    let block = visual_block(m);
    let record = block.wrapping_add(index << 3);
    let first = load_unaligned(m, op + 4);
    let second = load_unaligned(m, op + 8);
    store_unaligned(m, record + 0x418, first);
    store_unaligned(m, record + 0x41c, second);
}
/// 800F0F30 (opcode 16): 8-byte record selector (+0x49C).
pub fn op_select_rotation(m: &mut Machine, op: u32) {
    let block = visual_block(m);
    let value = m.lw(op + 4);
    m.sw(block + 0x49c, value);
}
/// 800F0748 / 800F08D8 / 800F0A9C (opcodes 17, 18, 19): flat line
/// (LINE_F2, code 0x40) or polylines of 3/4 points (code 0x48/0x4C, closed
/// by 0x55555555) at the operand points plus the scratch origin; bit 1 of
/// the code set when semi-transparent; hidden by the animation gate.
pub fn op_lines(m: &mut Machine, op: u32, points: u32) {
    let state = scratch(m);
    copy_record(m, state + 0x28);
    if record_hidden(m, state + 0x28) {
        return;
    }
    let block = visual_block(m);
    copy_color(m, block, state);
    let (bytes, length, code) = match points {
        2 => (0x10, 3, 0x40),
        3 => (0x18, 5, 0x48),
        _ => (0x1c, 6, 0x4c),
    };
    let prim = reserve(m, bytes);
    let color = color_word(m, state);
    m.sb(prim + 3, length);
    if points > 2 {
        m.sw(prim + 8 + points * 4, 0x5555_5555);
    }
    m.sw(prim + 4, color);
    m.sb(prim + 7, code);
    if semi_transparent(m) {
        if points == 2 {
            m.sb(prim + 7, 0x42);
        } else {
            let code = m.lbu(prim + 7);
            m.sb(prim + 7, code | 2);
        }
    }
    let state = scratch(m);
    for point in 0..points {
        let at = op + 4 + point * 4;
        let x = m.lhu(at).wrapping_add(m.lhu(state + 4));
        m.sh(prim + 8 + point * 4, x);
        let y = m.lhu(at + 2).wrapping_add(m.lhu(state + 6));
        m.sh(prim + 0xa + point * 4, y);
    }
    add_to_bucket(m, prim);
}
/// 800F0D40 (opcode 20): style index (halfword) and condition = its byte.
pub fn op_condition(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let index = m.lhu(op + 4);
    let block = visual_block(m);
    m.sh(state + 0xc, index);
    let style = m.lbu(block.wrapping_add(sext16(index as i16)).wrapping_add(0x4a4));
    m.sw(state + 0x10, u32::from(style));
}
/// 800F0D70 (opcode 21): selector (scratch +0xE) = halfword +6.
pub fn op_selector(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let value = m.lh(op + 6);
    m.sh(state + 0xe, value as u16);
}
/// 800F0DD4 (opcode 22): condition = the selector when `[op+4]` is 1, else
/// the style byte of the style index.
pub fn op_condition_select(m: &mut Machine, op: u32) {
    let state = scratch(m);
    if m.lw(op + 4) == 1 {
        let value = sext16(m.lh(state + 0xe));
        m.sw(state + 0x10, value);
    } else {
        let block = visual_block(m);
        let index = sext16(m.lh(state + 0xc));
        let style = m.lbu(block.wrapping_add(index).wrapping_add(0x4a4));
        m.sw(state + 0x10, u32::from(style));
    }
}
/// 800F0560 (opcode 23): DR_AREA clipped to the 320x512 screen from the
/// operand rectangle plus origin and offset pair (x -1: the whole screen).
/// The bottom test adds the width (+8), as the original does.
pub fn op_draw_area(m: &mut Machine, op: u32) {
    framed(m, 0x28, |m, sp| {
        let state = scratch(m);
        let block = visual_block(m);
        let raw_x = u32::from(m.lhu(op + 4));
        let index = m.lw(state + 0x18);
        let pair = block.wrapping_add(index << 2);
        let ox = u32::from(m.lhu(pair + 0x298));
        let oy = u32::from(m.lhu(pair + 0x29a));
        let oy = oy.wrapping_add(u32::from(m.lhu(state + 6)));
        let ox = ox.wrapping_add(u32::from(m.lhu(state + 4)));
        let operands = op + 4;
        let x = m.lh(op + 4);
        if x == -1 {
            m.sh(sp + 0x14, 0x140);
            m.sh(sp + 0x10, 0);
            m.sh(sp + 0x12, 0);
            m.sh(sp + 0x16, 0x1ff);
        } else {
            let left = i32::from(x).wrapping_add(i32::from(ox as u16 as i16));
            if left >= 0x140 {
                return;
            }
            let right = left.wrapping_add(i32::from(m.lh(operands + 4)));
            let start = raw_x.wrapping_add(ox);
            if right >= 0x140 {
                m.sh(sp + 0x10, start as u16);
                let width = 0x140u32
                    .wrapping_sub(ox)
                    .wrapping_sub(u32::from(m.lhu(op + 4)));
                m.sh(sp + 0x14, width as u16);
            } else if left >= 0 {
                m.sh(sp + 0x10, start as u16);
                let width = m.lhu(operands + 4);
                m.sh(sp + 0x14, width);
            } else {
                if right < 0 {
                    return;
                }
                m.sh(sp + 0x10, 0);
                let width = u32::from(m.lhu(operands + 4))
                    .wrapping_add(u32::from(m.lhu(op + 4)).wrapping_add(ox));
                m.sh(sp + 0x14, width as u16);
            }
            let raw_y = u32::from(m.lhu(operands + 2));
            let top = i32::from(m.lh(operands + 2)).wrapping_add(i32::from(oy as u16 as i16));
            if top >= 0x200 {
                return;
            }
            let start = raw_y.wrapping_add(oy);
            if top.wrapping_add(i32::from(m.lh(operands + 4))) >= 0x200 {
                m.sh(sp + 0x12, start as u16);
                let height = 0x1ffu32
                    .wrapping_sub(oy)
                    .wrapping_sub(u32::from(m.lhu(operands + 2)));
                m.sh(sp + 0x16, height as u16);
            } else if top >= 0 {
                m.sh(sp + 0x12, start as u16);
                let height = m.lhu(operands + 6);
                m.sh(sp + 0x16, height);
            } else {
                if top.wrapping_add(i32::from(m.lh(operands + 6))) < 0 {
                    return;
                }
                m.sh(sp + 0x12, 0);
                let height = u32::from(m.lhu(operands + 6))
                    .wrapping_add(u32::from(m.lhu(operands + 2)).wrapping_add(oy));
                m.sh(sp + 0x16, height as u16);
            }
        }
        let prim = reserve(m, 0xc);
        sdk::set_draw_area(m, prim, sp + 0x10);
        add_to_bucket(m, prim);
    })
}
/// 800F0D84 (opcode 24): selector = `[op+4]` - 1 and flag bit 3.
pub fn op_selector_flag(m: &mut Machine, op: u32) {
    let state = scratch(m);
    let value = m.lhu(op + 4).wrapping_sub(1);
    let block = visual_block(m);
    m.sh(state + 0xe, value);
    let flags = m.lw(block);
    m.sw(block, flags | 8);
}
/// 800F0DB4 (opcode 25): clear flag bit 3.
pub fn op_clear_flag(m: &mut Machine) {
    let block = visual_block(m);
    let flags = m.lw(block);
    m.sw(block, flags & !8);
}
/// 800F1050 (opcode 26): animated sprite: frames of 6 words after the
/// header (count +4, start +6, skipped through scratch +0x20); the frame
/// of the condition's elapsed time is drawn as opcode 7 does, followed by
/// a DR_MODE for its texture page.
pub fn op_sprite_frames(m: &mut Machine, op: u32) {
    framed(m, 0x30, |m, sp| {
        let count = m.lh(op + 4);
        let start = m.lh(op + 6);
        let state = scratch(m);
        let now = m.lw(state + 0x10);
        m.sh(state + 0x20, (i32::from(count).wrapping_mul(6)) as u16);
        let frame = now.wrapping_sub(sext16(start)).wrapping_mul(3);
        if (now as i32) < i32::from(start) {
            return;
        }
        if (now as i32) >= i32::from(start).wrapping_add(i32::from(count)) {
            return;
        }
        copy_record(m, state + 0x28);
        if record_hidden(m, state + 0x28) {
            return;
        }
        let current = scratch(m);
        let block = visual_block(m);
        let (x, y) = origin(m, block, current);
        copy_color(m, block, current);
        let prim = reserve(m, 0x14);
        let operands = op.wrapping_add(frame << 3).wrapping_add(0xc);
        emit_sprite(m, state, prim, x, y, operands);
        let mode = reserve(m, 0xc);
        let depth = u32::from(m.lhu(operands + 0x12));
        let ty = u32::from(m.lhu(operands + 0xa));
        let tx = u32::from(m.lhu(operands + 8));
        let current = scratch(m);
        let blend = m.lw(current + 0x1c);
        m.sw(sp + 0x10, 0);
        let page = ((depth & 3) << 7)
            | ((blend & 3) << 5)
            | (((((ty & 0x100) << 16) as i32) >> 20) as u32)
            | ((tx & 0x3ff) >> 6)
            | ((ty & 0x200) << 2);
        call(m, 0x800a_ac38, [mode, 0, 0, page]);
        add_to_bucket(m, mode);
    })
}
/// 800F1430 (opcode 27): animated tile: frames of 3 words (x/y, size)
/// after the header; the current frame is drawn as opcode 1 does.
pub fn op_tile_frames(m: &mut Machine, op: u32) {
    framed(m, 0x30, |m, _| {
        let count = m.lh(op + 4);
        let start = m.lh(op + 6);
        let state = scratch(m);
        let rect = state + 0x38;
        let now = m.lw(state + 0x10);
        m.sh(state + 0x20, (i32::from(count).wrapping_mul(3)) as u16);
        let frame = now.wrapping_sub(sext16(start)).wrapping_mul(3);
        if (now as i32) < i32::from(start) {
            return;
        }
        if (now as i32) >= i32::from(start).wrapping_add(i32::from(count)) {
            return;
        }
        let block = visual_block(m);
        let (x, y) = origin(m, block, state);
        let operands = op.wrapping_add((frame << 2).wrapping_add(0xc));
        copy_color(m, block, state);
        let value = u32::from(m.lhu(operands)).wrapping_add(x);
        m.sh(rect, value as u16);
        let value = u32::from(m.lhu(operands + 2)).wrapping_add(y);
        m.sh(rect + 2, value as u16);
        let size = m.lw(operands + 4);
        m.sw(rect + 4, size);
        tile_record(m, state);
        if clip(m, rect, state + 0x28) == 1 {
            emit_tile(m, state, rect);
        }
    })
}

// ---- 800EF21C / 800EF85C / 800EF1F4: clipping and rotation ------------------------------------

/// 800EF21C: animate/clip `rect` (x, y, w, h halfwords) by `record` (+2
/// mode, +4/+6 origin, +8/+0xA/+0xC times, +0xE current time). Returns 1
/// to draw, 0 to skip. Mode 0 and modes above 4 draw unchanged; 1 grows
/// the width from the origin, then the height; 2 the height, then the
/// width; 3 both at once; 4 draws from time +0xC on.
pub fn clip(m: &mut Machine, rect: u32, record: u32) -> u32 {
    let mode = m.lh(record + 2) as u16;
    let h = |m: &mut Machine, at: u32| i32::from(m.lh(at));
    let hu = |m: &mut Machine, at: u32| u32::from(m.lhu(at));
    match mode {
        0 => 1,
        1 => {
            let now = h(m, record + 0xe);
            let first = h(m, record + 8);
            if first >= now {
                return 0;
            }
            let second = h(m, record + 0xa);
            if second >= now {
                // Phase 1: x and width from the origin.
                let dx = h(m, rect).wrapping_sub(h(m, record + 4));
                let product = dx.wrapping_mul(now.wrapping_sub(first));
                let step = quotient(m, product, second.wrapping_sub(first), 0x800e_f2c0);
                let x = hu(m, record + 4).wrapping_add(step as u32);
                m.sh(rect, x as u16);
                let y = h(m, rect + 2);
                let oy = h(m, record + 6);
                let origin_y = hu(m, record + 6);
                if y == oy {
                    m.sh(rect + 2, origin_y as u16);
                } else {
                    let dy = y.wrapping_sub(oy);
                    let sign = quotient(m, dy, dy.wrapping_abs(), 0x800e_f31c);
                    m.sh(rect + 2, origin_y.wrapping_add(sign as u32) as u16);
                }
                let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 8));
                let product = h(m, rect + 4).wrapping_mul(elapsed);
                let span = h(m, record + 0xa).wrapping_sub(h(m, record + 8));
                let width = quotient(m, product, span, 0x800e_f380);
                m.sh(rect + 6, 2);
                m.sh(rect + 4, width as u16);
                return 1;
            }
            let third = h(m, record + 0xc);
            if now >= third {
                return 1;
            }
            // Phase 2: y and height.
            let dy = h(m, rect + 2).wrapping_sub(h(m, record + 6));
            let product = dy.wrapping_mul(now.wrapping_sub(second));
            let step = quotient(m, product, third.wrapping_sub(second), 0x800e_f3f8);
            let y = hu(m, record + 6).wrapping_add(step as u32);
            m.sh(rect + 2, y as u16);
            let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 0xa));
            let product = h(m, rect + 6).wrapping_mul(elapsed);
            let span = h(m, record + 0xc).wrapping_sub(h(m, record + 0xa));
            let height = quotient(m, product, span, 0x800e_f458);
            m.sh(rect + 6, height as u16);
            1
        }
        2 => {
            let now = h(m, record + 0xe);
            if h(m, record + 8) >= now {
                return 0;
            }
            let second = h(m, record + 0xa);
            if now < second {
                // Phase 1: x pinned near the origin, y and height.
                let dx = h(m, rect).wrapping_sub(h(m, record + 4));
                let sign = quotient(m, dx, dx.wrapping_abs(), 0x800e_f4d8);
                let x = hu(m, record + 4).wrapping_add(sign as u32);
                let y = h(m, rect + 2);
                m.sh(rect, x as u16);
                let oy = h(m, record + 6);
                let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 8));
                let product = y.wrapping_sub(oy).wrapping_mul(elapsed);
                let span = h(m, record + 0xa).wrapping_sub(h(m, record + 8));
                let step = quotient(m, product, span, 0x800e_f540);
                let origin_y = hu(m, record + 6);
                m.sh(rect + 4, 1);
                m.sh(rect + 2, origin_y.wrapping_add(step as u32) as u16);
                let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 8));
                let product = h(m, rect + 6).wrapping_mul(elapsed);
                let span = h(m, record + 0xa).wrapping_sub(h(m, record + 8));
                let height = quotient(m, product, span, 0x800e_f5a4);
                m.sh(rect + 6, height as u16);
                return 1;
            }
            let third = h(m, record + 0xc);
            if now >= third {
                return 1;
            }
            // Phase 2: x and width.
            let dx = h(m, rect).wrapping_sub(h(m, record + 4));
            let product = dx.wrapping_mul(now.wrapping_sub(second));
            let step = quotient(m, product, third.wrapping_sub(second), 0x800e_f610);
            let x = hu(m, record + 4).wrapping_add(step as u32);
            m.sh(rect, x as u16);
            let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 0xa));
            let product = h(m, rect + 4).wrapping_mul(elapsed);
            let span = h(m, record + 0xc).wrapping_sub(h(m, record + 0xa));
            let width = quotient(m, product, span, 0x800e_f670);
            m.sh(rect + 4, width as u16);
            1
        }
        3 => {
            let now = h(m, record + 0xe);
            let first = h(m, record + 8);
            if first >= now {
                return 0;
            }
            let second = h(m, record + 0xa);
            if now >= second {
                return 1;
            }
            let dx = h(m, rect).wrapping_sub(h(m, record + 4));
            let product = dx.wrapping_mul(now.wrapping_sub(first));
            let step = quotient(m, product, second.wrapping_sub(first), 0x800e_f6f4);
            let x = hu(m, record + 4).wrapping_add(step as u32);
            let y = h(m, rect + 2);
            m.sh(rect, x as u16);
            let oy = h(m, record + 6);
            let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 8));
            let product = y.wrapping_sub(oy).wrapping_mul(elapsed);
            let span = h(m, record + 0xa).wrapping_sub(h(m, record + 8));
            let step = quotient(m, product, span, 0x800e_f75c);
            let origin_y = hu(m, record + 6);
            m.sh(rect + 2, origin_y.wrapping_add(step as u32) as u16);
            let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 8));
            let product = h(m, rect + 4).wrapping_mul(elapsed);
            let span = h(m, record + 0xc).wrapping_sub(h(m, record + 8));
            let width = quotient(m, product, span, 0x800e_f7bc);
            m.sh(rect + 4, width as u16);
            let elapsed = h(m, record + 0xe).wrapping_sub(h(m, record + 8));
            let product = h(m, rect + 6).wrapping_mul(elapsed);
            let span = h(m, record + 0xa).wrapping_sub(h(m, record + 8));
            let height = quotient(m, product, span, 0x800e_f814);
            m.sh(rect + 6, height as u16);
            1
        }
        4 => u32::from(h(m, record + 0xe) >= h(m, record + 0xc)),
        _ => 1,
    }
}

/// 800EF85C: corners of a POLY_FT4 `prim` for `rect`: axis-aligned for
/// `mode` 0 (returns 2); otherwise rotated about the record's center
/// (`rotation` +2/+4 x/y, +6 angle mod 4096) with row 1 of the matrix
/// doubled (800EF1F4) through RotMatrix, TransMatrix, SetRotMatrix,
/// SetTransMatrix and RotTrans (returns 1).
pub fn rotate_corners(m: &mut Machine, prim: u32, rect: u32, rotation: u32, mode: u32) -> u32 {
    framed(m, 0xc8, |m, sp| {
        if mode == 0 {
            let x = m.lhu(rect);
            m.sh(prim + 8, x);
            let y = m.lhu(rect + 2);
            m.sh(prim + 0xa, y);
            let right = m.lhu(rect).wrapping_add(m.lhu(rect + 4));
            m.sh(prim + 0x10, right);
            let y = m.lhu(rect + 2);
            m.sh(prim + 0x12, y);
            let x = m.lhu(rect);
            m.sh(prim + 0x18, x);
            let bottom = m.lhu(rect + 2).wrapping_add(m.lhu(rect + 6));
            m.sh(prim + 0x1a, bottom);
            let right = m.lhu(rect).wrapping_add(m.lhu(rect + 4));
            m.sh(prim + 0x20, right);
            let bottom = m.lhu(rect + 2).wrapping_add(m.lhu(rect + 6));
            m.sh(prim + 0x22, bottom);
            return 2;
        }
        m.sh(sp + 0x10, 0);
        m.sh(sp + 0x12, 0);
        let angle = i32::from(m.lh(rotation + 6));
        m.sh(sp + 0x14, remainder(angle, 12) as u16);
        let center_x = sext16(m.lh(rotation + 2));
        m.sw(sp + 0x18, center_x);
        let center_y = sext16(m.lh(rotation + 4));
        m.sw(sp + 0x20, 0);
        m.sw(sp + 0x1c, center_y);
        let matrix = sp + 0x28;
        call(m, 0x800a_5958, [sp + 0x10, matrix, 0, 0]);
        call(m, 0x800a_53a8, [matrix, sp + 0x18, 0, 0]);
        double_row(m, matrix);
        call(m, 0x800a_5518, [matrix, 0, 0, 0]);
        call(m, 0x800a_5578, [matrix, 0, 0, 0]);
        let flag = sp + 0xa8;
        let left = u32::from(m.lhu(rect));
        let top = u32::from(m.lhu(rect + 2));
        let width = u32::from(m.lhu(rect + 4));
        let cx = u32::from(m.lhu(rotation + 2));
        let height = u32::from(m.lhu(rect + 6));
        m.sh(sp + 0x48, left.wrapping_sub(cx) as u16);
        let cy = u32::from(m.lhu(rotation + 4));
        let right = left.wrapping_add(width).wrapping_sub(2);
        m.sh(sp + 0x4c, 0);
        m.sh(sp + 0x4a, top.wrapping_sub(cy) as u16);
        let cx = u32::from(m.lhu(rotation + 2));
        m.sh(sp + 0x50, right.wrapping_sub(cx) as u16);
        let cy = u32::from(m.lhu(rotation + 4));
        m.sh(sp + 0x54, 0);
        m.sh(sp + 0x52, top.wrapping_sub(cy) as u16);
        let cx = u32::from(m.lhu(rotation + 2));
        let bottom = top.wrapping_add(height).wrapping_sub(2);
        m.sh(sp + 0x58, left.wrapping_sub(cx) as u16);
        let cy = u32::from(m.lhu(rotation + 4));
        m.sh(sp + 0x5c, 0);
        m.sh(sp + 0x5a, bottom.wrapping_sub(cy) as u16);
        let cx = u32::from(m.lhu(rotation + 2));
        m.sh(sp + 0x60, right.wrapping_sub(cx) as u16);
        let cy = u32::from(m.lhu(rotation + 4));
        m.sh(sp + 0x64, 0);
        m.sh(sp + 0x62, bottom.wrapping_sub(cy) as u16);
        for (vector, out) in [(0x48, 0x68), (0x50, 0x78), (0x58, 0x88), (0x60, 0x98)] {
            call(m, 0x800a_58b8, [sp + vector, sp + out, flag, 0]);
        }
        for (offset, at) in [
            (8, 0x68),
            (0xa, 0x6c),
            (0x10, 0x78),
            (0x12, 0x7c),
            (0x18, 0x88),
            (0x1a, 0x8c),
            (0x20, 0x98),
            (0x22, 0x9c),
        ] {
            let value = m.lhu(sp + at);
            m.sh(prim + offset, value);
        }
        1
    })
}

/// 800EF1F4: double the matrix halfwords +6, +0xA and +8 (row 1). Returns
/// the doubled +8 before truncation.
pub fn double_row(m: &mut Machine, matrix: u32) -> u32 {
    let first = m.lhu(matrix + 6);
    let third = m.lhu(matrix + 0xa);
    m.sh(matrix + 6, first << 1);
    let second = m.lhu(matrix + 8);
    m.sh(matrix + 0xa, third << 1);
    m.sh(matrix + 8, second << 1);
    u32::from(second) << 1
}

// ---- dispatch -----------------------------------------------------------------------------------

/// Visual routines by original address (only asked by `ov2_core::dispatch`
/// while module 2 is loaded). Each routine returns the original's v0,
/// except the 28 opcode handlers ([`HANDLERS`]): only the interpreter's
/// `jalr` at 800F1724 reaches them, and it reloads v0 at 800F1730 before
/// any use, so their v0 is dead and the arms return 0. 800F1674 itself
/// always leaves 0 (the flag test, or opcode 0 << 2 at the end).
pub(crate) fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800f_1770 => set_style(m, a0, a1),
        0x800f_18ec => set_offset(m, a0, a1, a2),
        0x800f_1908 => get_offset(m, a0, a1, a2),
        0x800f_193c => set_parameter(m, a0, a1),
        0x800f_196c => select_page(m, a0),
        0x800f_197c => initialize(m),
        0x800f_1a9c => bind_scripts(m, a0),
        0x800f_1b00 => prepare_ot(m, a0),
        0x800f_1ba0 => current_ot(m),
        0x800f_1bb8 => draw(m, a0),
        0x800f_1674 => {
            interpret(m, a0);
            0
        }
        0x800e_fac8 => {
            op_none();
            0
        }
        0x800e_fad0 => {
            op_tile(m, a0);
            0
        }
        0x800f_0c88 => {
            op_select_color(m, a0);
            0
        }
        0x800f_0c9c => {
            op_set_color(m, a0);
            0
        }
        0x800f_0cd8 => {
            op_blend(m, a0);
            0
        }
        0x800f_0cec => {
            op_bucket(m, a0);
            0
        }
        0x800f_0d0c => {
            op_condition_word(m, a0);
            0
        }
        0x800e_fcb8 => {
            op_sprite(m, a0);
            0
        }
        0x800f_04ec => {
            op_draw_mode(m, a0);
            0
        }
        0x800e_ff74 => {
            op_quad(m, a0);
            0
        }
        0x800f_0e30 => {
            op_move(m, a0);
            0
        }
        0x800f_0f44 => {
            op_call(m, a0);
            0
        }
        0x800f_0e1c => {
            op_select_offset(m, a0);
            0
        }
        0x800f_0e64 => {
            op_set_record(m, a0);
            0
        }
        0x800f_0ed0 => {
            op_select_record(m, a0);
            0
        }
        0x800f_0ef4 => {
            op_set_rotation(m, a0);
            0
        }
        0x800f_0f30 => {
            op_select_rotation(m, a0);
            0
        }
        0x800f_0748 => {
            op_lines(m, a0, 2);
            0
        }
        0x800f_08d8 => {
            op_lines(m, a0, 3);
            0
        }
        0x800f_0a9c => {
            op_lines(m, a0, 4);
            0
        }
        0x800f_0d40 => {
            op_condition(m, a0);
            0
        }
        0x800f_0d70 => {
            op_selector(m, a0);
            0
        }
        0x800f_0dd4 => {
            op_condition_select(m, a0);
            0
        }
        0x800f_0560 => {
            op_draw_area(m, a0);
            0
        }
        0x800f_0d84 => {
            op_selector_flag(m, a0);
            0
        }
        0x800f_0db4 => {
            op_clear_flag(m);
            0
        }
        0x800f_1050 => {
            op_sprite_frames(m, a0);
            0
        }
        0x800f_1430 => {
            op_tile_frames(m, a0);
            0
        }
        0x800e_f21c => clip(m, a0, a1),
        0x800e_f85c => rotate_corners(m, a0, a1, a2, a3),
        0x800e_f1f4 => double_row(m, a0),
        _ => return None,
    })
}
