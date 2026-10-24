//! Overlay 1 (front-end menus): the draw-script interpreter `800D51E0`, its
//! 28 command handlers (table `800D88D8`) and the visual-state helpers
//! (`800D52DC`..`800D5724`), on the address-preserving machine.
//!
//! The visual block `[0x800DB00C]` (0x528 bytes, [`init_visuals`]):
//! +0x0 flags (bit 0 scripts installed, bit 3 set by command 24), +0x4 packet
//! pointer, +0x8 ordering table of the current buffer (two 16-word tables at
//! +0x10 and +0x50, buffer index +0x90), +0xC ordering-table bin the commands
//! link into, +0x98 128 RGB colours (4 bytes), +0x298 32 x/y offsets,
//! +0x318 16 animation records (0x10 bytes), +0x418 16 rotation records
//! (8 bytes), +0x498 selected animation, +0x49C selected rotation, +0x4A0
//! page (script index), +0x4A4 128 style bytes, +0x524 script table.
//!
//! The interpreter context lives on the scratchpad (`[0x800DB008]` =
//! 0x1F800000): +0x4/+0x6 translation, +0x8/+0xA animation offset, +0xC
//! selector, +0xE case value, +0x10 selected value, +0x14 colour slot,
//! +0x18 offset slot, +0x1C blend mode (-1: opaque), +0x20 extra command
//! length (words), +0x28 animation record copy, +0x38 rectangle, +0x40
//! colour, +0x44 command lengths, +0x60 conditional-command flags.
use crate::{
    dispatch,
    machine::{Machine, SmallData},
    ov1::{DISPLAY, SCRIPT_CONTEXT, VISUAL, copy_word, div_trap, lbu, lh, lhu, link, off, sra},
};

/// 800D884C: command lengths in words (one per word, low byte used).
const COMMAND_LENGTHS: u32 = 0x800d_884c;
/// 800D88BC: per-command flag: executed only when the case value matches.
const COMMAND_CONDITIONAL: u32 = 0x800d_88bc;
/// 800D88D8: command handlers.
const COMMAND_HANDLERS: u32 = 0x800d_88d8;
/// Scratchpad interpreter context.
const CONTEXT_BASE: u32 = 0x1f80_0000;

fn visual(m: &mut Machine) -> u32 {
    m.lw(VISUAL)
}
fn context(m: &mut Machine) -> u32 {
    m.lw(SCRIPT_CONTEXT)
}

/// Copy the selected animation record (visual +0x318 + 16 * [+0x498]) to
/// context +0x28 (four unaligned word pairs).
fn copy_animation(m: &mut Machine, visual: u32, ctx: u32) {
    let index = m.lw(off(visual, 0x498));
    let record = off(visual, index << 4);
    for i in 0..4 {
        copy_word(m, off(record, 0x318 + 4 * i), off(ctx, 0x28 + 4 * i));
    }
}

/// The animation gate shared by the drawing commands: a running animation
/// (type +0x2A nonzero) whose time +0x36 is still before its start +0x34
/// suppresses the command.
fn animation_waiting(m: &mut Machine, ctx: u32) -> bool {
    if lh(m, off(ctx, 0x2a)) == 0 {
        return false;
    }
    let start = lh(m, off(ctx, 0x34));
    let time = lh(m, off(ctx, 0x36));
    (time as i32) < (start as i32)
}

/// The packet colour word: bytes 0..2 of context +0x40.
fn color_word(m: &mut Machine, ctx: u32) -> u32 {
    let r = lbu(m, off(ctx, 0x40));
    let g = lbu(m, off(ctx, 0x41));
    let b = lbu(m, off(ctx, 0x42));
    r | g << 8 | b << 16
}

/// Reserve `bytes` of packet space at visual +0x4; returns the packet.
fn reserve(m: &mut Machine, bytes: u32) -> u32 {
    let visual = visual(m);
    let packet = m.lw(off(visual, 4));
    m.sw(off(visual, 4), packet.wrapping_add(bytes));
    packet
}

/// Link `packet` into the bin at visual +0xC.
fn link_current(m: &mut Machine, packet: u32) {
    let visual = visual(m);
    let bin = m.lw(off(visual, 0xc));
    link(m, bin, packet);
}

/// `(value % 256) << 8` and `value % 64` with C (truncating) remainders, the
/// texture-coordinate split of commands 7, 9 and 26.
fn rem(value: u32, divisor: i32) -> u32 {
    (value as i32).wrapping_rem(divisor) as u32
}

/// The packed U/V halfword of a sprite record (`v` at +0xA, `u` at +0x8,
/// colour mode at +0x12: 0 four-bit, 1 eight-bit, 2 sixteen-bit; another
/// mode leaves the packet's word untouched).
fn sprite_uv(m: &mut Machine, record: u32, packet: u32) {
    let mode = lh(m, off(record, 0x12));
    let shift = match mode {
        0 => 14,
        1 => 15,
        2 => 16,
        _ => return,
    };
    let v = rem(lh(m, off(record, 0xa)), 256) << 8;
    let u = rem(lh(m, off(record, 0x8)), 64);
    let u = if shift == 16 { u } else { sra(u << 16, shift) };
    m.sh(off(packet, 0xc), (v | u) as u16);
}

// ---- interpreter ------------------------------------------------------------------

/// 800D51E0: run script `index` of the installed table (visual +0x524)
/// when bit 0 of the visual flags is set. A command whose conditional flag
/// (context +0x60 + command) is set runs only while the case value +0xE
/// equals the selected value +0x10; each command's length (context +0x44 +
/// command, plus the extra +0x20 a handler may set) advances to the next.
/// Command 0 ends the script.
pub fn run_script(m: &mut Machine, index: u32) {
    m.push_frame(0x20);
    run_script_body(m, index);
    m.pop_frame(0x20);
}

fn run_script_body(m: &mut Machine, index: u32) {
    let visual = visual(m);
    if m.lw(visual) & 1 == 0 {
        return;
    }
    let table = m.lw(off(visual, 0x524));
    let mut command = m.lw(off(table, index << 2));
    loop {
        let ctx = context(m);
        let opcode = m.lw(command);
        let entry = off(ctx, opcode);
        if lbu(m, off(entry, 0x60)) != 0 {
            let case = lh(m, off(ctx, 0xe));
            let value = m.lw(off(ctx, 0x10));
            if value != case {
                let length = lbu(m, off(entry, 0x44));
                command = off(command, length << 2);
                continue;
            }
        }
        let ctx = context(m);
        m.sh(off(ctx, 0x20), 0);
        let opcode = m.lw(command);
        if opcode == 0 {
            return;
        }
        let handler = m.lw(off(COMMAND_HANDLERS, opcode << 2));
        dispatch::call(m, handler, [command, 0, 0, 0]);
        let ctx = context(m);
        let opcode = m.lw(command);
        let length = lbu(m, off(off(ctx, opcode), 0x44));
        let extra = lh(m, off(ctx, 0x20));
        command = off(command, length.wrapping_add(extra) << 2);
    }
}

// ---- commands ---------------------------------------------------------------------

/// 800D3634 (command 0): no operation (the interpreter stops before it).
pub fn command_end() {}

/// 800D363C (command 1): flat rectangle `{x, y, w, h}` at +4 in the current
/// colour, offset slot and translation, animated by the selected animation
/// record (800D2D88 result 1 draws); TILE 0x60, 0x62 when blended.
pub fn command_tile(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let visual = visual(m);
    let rect = off(ctx, 0x38);
    let slot = m.lw(off(ctx, 0x18));
    let color = m.lw(off(ctx, 0x14));
    let tx = lhu(m, off(ctx, 4));
    let ty = lhu(m, off(ctx, 6));
    let offset = off(visual, slot << 2);
    let ox = lhu(m, off(offset, 0x298));
    let oy = lhu(m, off(offset, 0x29a));
    copy_word(m, off(off(visual, color << 2), 0x98), off(ctx, 0x40));
    let x = lhu(m, off(command, 4));
    m.sh(rect, x.wrapping_add(ox.wrapping_add(tx)) as u16);
    let y = lhu(m, off(command, 6));
    m.sh(off(rect, 2), y.wrapping_add(oy.wrapping_add(ty)) as u16);
    let size = m.lw(off(command, 8));
    m.sw(off(rect, 4), size);
    let visual = self::visual(m);
    let anim = off(ctx, 0x28);
    let ctx2 = context(m);
    copy_animation(m, visual, ctx);
    let v = lhu(m, off(anim, 4)).wrapping_add(lhu(m, off(ctx2, 8)));
    m.sh(off(anim, 4), v as u16);
    let v = lhu(m, off(anim, 6)).wrapping_add(lhu(m, off(ctx2, 0xa)));
    m.sh(off(anim, 6), v as u16);
    if animate(m, rect, anim) != 1 {
        return;
    }
    let packet = reserve(m, 0x10);
    let rgb = color_word(m, ctx);
    m.sb(off(packet, 3), 3);
    m.sw(off(packet, 4), rgb);
    m.sb(off(packet, 7), 0x60);
    let ctx2 = context(m);
    if m.lw(off(ctx2, 0x1c)) != 0xffff_ffff {
        m.sb(off(packet, 7), 0x62);
    }
    let xy = m.lw(rect);
    m.sw(off(packet, 8), xy);
    let wh = m.lw(off(rect, 4));
    m.sw(off(packet, 0xc), wh);
    link_current(m, packet);
}

/// 800D47F4 (command 2): colour slot (context +0x14) = word +4.
pub fn command_color_slot(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let value = m.lw(off(command, 4));
    m.sw(off(ctx, 0x14), value);
}

/// 800D4808 (command 3): colour `[+4]` = RGB bytes +5..+7.
pub fn command_set_color(m: &mut Machine, command: u32) {
    let slot = lbu(m, off(command, 4));
    let visual = visual(m);
    let b = lbu(m, off(command, 7));
    let g = lbu(m, off(command, 6));
    let r = lbu(m, off(command, 5));
    let value = r | g << 8 | b << 16;
    m.sw(off(off(visual, slot << 2), 0x98), value);
}

/// 800D4844 (command 4): blend mode (context +0x1C) = word +4.
pub fn command_blend(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let value = m.lw(off(command, 4));
    m.sw(off(ctx, 0x1c), value);
}

/// 800D4858 (command 5): link bin (visual +0xC) = ordering table +4 * [+4].
pub fn command_layer(m: &mut Machine, command: u32) {
    let visual = visual(m);
    let bin = m.lw(off(command, 4));
    let ot = m.lw(off(visual, 8));
    m.sw(off(visual, 0xc), ot.wrapping_add(bin << 2));
}

/// 800D4878 (command 6): selector word (context +0xC) = word +4; selected
/// value +0x10 = the style byte of the selector's low halfword.
pub fn command_select(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let value = m.lw(off(command, 4));
    m.sw(off(ctx, 0xc), value);
    let visual = visual(m);
    let slot = lh(m, off(ctx, 0xc));
    let style = lbu(m, off(off(visual, slot), 0x4a4));
    m.sw(off(ctx, 0x10), style);
}

/// 800D3824 (command 7): sprite record at +4 (x, y, w, h, u, v, -, clut,
/// mode at +0x12) with the offset slot and translation: SPRT 0x64 (0x66
/// blended), animation gate as command 1 but without interpolation.
pub fn command_sprite(m: &mut Machine, command: u32) {
    let visual = visual(m);
    let ctx = context(m);
    copy_animation(m, visual, ctx);
    if animation_waiting(m, ctx) {
        return;
    }
    let ctx2 = context(m);
    let visual = self::visual(m);
    let slot = m.lw(off(ctx2, 0x18));
    let color = m.lw(off(ctx2, 0x14));
    let tx = lhu(m, off(ctx2, 4));
    let ty = lhu(m, off(ctx2, 6));
    let offset = off(visual, slot << 2);
    let ox = lhu(m, off(offset, 0x298));
    let oy = lhu(m, off(offset, 0x29a));
    copy_word(m, off(off(visual, color << 2), 0x98), off(ctx, 0x40));
    let packet = m.lw(off(visual, 4));
    m.sw(off(visual, 4), packet.wrapping_add(0x14));
    let rgb = color_word(m, ctx);
    m.sb(off(packet, 3), 4);
    let x = ox.wrapping_add(tx);
    let y = oy.wrapping_add(ty);
    m.sw(off(packet, 4), rgb);
    m.sb(off(packet, 7), 0x64);
    let ctx2 = context(m);
    let record = off(command, 4);
    if m.lw(off(ctx2, 0x1c)) != 0xffff_ffff {
        m.sb(off(packet, 7), 0x66);
    }
    sprite_body(m, record, packet, x, y);
}

/// The common tail of the sprite commands 7 and 26 (`record` = x, y, w, h,
/// u, v, -, -, clut, mode): position word with the 16-bit offset (`x`,
/// `y`), size word, U/V, CLUT, then the link.
fn sprite_body(m: &mut Machine, record: u32, packet: u32, x: u32, y: u32) {
    let x = x as u16 as i16 as u32;
    let y = y as u16 as i16 as u32;
    let px = lh(m, record).wrapping_add(x) & 0xffff;
    let py = lh(m, off(record, 2)).wrapping_add(y) << 16;
    m.sw(off(packet, 8), px | py);
    let h = lh(m, off(record, 6)) << 16;
    let w = lhu(m, off(record, 4));
    m.sw(off(packet, 0x10), w | h);
    sprite_uv(m, record, packet);
    let clut = lhu(m, off(record, 0x10));
    m.sh(off(packet, 0xe), clut as u16);
    link_current(m, packet);
}

/// 800D4058 (command 8): DR_MODE from `SetDrawMode(p, 0, 0, [+4], NULL)`.
pub fn command_draw_mode(m: &mut Machine, command: u32) {
    let frame = m.push_frame(0x28);
    let visual = visual(m);
    let packet = m.lw(off(visual, 4));
    m.sw(off(visual, 4), packet.wrapping_add(0xc));
    m.sw(off(frame, 0x10), 0);
    let tpage = m.lw(off(command, 4));
    dispatch::call(m, SET_DRAW_MODE, [packet, 0, 0, tpage]);
    link_current(m, packet);
    m.pop_frame(0x28);
}

/// 800AAC38 SetDrawMode (libgpu, owned by `sdk.rs`): fifth argument at
/// `sp + 0x10`.
pub const SET_DRAW_MODE: u32 = 0x800a_ac38;

/// 800D3AE0 (command 9): textured quadrilateral (POLY_FT4 0x2C, 0x2E
/// blended) from the sprite record at +4, rotated through 800D33C8 when a
/// rotation record is selected (visual +0x49C).
pub fn command_quad(m: &mut Machine, command: u32) {
    let frame = m.push_frame(0x38);
    let ctx = context(m);
    let visual = visual(m);
    let record = off(command, 4);
    let rect = off(ctx, 0x38);
    let color_out = off(ctx, 0x40);
    let slot = m.lw(off(ctx, 0x18));
    let color = m.lw(off(ctx, 0x14));
    let tx = lhu(m, off(ctx, 4));
    let offset = off(visual, slot << 2);
    let ox = lhu(m, off(offset, 0x298));
    let oy = lhu(m, off(offset, 0x29a));
    let ty = lhu(m, off(ctx, 6));
    copy_word(m, off(off(visual, color << 2), 0x98), off(ctx, 0x40));
    let anim = off(ctx, 0x28);
    let mode = lh(m, off(record, 0x12));
    let x = ox.wrapping_add(tx);
    let y = oy.wrapping_add(ty);
    let scale = match mode {
        1 => 2,
        2 => 1,
        _ => 4,
    };
    let v = lhu(m, record).wrapping_add(x);
    m.sh(rect, v as u16);
    let v = lhu(m, off(record, 2)).wrapping_add(y);
    m.sh(off(rect, 2), v as u16);
    let size = m.lw(off(record, 4));
    m.sw(off(rect, 4), size);
    let visual = self::visual(m);
    copy_animation(m, visual, ctx);
    let result = if lh(m, off(anim, 2)) != 0 {
        let ctx2 = context(m);
        let v = lhu(m, off(anim, 4)).wrapping_add(lhu(m, off(ctx2, 8)));
        m.sh(off(anim, 4), v as u16);
        let v = lhu(m, off(anim, 6)).wrapping_add(lhu(m, off(ctx2, 0xa)));
        m.sh(off(anim, 6), v as u16);
        animate(m, rect, anim)
    } else {
        1
    };
    if result == 1 {
        let visual = self::visual(m);
        let packet = m.lw(off(visual, 4));
        m.sw(off(visual, 4), packet.wrapping_add(0x28));
        let rgb = {
            let r = lbu(m, color_out);
            let g = lbu(m, off(color_out, 1));
            let b = lbu(m, off(color_out, 2));
            r | g << 8 | b << 16
        };
        m.sb(off(packet, 3), 9);
        m.sw(off(packet, 4), rgb);
        m.sb(off(packet, 7), 0x2c);
        let ctx2 = context(m);
        let tpage = if m.lw(off(ctx2, 0x1c)) == 0xffff_ffff {
            let depth = (lhu(m, off(record, 0x12)) & 3) << 7;
            let v = lhu(m, off(record, 0xa));
            let u = lhu(m, off(record, 8));
            let page_y = sra((v & 0x100) << 16, 20);
            depth | page_y | ((u & 0x3ff) >> 6) | ((v & 0x200) << 2)
        } else {
            m.sb(off(packet, 7), 0x2e);
            let depth = (lhu(m, off(record, 0x12)) & 3) << 7;
            let ctx2 = context(m);
            let v = lhu(m, off(record, 0xa));
            let u = lhu(m, off(record, 8));
            let page_x = (u & 0x3ff) >> 6;
            let blend = (lhu(m, off(ctx2, 0x1c)) & 3) << 5;
            let page_y = sra((v & 0x100) << 16, 20);
            depth | blend | page_y | page_x | ((v & 0x200) << 2)
        };
        m.sh(off(packet, 0x16), tpage as u16);
        let clut = lhu(m, off(record, 0x10));
        let visual = self::visual(m);
        m.sh(off(packet, 0xe), clut as u16);
        let rotation = m.lw(off(visual, 0x49c));
        if rotation == 0 {
            let x0 = lhu(m, rect);
            let y0 = lh(m, off(rect, 2)) << 16;
            m.sw(off(packet, 8), y0 | x0);
            let y0 = lh(m, off(rect, 2)) << 16;
            let x1 = lh(m, rect).wrapping_add(lh(m, off(rect, 4))) & 0xffff;
            m.sw(off(packet, 0x10), y0 | x1);
            let y1 = lh(m, off(rect, 2)).wrapping_add(lh(m, off(rect, 6))) << 16;
            let x0 = lhu(m, rect);
            m.sw(off(packet, 0x18), y1 | x0);
            let y1 = lh(m, off(rect, 2)).wrapping_add(lh(m, off(rect, 6))) << 16;
            let x1 = lh(m, rect).wrapping_add(lh(m, off(rect, 4))) & 0xffff;
            m.sw(off(packet, 0x20), y1 | x1);
        } else {
            let source = off(off(visual, rotation << 3), 0x418);
            copy_word(m, source, off(frame, 0x10));
            copy_word(m, off(source, 4), off(frame, 0x14));
            let v = lhu(m, off(frame, 0x12)).wrapping_add(x);
            m.sh(off(frame, 0x12), v as u16);
            let v = lhu(m, off(frame, 0x14)).wrapping_add(y);
            let visual = self::visual(m);
            m.sh(off(frame, 0x14), v as u16);
            let mode = m.lw(off(visual, 0x49c));
            quad_corners(m, packet, rect, off(frame, 0x10), mode);
        }
        let v = rem(lh(m, off(record, 0xa)), 256) << 8;
        let u = sx16(rem(lh(m, off(record, 8)), 64)).wrapping_mul(scale);
        m.sh(off(packet, 0xc), (v | (u & 0xff)) as u16);
        let v = rem(lh(m, off(record, 0xa)), 256) << 8;
        let u = sx16(rem(lh(m, off(record, 8)), 64)).wrapping_mul(scale);
        let w = lh(m, off(record, 0xc)).wrapping_mul(scale);
        let u1 = u.wrapping_sub(1).wrapping_add(w);
        m.sh(off(packet, 0x14), (v | (u1 & 0xff)) as u16);
        let vm = rem(lh(m, off(record, 0xa)), 256).wrapping_sub(1);
        let v1 = lhu(m, off(record, 0xe)).wrapping_add(vm) << 8;
        let u = sx16(rem(lh(m, off(record, 8)), 64)).wrapping_mul(scale);
        m.sh(off(packet, 0x1c), (v1 | (u & 0xff)) as u16);
        let vm = rem(lh(m, off(record, 0xa)), 256).wrapping_sub(1);
        let v1 = lhu(m, off(record, 0xe)).wrapping_add(vm) << 8;
        let u = sx16(rem(lh(m, off(record, 8)), 64)).wrapping_mul(scale);
        let w = lh(m, off(record, 0xc)).wrapping_mul(scale);
        let u1 = u.wrapping_sub(1).wrapping_add(w);
        m.sh(off(packet, 0x24), (v1 | (u1 & 0xff)) as u16);
        link_current(m, packet);
    }
    m.pop_frame(0x38);
}

fn sx16(value: u32) -> u32 {
    value as u16 as i16 as u32
}

/// 800D499C (command 10): translation (context +4/+6) += halfwords +4/+6.
pub fn command_translate(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let dx = lhu(m, off(command, 4));
    let x = lhu(m, off(ctx, 4)).wrapping_add(dx);
    m.sh(off(ctx, 4), x as u16);
    let y = lhu(m, off(ctx, 6));
    let dy = lhu(m, off(command, 6));
    m.sh(off(ctx, 6), y.wrapping_add(dy) as u16);
}

/// 800D4AB0 (command 11): call script [+4] translated by +6/+8. With the
/// gate halfword +0xA equal to 1 the call needs the case value to match
/// the selected value and, while an animation record is selected, the
/// record's time (+0xE) to be past its first key (+8). A call with no
/// animation offset (context +8 word zero) first records the translation
/// there.
pub fn command_call(m: &mut Machine, command: u32) {
    m.push_frame(0x20);
    let visual = visual(m);
    let index = m.lw(off(visual, 0x498));
    let args = off(command, 4);
    let record = off(visual, (index << 4).wrapping_add(0x318));
    let gated = lh(m, off(args, 6)) == 1;
    let run = if gated {
        let ctx = context(m);
        let case = lh(m, off(ctx, 0xe));
        let value = m.lw(off(ctx, 0x10));
        if value != case {
            false
        } else if index != 0 {
            let time = lh(m, off(record, 0xe));
            let first = lh(m, off(record, 8));
            (first as i32) < (time as i32)
        } else {
            true
        }
    } else {
        true
    };
    if run {
        let ctx = context(m);
        let dx = lhu(m, off(args, 2));
        let x = lhu(m, off(ctx, 4)).wrapping_add(dx);
        m.sh(off(ctx, 4), x as u16);
        let y = lhu(m, off(ctx, 6));
        let dy = lhu(m, off(args, 4));
        let anim = m.lw(off(ctx, 8));
        m.sh(off(ctx, 6), y.wrapping_add(dy) as u16);
        if anim == 0 {
            let x = lhu(m, off(ctx, 4));
            let y = lhu(m, off(ctx, 6));
            m.sh(off(ctx, 8), x as u16);
            m.sh(off(ctx, 0xa), y as u16);
        }
        let script = lh(m, args);
        run_script(m, script);
        let ctx = context(m);
        let dx = lhu(m, off(args, 2));
        let x = lhu(m, off(ctx, 4)).wrapping_sub(dx);
        m.sh(off(ctx, 4), x as u16);
        let y = lhu(m, off(ctx, 6));
        let dy = lhu(m, off(args, 4));
        m.sh(off(ctx, 0xa), 0);
        m.sh(off(ctx, 8), 0);
        m.sh(off(ctx, 6), y.wrapping_sub(dy) as u16);
    }
    m.pop_frame(0x20);
}

/// 800D4988 (command 12): offset slot (context +0x18) = word +4.
pub fn command_offset_slot(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let value = m.lw(off(command, 4));
    m.sw(off(ctx, 0x18), value);
}

/// 800D49D0 (command 13): animation record [+4] = the seven halfwords +4..
/// +0x10 (its index halfword first).
pub fn command_set_animation(m: &mut Machine, command: u32) {
    let index = lh(m, off(command, 4));
    let first = lhu(m, off(command, 4));
    let args = off(command, 4);
    let visual = visual(m);
    let record = off(visual, (index << 4).wrapping_add(0x318));
    m.sh(record, first as u16);
    for k in 1..7 {
        let v = lhu(m, off(args, 2 * k));
        m.sh(off(record, 2 * k), v as u16);
    }
}

/// 800D4A3C (command 14): select animation record [+4]; clear the context
/// animation offset.
pub fn command_select_animation(m: &mut Machine, command: u32) {
    let visual = visual(m);
    let value = m.lw(off(command, 4));
    let ctx = context(m);
    m.sw(off(visual, 0x498), value);
    m.sh(off(ctx, 0xa), 0);
    m.sh(off(ctx, 8), 0);
}

/// 800D4A60 (command 15): rotation record [+4] = the 8 bytes at +4.
pub fn command_set_rotation(m: &mut Machine, command: u32) {
    let index = lh(m, off(command, 4));
    let visual = visual(m);
    let record = off(visual, index << 3);
    copy_word(m, off(command, 4), off(record, 0x418));
    copy_word(m, off(command, 8), off(record, 0x41c));
}

/// 800D4A9C (command 16): select rotation record [+4].
pub fn command_select_rotation(m: &mut Machine, command: u32) {
    let visual = visual(m);
    let value = m.lw(off(command, 4));
    m.sw(off(visual, 0x49c), value);
}

/// The preamble of the line commands 17..19: animation gate, then the
/// colour; returns false when the gate suppresses the command.
fn line_prelude(m: &mut Machine) -> Option<u32> {
    let visual = visual(m);
    let ctx = context(m);
    copy_animation(m, visual, ctx);
    if animation_waiting(m, ctx) {
        return None;
    }
    let ctx2 = context(m);
    let color = m.lw(off(ctx2, 0x14));
    let visual = self::visual(m);
    copy_word(m, off(off(visual, color << 2), 0x98), off(ctx, 0x40));
    Some(ctx)
}

/// Line vertices `count` from +4: each translated by context +4/+6.
fn line_vertices(m: &mut Machine, command: u32, packet: u32, count: u32) {
    let args = off(command, 4);
    let ctx = context(m);
    for k in 0..count {
        let x = lhu(m, off(args, 4 * k)).wrapping_add(lhu(m, off(ctx, 4)));
        m.sh(off(packet, 8 + 4 * k), x as u16);
        let y = lhu(m, off(args, 4 * k + 2)).wrapping_add(lhu(m, off(ctx, 6)));
        m.sh(off(packet, 0xa + 4 * k), y as u16);
    }
}

/// 800D42B4 (command 17): line from +4 to +8 (LINE_F2 0x40, 0x42 blended).
pub fn command_line(m: &mut Machine, command: u32) {
    let Some(ctx) = line_prelude(m) else {
        return;
    };
    let packet = reserve(m, 0x10);
    let rgb = color_word(m, ctx);
    m.sb(off(packet, 3), 3);
    m.sw(off(packet, 4), rgb);
    m.sb(off(packet, 7), 0x40);
    let ctx2 = context(m);
    if m.lw(off(ctx2, 0x1c)) != 0xffff_ffff {
        m.sb(off(packet, 7), 0x42);
    }
    line_vertices(m, command, packet, 2);
    link_current(m, packet);
}

/// 800D4444 / 800D4608 (commands 18, 19): polyline of `count` vertices
/// (LINE_F3 0x48 / LINE_F4 0x4C, blend bit 2), 0x55555555 terminated.
fn command_polyline(m: &mut Machine, command: u32, count: u32) {
    let Some(ctx) = line_prelude(m) else {
        return;
    };
    let bytes = 0xc + 4 * count;
    let packet = reserve(m, bytes);
    let rgb = color_word(m, ctx);
    m.sb(off(packet, 3), (count + 2) as u8);
    m.sw(off(packet, bytes - 4), 0x5555_5555);
    m.sw(off(packet, 4), rgb);
    m.sb(off(packet, 7), if count == 3 { 0x48 } else { 0x4c });
    let ctx2 = context(m);
    if m.lw(off(ctx2, 0x1c)) != 0xffff_ffff {
        let code = lbu(m, off(packet, 7));
        m.sb(off(packet, 7), (code | 2) as u8);
    }
    line_vertices(m, command, packet, count);
    link_current(m, packet);
}

/// 800D4444 (command 18): three-vertex polyline.
pub fn command_line3(m: &mut Machine, command: u32) {
    command_polyline(m, command, 3);
}

/// 800D4608 (command 19): four-vertex polyline.
pub fn command_line4(m: &mut Machine, command: u32) {
    command_polyline(m, command, 4);
}

/// 800D48AC (command 20): selector halfword (context +0xC) = halfword +4;
/// selected value = its style byte.
pub fn command_select_half(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let value = lhu(m, off(command, 4));
    let visual = visual(m);
    m.sh(off(ctx, 0xc), value as u16);
    let slot = sx16(value);
    let style = lbu(m, off(off(visual, slot), 0x4a4));
    m.sw(off(ctx, 0x10), style);
}

/// 800D48DC (command 21): case value (context +0xE) = halfword +6.
pub fn command_case(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let value = lh(m, off(command, 6));
    m.sh(off(ctx, 0xe), value as u16);
}

/// 800D4940 (command 22): selected value = the case value (word +4 == 1)
/// or the style byte of the selector.
pub fn command_case_source(m: &mut Machine, command: u32) {
    if m.lw(off(command, 4)) == 1 {
        let ctx = context(m);
        let case = lh(m, off(ctx, 0xe));
        m.sw(off(ctx, 0x10), case);
    } else {
        let ctx = context(m);
        let visual = visual(m);
        let slot = lh(m, off(ctx, 0xc));
        let style = lbu(m, off(off(visual, slot), 0x4a4));
        m.sw(off(ctx, 0x10), style);
    }
}

/// 800D40CC (command 23): DR_AREA for the rectangle at +4 (offset slot and
/// translation applied, clipped to 320x512; -1 selects the full screen,
/// a rectangle wholly outside draws nothing).
pub fn command_draw_area(m: &mut Machine, command: u32) {
    let frame = m.push_frame(0x28);
    let ctx = context(m);
    let visual = visual(m);
    let slot = m.lw(off(ctx, 0x18));
    let x16 = lhu(m, off(command, 4));
    let offset = off(visual, slot << 2);
    let ox = lhu(m, off(offset, 0x298));
    let oy = lhu(m, off(offset, 0x29a));
    let ty = lhu(m, off(ctx, 6));
    let tx = lhu(m, off(ctx, 4));
    let dy = oy.wrapping_add(ty);
    let dx = ox.wrapping_add(tx);
    let rect = off(command, 4);
    let x = lh(m, rect);
    let visible = if x == 0xffff_ffff {
        m.sh(off(frame, 0x14), 0x140);
        m.sh(off(frame, 0x10), 0);
        m.sh(off(frame, 0x12), 0);
        m.sh(off(frame, 0x16), 0x1ff);
        true
    } else {
        draw_area_rect(m, command, frame, x, x16, dx, dy)
    };
    if visible {
        let visual = self::visual(m);
        let packet = m.lw(off(visual, 4));
        m.sw(off(visual, 4), packet.wrapping_add(0xc));
        dispatch::call(m, SET_DRAW_AREA, [packet, off(frame, 0x10), 0, 0]);
        link_current(m, packet);
    }
    m.pop_frame(0x28);
}

/// 800A73F0 SetDrawArea (libgpu, `sdk.rs`).
const SET_DRAW_AREA: u32 = 0x800a_73f0;

/// The clipped rectangle of command 23 into `frame + 0x10`; false when it
/// lies outside the screen. The vertical extent test uses the width (+8),
/// as the original does.
fn draw_area_rect(
    m: &mut Machine,
    command: u32,
    frame: u32,
    x: u32,
    x16: u32,
    dx: u32,
    dy: u32,
) -> bool {
    let rect = off(command, 4);
    let sdx = dx as u16 as i16 as u32;
    let left = x.wrapping_add(sdx);
    if (left as i32) >= 0x140 {
        return false;
    }
    let right = left.wrapping_add(lh(m, off(rect, 4)));
    if (right as i32) >= 0x140 {
        m.sh(off(frame, 0x10), x16.wrapping_add(dx) as u16);
        let w = 0x140u32.wrapping_sub(dx).wrapping_sub(lhu(m, rect));
        m.sh(off(frame, 0x14), w as u16);
    } else if (left as i32) >= 0 {
        m.sh(off(frame, 0x10), x16.wrapping_add(dx) as u16);
        let w = lhu(m, off(rect, 4));
        m.sh(off(frame, 0x14), w as u16);
    } else {
        if (right as i32) < 0 {
            return false;
        }
        m.sh(off(frame, 0x10), 0);
        let w = lhu(m, rect)
            .wrapping_add(dx)
            .wrapping_add(lhu(m, off(rect, 4)));
        m.sh(off(frame, 0x14), w as u16);
    }
    let sdy = dy as u16 as i16 as u32;
    let top = lh(m, off(rect, 2)).wrapping_add(sdy);
    let y16 = lhu(m, off(rect, 2));
    if (top as i32) >= 0x200 {
        return false;
    }
    if (top.wrapping_add(lh(m, off(rect, 4))) as i32) >= 0x200 {
        m.sh(off(frame, 0x12), y16.wrapping_add(dy) as u16);
        let h = 0x1ffu32.wrapping_sub(dy).wrapping_sub(lhu(m, off(rect, 2)));
        m.sh(off(frame, 0x16), h as u16);
    } else if (top as i32) >= 0 {
        m.sh(off(frame, 0x12), y16.wrapping_add(dy) as u16);
        let h = lhu(m, off(rect, 6));
        m.sh(off(frame, 0x16), h as u16);
    } else {
        if (top.wrapping_add(lh(m, off(rect, 6))) as i32) < 0 {
            return false;
        }
        m.sh(off(frame, 0x12), 0);
        let h = lhu(m, off(rect, 2))
            .wrapping_add(dy)
            .wrapping_add(lhu(m, off(rect, 6)));
        m.sh(off(frame, 0x16), h as u16);
    }
    true
}

/// 800D48F0 (command 24): case value = halfword +4 - 1; visual flag 8.
pub fn command_case_minus_one(m: &mut Machine, command: u32) {
    let ctx = context(m);
    let value = lhu(m, off(command, 4));
    let visual = visual(m);
    m.sh(off(ctx, 0xe), value.wrapping_sub(1) as u16);
    let flags = m.lw(visual);
    m.sw(visual, flags | 8);
}

/// 800D4920 (command 25): clear visual flag 8.
pub fn command_clear_flag(m: &mut Machine) {
    let visual = visual(m);
    let flags = m.lw(visual);
    m.sw(visual, flags & !8);
}

/// 800D4BBC (command 26): frame `[ctx+0x10] - first` (first at +6) of the
/// `count` (+4) 24-byte sprite records from +0xC, as command 7 plus a
/// DR_MODE for its texture page; the extra length is `6 * count` words.
pub fn command_sprite_frames(m: &mut Machine, command: u32) {
    let frame = m.push_frame(0x30);
    let count = lh(m, off(command, 4));
    let first = lh(m, off(command, 6));
    let ctx = context(m);
    let value = m.lw(off(ctx, 0x10));
    m.sh(off(ctx, 0x20), count.wrapping_mul(6) as u16);
    let index3 = value.wrapping_sub(first).wrapping_mul(3);
    let in_range =
        (value as i32) >= (first as i32) && (value as i32) < (first.wrapping_add(count) as i32);
    if in_range {
        sprite_frame(m, command, ctx, index3, frame);
    }
    m.pop_frame(0x30);
}

fn sprite_frame(m: &mut Machine, command: u32, ctx: u32, index3: u32, frame: u32) {
    let visual = visual(m);
    copy_animation(m, visual, ctx);
    if animation_waiting(m, ctx) {
        return;
    }
    let ctx2 = context(m);
    let visual = self::visual(m);
    let slot = m.lw(off(ctx2, 0x18));
    let color = m.lw(off(ctx2, 0x14));
    let tx = lhu(m, off(ctx2, 4));
    let ty = lhu(m, off(ctx2, 6));
    let offset = off(visual, slot << 2);
    let ox = lhu(m, off(offset, 0x298));
    let oy = lhu(m, off(offset, 0x29a));
    copy_word(m, off(off(visual, color << 2), 0x98), off(ctx, 0x40));
    let packet = m.lw(off(visual, 4));
    m.sw(off(visual, 4), packet.wrapping_add(0x14));
    let rgb = color_word(m, ctx);
    m.sb(off(packet, 3), 4);
    let x = ox.wrapping_add(tx);
    let y = oy.wrapping_add(ty);
    m.sw(off(packet, 4), rgb);
    m.sb(off(packet, 7), 0x64);
    let entry = off(command, index3 << 3);
    let ctx2 = context(m);
    let record = off(entry, 0xc);
    if m.lw(off(ctx2, 0x1c)) != 0xffff_ffff {
        m.sb(off(packet, 7), 0x66);
    }
    sprite_body(m, record, packet, x, y);
    let visual = self::visual(m);
    let mode_packet = m.lw(off(visual, 4));
    m.sw(off(visual, 4), mode_packet.wrapping_add(0xc));
    let depth = (lhu(m, off(record, 0x12)) & 3) << 7;
    let v = lhu(m, off(record, 0xa));
    let u = lhu(m, off(record, 8));
    let ctx2 = context(m);
    let page_x = (u & 0x3ff) >> 6;
    let blend = (m.lw(off(ctx2, 0x1c)) & 3) << 5;
    m.sw(off(frame, 0x10), 0);
    let page_y = sx16(sra((v & 0x100) << 16, 20));
    let high = sx16(v & 0x200) << 2;
    let tpage = depth | blend | page_y | page_x | high;
    dispatch::call(m, SET_DRAW_MODE, [mode_packet, 0, 0, tpage]);
    link_current(m, mode_packet);
}

/// 800D4F9C (command 27): frame `[ctx+0x10] - first` of the `count` 12-byte
/// rectangles from +0xC, drawn as command 1; extra length `3 * count`.
pub fn command_tile_frames(m: &mut Machine, command: u32) {
    m.push_frame(0x30);
    let count = lh(m, off(command, 4));
    let first = lh(m, off(command, 6));
    let ctx = context(m);
    let value = m.lw(off(ctx, 0x10));
    m.sh(off(ctx, 0x20), count.wrapping_mul(3) as u16);
    let index3 = value.wrapping_sub(first).wrapping_mul(3);
    let in_range =
        (value as i32) >= (first as i32) && (value as i32) < (first.wrapping_add(count) as i32);
    if in_range {
        tile_frame(m, command, ctx, index3);
    }
    m.pop_frame(0x30);
}

fn tile_frame(m: &mut Machine, command: u32, ctx: u32, index3: u32) {
    let rect = off(ctx, 0x38);
    let anim = off(ctx, 0x28);
    let slot = m.lw(off(ctx, 0x18));
    let visual = visual(m);
    let color = m.lw(off(ctx, 0x14));
    let tx = lhu(m, off(ctx, 4));
    let ty = lhu(m, off(ctx, 6));
    let offset = off(visual, slot << 2);
    let entry = off(command, (index3 << 2).wrapping_add(0xc));
    let ox = lhu(m, off(offset, 0x298));
    let oy = lhu(m, off(offset, 0x29a));
    copy_word(m, off(off(visual, color << 2), 0x98), off(ctx, 0x40));
    let x = lhu(m, entry).wrapping_add(ox.wrapping_add(tx));
    m.sh(rect, x as u16);
    let y = lhu(m, off(entry, 2)).wrapping_add(oy.wrapping_add(ty));
    m.sh(off(rect, 2), y as u16);
    let size = m.lw(off(entry, 4));
    m.sw(off(rect, 4), size);
    let visual = self::visual(m);
    let ctx2 = context(m);
    copy_animation(m, visual, ctx);
    let v = lhu(m, off(anim, 4)).wrapping_add(lhu(m, off(ctx2, 8)));
    m.sh(off(anim, 4), v as u16);
    let v = lhu(m, off(anim, 6)).wrapping_add(lhu(m, off(ctx2, 0xa)));
    m.sh(off(anim, 6), v as u16);
    if animate(m, rect, anim) != 1 {
        return;
    }
    let packet = reserve(m, 0x10);
    let rgb = color_word(m, ctx);
    m.sb(off(packet, 3), 3);
    m.sw(off(packet, 4), rgb);
    m.sb(off(packet, 7), 0x60);
    let ctx2 = context(m);
    if m.lw(off(ctx2, 0x1c)) != 0xffff_ffff {
        m.sb(off(packet, 7), 0x62);
    }
    let xy = m.lw(rect);
    m.sw(off(packet, 8), xy);
    let wh = m.lw(off(rect, 4));
    m.sw(off(packet, 0xc), wh);
    link_current(m, packet);
}

// ---- animation and geometry ----------------------------------------------------------

/// 800D2D60: double the halfwords +6, +0xA, +8 (matrix row 1, for the
/// 512-line display).
pub fn double_row(m: &mut Machine, matrix: u32) {
    let a = lhu(m, off(matrix, 6));
    let b = lhu(m, off(matrix, 0xa));
    m.sh(off(matrix, 6), (a << 1) as u16);
    let c = lhu(m, off(matrix, 8));
    m.sh(off(matrix, 0xa), (b << 1) as u16);
    m.sh(off(matrix, 8), (c << 1) as u16);
}

/// 800D2D88: apply animation record `anim` (+2 type 0..4, +4/+6 origin,
/// +8/+0xA/+0xC key times, +0xE time) to the rectangle `out` {x, y, w, h}.
/// Returns 1 to draw, 0 before the first key time (type 4: time past the
/// last key). The divisions keep the compiler's `break 7` / `break 6`
/// checks (raised as [`Fault::Trap`](crate::Fault::Trap)).
pub fn animate(m: &mut Machine, out: u32, anim: u32) -> u32 {
    let kind = lh(m, off(anim, 2));
    if kind >= 5 {
        return 1;
    }
    match kind {
        0 => 1,
        1 => animate_grow(m, out, anim),
        2 => animate_slide(m, out, anim),
        3 => animate_zoom(m, out, anim),
        _ => {
            let time = lh(m, off(anim, 0xe)) as i32;
            let last = lh(m, off(anim, 0xc)) as i32;
            u32::from(time >= last)
        }
    }
}

fn mul(a: u32, b: u32) -> u32 {
    a.wrapping_mul(b)
}

/// Type 1 (800D2DC8).
fn animate_grow(m: &mut Machine, out: u32, anim: u32) -> u32 {
    let time = lh(m, off(anim, 0xe));
    let t0 = lh(m, off(anim, 8));
    if (t0 as i32) >= (time as i32) {
        return 0;
    }
    let t1 = lh(m, off(anim, 0xa));
    if (t1 as i32) >= (time as i32) {
        let dx = lh(m, out).wrapping_sub(lh(m, off(anim, 4)));
        let product = mul(dx, time.wrapping_sub(t0));
        let q = div_trap(m, product, t1.wrapping_sub(t0), 0x800d_2e2c, 0x800d_2e44);
        let x = lhu(m, off(anim, 4)).wrapping_add(q);
        m.sh(out, x as u16);
        let y = lh(m, off(out, 2));
        let oy = lh(m, off(anim, 6));
        let origin = lhu(m, off(anim, 6));
        if y == oy {
            m.sh(off(out, 2), origin as u16);
        } else {
            let d = y.wrapping_sub(oy);
            let magnitude = (d as i32).wrapping_abs() as u32;
            let step = div_trap(m, d, magnitude, 0x800d_2e88, 0x800d_2ea0);
            m.sh(off(out, 2), origin.wrapping_add(step) as u16);
        }
        let time = lh(m, off(anim, 0xe));
        let t0 = lh(m, off(anim, 8));
        let w = lh(m, off(out, 4));
        let product = mul(w, time.wrapping_sub(t0));
        let t1 = lh(m, off(anim, 0xa));
        let q = div_trap(m, product, t1.wrapping_sub(t0), 0x800d_2eec, 0x800d_2f04);
        m.sh(off(out, 6), 2);
        m.sh(off(out, 4), q as u16);
        return 1;
    }
    let t2 = lh(m, off(anim, 0xc));
    if (time as i32) >= (t2 as i32) {
        return 1;
    }
    let dy = lh(m, off(out, 2)).wrapping_sub(lh(m, off(anim, 6)));
    let product = mul(dy, time.wrapping_sub(t1));
    let q = div_trap(m, product, t2.wrapping_sub(t1), 0x800d_2f64, 0x800d_2f7c);
    let y = lhu(m, off(anim, 6)).wrapping_add(q);
    m.sh(off(out, 2), y as u16);
    let time = lh(m, off(anim, 0xe));
    let t1 = lh(m, off(anim, 0xa));
    let h = lh(m, off(out, 6));
    let product = mul(h, time.wrapping_sub(t1));
    let t2 = lh(m, off(anim, 0xc));
    let q = div_trap(m, product, t2.wrapping_sub(t1), 0x800d_2fc4, 0x800d_2fdc);
    m.sh(off(out, 6), q as u16);
    1
}

/// Type 2 (800D2FF0).
fn animate_slide(m: &mut Machine, out: u32, anim: u32) -> u32 {
    let time = lh(m, off(anim, 0xe));
    let t0 = lh(m, off(anim, 8));
    if (t0 as i32) >= (time as i32) {
        return 0;
    }
    let t1 = lh(m, off(anim, 0xa));
    if (time as i32) < (t1 as i32) {
        let d = lh(m, out).wrapping_sub(lh(m, off(anim, 4)));
        let magnitude = (d as i32).wrapping_abs() as u32;
        let step = div_trap(m, d, magnitude, 0x800d_3044, 0x800d_305c);
        let x = lhu(m, off(anim, 4)).wrapping_add(step);
        let y = lh(m, off(out, 2));
        m.sh(out, x as u16);
        let oy = lh(m, off(anim, 6));
        let time = lh(m, off(anim, 0xe));
        let t0 = lh(m, off(anim, 8));
        let product = mul(y.wrapping_sub(oy), time.wrapping_sub(t0));
        let t1 = lh(m, off(anim, 0xa));
        let q = div_trap(m, product, t1.wrapping_sub(t0), 0x800d_30ac, 0x800d_30c4);
        let origin = lhu(m, off(anim, 6));
        m.sh(off(out, 4), 1);
        m.sh(off(out, 2), origin.wrapping_add(q) as u16);
        let time = lh(m, off(anim, 0xe));
        let t0 = lh(m, off(anim, 8));
        let h = lh(m, off(out, 6));
        let product = mul(h, time.wrapping_sub(t0));
        let t1 = lh(m, off(anim, 0xa));
        let q = div_trap(m, product, t1.wrapping_sub(t0), 0x800d_3110, 0x800d_3128);
        m.sh(off(out, 6), q as u16);
        return 1;
    }
    let t2 = lh(m, off(anim, 0xc));
    if (time as i32) >= (t2 as i32) {
        return 1;
    }
    let dx = lh(m, out).wrapping_sub(lh(m, off(anim, 4)));
    let product = mul(dx, time.wrapping_sub(t1));
    let q = div_trap(m, product, t2.wrapping_sub(t1), 0x800d_317c, 0x800d_3194);
    let x = lhu(m, off(anim, 4)).wrapping_add(q);
    m.sh(out, x as u16);
    let time = lh(m, off(anim, 0xe));
    let t1 = lh(m, off(anim, 0xa));
    let w = lh(m, off(out, 4));
    let product = mul(w, time.wrapping_sub(t1));
    let t2 = lh(m, off(anim, 0xc));
    let q = div_trap(m, product, t2.wrapping_sub(t1), 0x800d_31dc, 0x800d_31f4);
    m.sh(off(out, 4), q as u16);
    1
}

/// Type 3 (800D3204).
fn animate_zoom(m: &mut Machine, out: u32, anim: u32) -> u32 {
    let time = lh(m, off(anim, 0xe));
    let t0 = lh(m, off(anim, 8));
    if (t0 as i32) >= (time as i32) {
        return 0;
    }
    let t1 = lh(m, off(anim, 0xa));
    if (time as i32) >= (t1 as i32) {
        return 1;
    }
    let dx = lh(m, out).wrapping_sub(lh(m, off(anim, 4)));
    let product = mul(dx, time.wrapping_sub(t0));
    let q = div_trap(m, product, t1.wrapping_sub(t0), 0x800d_3260, 0x800d_3278);
    let x = lhu(m, off(anim, 4)).wrapping_add(q);
    let y = lh(m, off(out, 2));
    m.sh(out, x as u16);
    let oy = lh(m, off(anim, 6));
    let time = lh(m, off(anim, 0xe));
    let t0 = lh(m, off(anim, 8));
    let product = mul(y.wrapping_sub(oy), time.wrapping_sub(t0));
    let t1 = lh(m, off(anim, 0xa));
    let q = div_trap(m, product, t1.wrapping_sub(t0), 0x800d_32c8, 0x800d_32e0);
    let y = lhu(m, off(anim, 6)).wrapping_add(q);
    m.sh(off(out, 2), y as u16);
    let time = lh(m, off(anim, 0xe));
    let t0 = lh(m, off(anim, 8));
    let w = lh(m, off(out, 4));
    let product = mul(w, time.wrapping_sub(t0));
    let t2 = lh(m, off(anim, 0xc));
    let q = div_trap(m, product, t2.wrapping_sub(t0), 0x800d_3328, 0x800d_3340);
    m.sh(off(out, 4), q as u16);
    let time = lh(m, off(anim, 0xe));
    let t0 = lh(m, off(anim, 8));
    let h = lh(m, off(out, 6));
    let product = mul(h, time.wrapping_sub(t0));
    let t1 = lh(m, off(anim, 0xa));
    let q = div_trap(m, product, t1.wrapping_sub(t0), 0x800d_3380, 0x800d_3398);
    m.sh(off(out, 6), q as u16);
    1
}

/// 800D33C8: the four corners of a quadrilateral primitive `prim` (xy at
/// +8, +0x10, +0x18, +0x20) for the rectangle `rect` {x, y, w, h}. Mode 0:
/// the axis-aligned corners, returns 2. Otherwise rotation record
/// `rotation` {-, cx, cy, angle}: RotMatrix about z by `angle % 4096`,
/// translated to (cx, cy), row 1 doubled, and the corners (inset by 2 on
/// the far edges) relative to the centre through RotTrans; returns 1.
pub fn quad_corners(m: &mut Machine, prim: u32, rect: u32, rotation: u32, mode: u32) -> u32 {
    if mode == 0 {
        let x = lhu(m, rect);
        m.sh(off(prim, 8), x as u16);
        let y = lhu(m, off(rect, 2));
        m.sh(off(prim, 0xa), y as u16);
        let v = lhu(m, rect).wrapping_add(lhu(m, off(rect, 4)));
        m.sh(off(prim, 0x10), v as u16);
        let y = lhu(m, off(rect, 2));
        m.sh(off(prim, 0x12), y as u16);
        let x = lhu(m, rect);
        m.sh(off(prim, 0x18), x as u16);
        let v = lhu(m, off(rect, 2)).wrapping_add(lhu(m, off(rect, 6)));
        m.sh(off(prim, 0x1a), v as u16);
        let v = lhu(m, rect).wrapping_add(lhu(m, off(rect, 4)));
        m.sh(off(prim, 0x20), v as u16);
        let v = lhu(m, off(rect, 2)).wrapping_add(lhu(m, off(rect, 6)));
        m.sh(off(prim, 0x22), v as u16);
        return 2;
    }
    let sp = m.push_frame(0xc8);
    m.sh(off(sp, 0x10), 0);
    m.sh(off(sp, 0x12), 0);
    let angle = lh(m, off(rotation, 6));
    m.sh(off(sp, 0x14), rem(angle, 4096) as u16);
    let cx = lh(m, off(rotation, 2));
    m.sw(off(sp, 0x18), cx);
    let cy = lh(m, off(rotation, 4));
    m.sw(off(sp, 0x20), 0);
    m.sw(off(sp, 0x1c), cy);
    let matrix = off(sp, 0x28);
    dispatch::call(m, ROT_MATRIX, [off(sp, 0x10), matrix, 0, 0]);
    dispatch::call(m, TRANS_MATRIX, [matrix, off(sp, 0x18), 0, 0]);
    double_row(m, matrix);
    dispatch::call(m, SET_ROT_MATRIX, [matrix, 0, 0, 0]);
    dispatch::call(m, SET_TRANS_MATRIX, [matrix, 0, 0, 0]);
    let flag = off(sp, 0xa8);
    let x = lhu(m, rect);
    let y = lhu(m, off(rect, 2));
    let w = lhu(m, off(rect, 4));
    let cx = lhu(m, off(rotation, 2));
    let h = lhu(m, off(rect, 6));
    m.sh(off(sp, 0x48), x.wrapping_sub(cx) as u16);
    let cy = lhu(m, off(rotation, 4));
    let right = x.wrapping_add(w);
    m.sh(off(sp, 0x4c), 0);
    m.sh(off(sp, 0x4a), y.wrapping_sub(cy) as u16);
    let cx = lhu(m, off(rotation, 2));
    let right = right.wrapping_sub(2);
    m.sh(off(sp, 0x50), right.wrapping_sub(cx) as u16);
    let cy = lhu(m, off(rotation, 4));
    m.sh(off(sp, 0x54), 0);
    m.sh(off(sp, 0x52), y.wrapping_sub(cy) as u16);
    let cx = lhu(m, off(rotation, 2));
    let bottom = y.wrapping_add(h);
    m.sh(off(sp, 0x58), x.wrapping_sub(cx) as u16);
    let cy = lhu(m, off(rotation, 4));
    let bottom = bottom.wrapping_sub(2);
    m.sh(off(sp, 0x5c), 0);
    m.sh(off(sp, 0x5a), bottom.wrapping_sub(cy) as u16);
    let cx = lhu(m, off(rotation, 2));
    m.sh(off(sp, 0x60), right.wrapping_sub(cx) as u16);
    let cy = lhu(m, off(rotation, 4));
    m.sh(off(sp, 0x64), 0);
    m.sh(off(sp, 0x62), bottom.wrapping_sub(cy) as u16);
    for k in 0..4 {
        dispatch::call(
            m,
            ROT_TRANS,
            [off(sp, 0x48 + 8 * k), off(sp, 0x68 + 0x10 * k), flag, 0],
        );
    }
    for k in 0..4 {
        let x = lhu(m, off(sp, 0x68 + 0x10 * k));
        m.sh(off(prim, 8 + 8 * k), x as u16);
        let y = lhu(m, off(sp, 0x6c + 0x10 * k));
        m.sh(off(prim, 0xa + 8 * k), y as u16);
    }
    m.pop_frame(0xc8);
    1
}

/// libgte (`sdk.rs`): RotMatrix, TransMatrix, SetRotMatrix,
/// SetTransMatrix, RotTrans.
const ROT_MATRIX: u32 = 0x800a_5958;
const TRANS_MATRIX: u32 = 0x800a_53a8;
const SET_ROT_MATRIX: u32 = 0x800a_5518;
const SET_TRANS_MATRIX: u32 = 0x800a_5578;
const ROT_TRANS: u32 = 0x800a_58b8;

// ---- visual state -----------------------------------------------------------------

/// 800D52DC: style byte `slot` = `value`.
pub fn set_style(m: &mut Machine, slot: u32, value: u32) {
    let visual = visual(m);
    m.sb(off(off(visual, slot), 0x4a4), value as u8);
}

/// 800D52F4 (unreferenced): colour `slot` = bytes `r`, `g`, `b`.
pub fn set_color(m: &mut Machine, slot: u32, r: u32, g: u32, b: u32) {
    let entry = slot << 2;
    let visual = visual(m);
    m.sb(off(off(visual, entry), 0x98), r as u8);
    let visual = self::visual(m);
    m.sb(off(off(visual, entry), 0x99), g as u8);
    let visual = self::visual(m);
    m.sb(off(off(visual, entry), 0x9a), b as u8);
}

/// 800D532C: colour `slot` = `from` + (`to` - `from`) * |frame % 50 - 25| /
/// 25 per component (frame = context +0x20, signed divisions by the
/// compiler's reciprocal multiplications).
pub fn pulse_color(m: &mut Machine, slot: u32, from: u32, to: u32) {
    let context = m.gw(crate::common::CONTEXT);
    let frame = m.lw(off(context, 0x20));
    let r0 = lbu(m, from);
    let q = div25(frame, 4);
    let phase = frame.wrapping_sub(q.wrapping_mul(50)).wrapping_sub(0x19);
    let weight = (phase as i32).wrapping_abs() as u32;
    let entry = slot << 2;
    for k in 0..3 {
        let start = if k == 0 { r0 } else { lbu(m, off(from, k)) };
        let end = lbu(m, off(to, k));
        let product = end.wrapping_sub(start).wrapping_mul(weight);
        let step = div25(product, 3);
        let visual = visual(m);
        m.sb(
            off(off(visual, entry), 0x98 + k),
            start.wrapping_add(step) as u8,
        );
    }
}

/// `hi(x * 0x51EB851F) >> shift` minus the sign word: the compiler's signed
/// division by 50 (`shift` 4) or 25 (`shift` 3).
fn div25(x: u32, shift: u32) -> u32 {
    let hi = ((i64::from(x as i32) * 0x51eb_851f) >> 32) as u32;
    sra(hi, shift).wrapping_sub(sra(x, 31))
}

/// 800D5458: offset `slot` = (`x`, `y`).
pub fn set_offset(m: &mut Machine, slot: u32, x: u32, y: u32) {
    let visual = visual(m);
    let entry = off(visual, slot << 2);
    m.sh(off(entry, 0x298), x as u16);
    m.sh(off(entry, 0x29a), y as u16);
}

/// 800D5474: read offset `slot` into the halfwords at `x` and `y`.
pub fn get_offset(m: &mut Machine, slot: u32, x: u32, y: u32) {
    let visual = visual(m);
    let v = lhu(m, off(off(visual, slot << 2), 0x298));
    m.sh(x, v as u16);
    let visual = self::visual(m);
    let v = lhu(m, off(off(visual, slot << 2), 0x29a));
    m.sh(y, v as u16);
}

/// 800D54A8 (unreferenced): animation record `index` time (+0xE) = `time`.
pub fn set_animation_time(m: &mut Machine, index: u32, time: u32) {
    let visual = visual(m);
    m.sh(off(off(visual, index << 4), 0x326), time as u16);
}

/// 800D54C0: rotation record `index` angle (+6) = `angle`.
pub fn set_rotation_angle(m: &mut Machine, index: u32, angle: u32) {
    let visual = visual(m);
    m.sh(off(off(visual, index << 3), 0x41e), angle as u16);
}

/// 800D54D8: page (the script [`draw_visuals`] runs) = `page`.
pub fn select_page(m: &mut Machine, page: u32) {
    let visual = visual(m);
    m.sw(off(visual, 0x4a0), page);
}

/// 800D54E8: allocate and reset the visual block: styles 0, colours
/// (0x80, 0x80, 0x80), offsets 0, animation and rotation records numbered
/// with zero fields, nothing selected.
pub fn init_visuals(m: &mut Machine) {
    let block = crate::arena::alloc(m, 0x528);
    m.sw(VISUAL, block);
    m.sw(off(block, 0x90), 0);
    m.sw(block, 0);
    for i in 0..0x80 {
        let visual = visual(m);
        m.sb(off(off(visual, i), 0x4a4), 0);
    }
    for i in 0..0x80 {
        let visual = visual(m);
        let entry = off(visual, i << 2);
        m.sb(off(entry, 0x9a), 0x80);
        m.sb(off(entry, 0x99), 0x80);
        m.sb(off(entry, 0x98), 0x80);
    }
    let visual = self::visual(m);
    for i in 0..0x20 {
        let entry = off(visual, 4 * i);
        m.sh(off(entry, 0x29a), 0);
        m.sh(off(entry, 0x298), 0);
    }
    let visual = self::visual(m);
    m.sw(off(visual, 0x498), 0);
    for i in 0..0x10 {
        let entry = off(visual, 0x10 * i);
        m.sh(off(entry, 0x318), i as u16);
        for field in [0x326, 0x324, 0x322, 0x320, 0x31e, 0x31c, 0x31a] {
            m.sh(off(entry, field), 0);
        }
    }
    let visual = self::visual(m);
    m.sw(off(visual, 0x49c), 0);
    for i in 0..0x10 {
        let entry = off(visual, 8 * i);
        m.sh(off(entry, 0x418), i as u16);
        m.sh(off(entry, 0x41e), 0);
        m.sh(off(entry, 0x41c), 0);
        m.sh(off(entry, 0x41a), 0);
    }
}

/// 800D5608: install the script table `table` (relocated by 80026E98) or,
/// for 0, remove it.
pub fn set_scripts(m: &mut Machine, table: u32) {
    m.push_frame(0x18);
    if table == 0 {
        let visual = visual(m);
        let flags = m.lw(visual);
        m.sw(off(visual, 0x524), 0);
        m.sw(visual, flags & !1);
    } else {
        let relocated = dispatch::call(m, RELOCATE_DIRECTORY, [table, 0, 0, 0]);
        let visual = visual(m);
        let flags = m.lw(visual);
        m.sw(off(visual, 0x524), relocated);
        m.sw(visual, flags | 1);
    }
    m.pop_frame(0x18);
}

/// 80026E98 (`loading.rs`): relocate an offset table in place.
pub const RELOCATE_DIRECTORY: u32 = 0x8002_6e98;

/// 800D566C: select the visual ordering table of the context's buffer
/// (+4), clear it (ClearOTagR, 16 entries) and splice it in front of the
/// packet `link`: the table's first entry takes `link`'s address and
/// `link` points at the table's last entry.
pub fn begin_visual_frame(m: &mut Machine, link: u32) {
    m.push_frame(0x20);
    let context = m.gw(crate::common::CONTEXT);
    let visual = visual(m);
    let buffer = m.lw(off(context, 4));
    m.sw(off(visual, 0x90), buffer);
    let ot = off(visual, (buffer << 6).wrapping_add(0x10));
    m.sw(off(visual, 8), ot);
    dispatch::call(m, CLEAR_OTAG_R, [ot, 0x10, 0, 0]);
    let visual = self::visual(m);
    let ot = m.lw(off(visual, 8));
    let target = m.lw(link);
    let head = m.lw(ot);
    m.sw(ot, (head & 0xff00_0000) | (target & 0x00ff_ffff));
    let word = m.lw(link);
    let ot = m.lw(off(visual, 8));
    m.sw(
        link,
        (word & 0xff00_0000) | (ot.wrapping_add(0x3c) & 0x00ff_ffff),
    );
    m.pop_frame(0x20);
}

/// 800A6B50 ClearOTagR (`sdk.rs`).
pub const CLEAR_OTAG_R: u32 = 0x800a_6b50;

/// 800D570C (unreferenced): the visual ordering table pointer (+8).
pub fn visual_table(m: &mut Machine) -> u32 {
    let visual = visual(m);
    m.lw(off(visual, 8))
}

/// 800D5724: run the page script with packets from `packet`, then add the
/// two 320x4 background tiles (rows -1 and 511, colour of the display block
/// +0x10..+0x12) at ordering-table entry 15. Returns the packet pointer.
pub fn draw_visuals(m: &mut Machine, packet: u32) -> u32 {
    m.push_frame(0x18);
    let visual = visual(m);
    m.sw(SCRIPT_CONTEXT, CONTEXT_BASE);
    m.sw(off(visual, 4), packet);
    let ctx = CONTEXT_BASE;
    m.sw(ctx, 0);
    m.sh(off(ctx, 6), 0);
    m.sh(off(ctx, 4), 0);
    m.sh(off(ctx, 0xe), 0);
    m.sh(off(ctx, 0xc), 0);
    m.sw(off(ctx, 0x1c), 0xffff_ffff);
    m.sw(off(ctx, 0x10), 0);
    m.sw(off(ctx, 0x14), 0);
    m.sw(off(ctx, 0x18), 0);
    for i in 0..0x1c {
        let ctx = context(m);
        let length = lbu(m, off(COMMAND_LENGTHS, 4 * i));
        m.sb(off(off(ctx, i), 0x44), length as u8);
        let flag = lbu(m, off(COMMAND_CONDITIONAL, i));
        let ctx = context(m);
        m.sb(off(off(ctx, i), 0x60), flag as u8);
    }
    let visual = self::visual(m);
    let ot = m.lw(off(visual, 8));
    let page = m.lw(off(visual, 0x4a0));
    m.sw(off(visual, 0xc), ot.wrapping_add(4));
    run_script(m, page);
    for y in [0xffffu16, 0x1ff] {
        let visual = self::visual(m);
        let packet = m.lw(off(visual, 4));
        m.sw(off(visual, 4), packet.wrapping_add(0x10));
        m.sb(off(packet, 3), 3);
        m.sb(off(packet, 7), 0x60);
        let display = m.gw(DISPLAY);
        let r = lbu(m, off(display, 0x10));
        m.sb(off(packet, 4), r as u8);
        let display = m.gw(DISPLAY);
        let g = lbu(m, off(display, 0x11));
        m.sb(off(packet, 5), g as u8);
        let display = m.gw(DISPLAY);
        let b = lbu(m, off(display, 0x12));
        if y == 0xffff {
            m.sb(off(packet, 6), b as u8);
            m.sh(off(packet, 8), 0);
            m.sh(off(packet, 0xa), y);
            m.sh(off(packet, 0xc), 0x140);
            m.sh(off(packet, 0xe), 4);
        } else {
            m.sh(off(packet, 8), 0);
            m.sh(off(packet, 0xc), 0x140);
            m.sh(off(packet, 0xe), 4);
            m.sb(off(packet, 6), b as u8);
            m.sh(off(packet, 0xa), y);
        }
        let visual = self::visual(m);
        let ot = m.lw(off(visual, 8));
        let tag = m.lw(packet);
        let entry = m.lw(off(ot, 0x3c));
        m.sw(packet, (tag & 0xff00_0000) | (entry & 0x00ff_ffff));
        let ot = m.lw(off(visual, 8));
        let entry = m.lw(off(ot, 0x3c));
        m.sw(
            off(ot, 0x3c),
            (entry & 0xff00_0000) | (packet & 0x00ff_ffff),
        );
    }
    let visual = self::visual(m);
    let result = m.lw(off(visual, 4));
    m.pop_frame(0x18);
    result
}
