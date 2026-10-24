//! JP overlay 2, part 2: the display-list command handlers and the step
//! routines of their animation records, translated from the JP
//! instructions.
//!
//! The display-list interpreter (800E22E4, another part) calls handler
//! `opcode` of table 0x8010AEC4 with (command body, ordering table, cell
//! record, table entry) and continues at the returned pointer. Handlers
//! here: 0 tile ([`draw_tile`]), 1 sprite ([`draw_sprite`]), 2 draw mode
//! ([`draw_mode`]), 3 textured quad ([`draw_quad`]), 4 skip
//! ([`skip_command`]), 5 animation record ([`set_record`]), 6 line
//! ([`draw_line`]).
//!
//! A drawing command may be hidden by a flag test (flag index, value
//! halfwords; index 0: always drawn). It allocates its primitive from the
//! work block's packet pointer (+4), writes its rectangle to the current
//! rectangle (`[0x80112DB4]`: x, y, w, h halfwords), runs the step routine
//! of the current animation record (work block +0x3C; table 0x8010AEAC
//! indexed by record +0xA), and is drawn only when that returns nonzero,
//! at the stepped rectangle plus the cell offset (+4/+6), into ordering
//! table slot record +0xE.
//!
//! Animation record: +0/+2 origin, +4/+6/+8 start, middle and end times,
//! +0xA step routine, +0xC fade slot (its level is the time, its mode 0
//! stops and 2 finishes the step), +0xE ordering table slot.
//!
//! The matcher listed one routine at 800DEFF0, which is the delay slot of
//! the step routine 800DEFEC (`jr ra; addiu v0, zero, 1`) followed by the
//! step routine 800DEFF4; the step table holds 800DEFEC and 800DEFF4.
//! `tools/jp/fix_map.py` splits it into those two map functions, claimed
//! here ([`call_step`] still runs both natively for the handlers of this
//! file).
use super::ov2_p2::{MODULE, lbu, less, lh, lhu, mul, quotient, sext16, work};
use crate::common::loaded_module;
use crate::machine::Machine;
use crate::{dispatch, sdk};

/// Pointer to the current rectangle (x, y, w, h halfwords).
pub const CURRENT: u32 = 0x8011_2db4;
/// Step routines of the animation records.
pub const STEPS: u32 = 0x8010_aeac;
/// Display-list command handlers (the interpreter's table).
pub const HANDLERS: u32 = 0x8010_aec4;
/// Step routine 800DEFEC: always 1 (see the module docs).
pub const STEP_ALWAYS: u32 = 0x800d_efec;
/// Step routine 800DEFF4: [`open_window`].
pub const STEP_OPEN_WINDOW: u32 = 0x800d_eff4;

fn current(m: &mut Machine) -> u32 {
    m.lw(CURRENT)
}

// ---- step routines -------------------------------------------------------------------------------

/// The animation record and its timing: (record, time, start, middle, end).
/// The time is the fade level of the record's slot.
fn timing(m: &mut Machine, block: u32) -> (u32, u32, u32, u32, u32) {
    let record = m.lw(block.wrapping_add(0x3c));
    let levels = m.lw(block.wrapping_add(0x50));
    let slot = lh(m, record.wrapping_add(0xc));
    let start = lh(m, record.wrapping_add(4));
    let middle = lh(m, record.wrapping_add(6));
    let time = lh(m, (slot << 1).wrapping_add(levels));
    let end = lh(m, record.wrapping_add(8));
    (record, time, start, middle, end)
}
/// The fade mode of the record's slot.
fn record_mode(m: &mut Machine, block: u32, record: u32) -> u32 {
    let slot = lh(m, record.wrapping_add(0xc));
    let modes = m.lw(block.wrapping_add(0x58));
    lh(m, (slot << 1).wrapping_add(modes))
}

/// 800DEFF4: open a window from the record's origin: from start to middle
/// the rectangle grows horizontally as a line 2 high (y one step from the
/// origin towards its own y), from middle to end it grows vertically.
/// Returns 0 (not drawn) while the slot is off or before start, else 1.
pub fn open_window(m: &mut Machine) -> u32 {
    let block = work(m);
    let (record, time, start, middle, end) = timing(m, block);
    let mode = record_mode(m, block, record);
    let cur = current(m);
    if mode == 0 {
        return 0;
    }
    if mode == 2 {
        return 1;
    }
    if less(time, start) {
        return 0;
    }
    if less(time, middle) {
        let dx = lh(m, cur).wrapping_sub(lh(m, record));
        let n = mul(dx, time.wrapping_sub(start));
        let q = quotient(m, n, middle.wrapping_sub(start), 0x800d_f0d8);
        let x = lhu(m, record).wrapping_add(q);
        m.sh(cur, x as u16);
        let record = m.lw(block.wrapping_add(0x3c));
        let dy = lh(m, cur.wrapping_add(2)).wrapping_sub(lh(m, record.wrapping_add(2)));
        let magnitude = if less(dy, 0) {
            0u32.wrapping_sub(dy)
        } else {
            dy
        };
        let q = quotient(m, dy, magnitude, 0x800d_f134);
        let y = lhu(m, record.wrapping_add(2)).wrapping_add(q);
        m.sh(cur.wrapping_add(2), y as u16);
        let n = mul(lh(m, cur.wrapping_add(4)), time.wrapping_sub(start));
        let q = quotient(m, n, middle.wrapping_sub(start), 0x800d_f198);
        m.sh(cur.wrapping_add(6), 2);
        m.sh(cur.wrapping_add(4), q as u16);
        return 1;
    }
    if !less(time, end) {
        return 1;
    }
    let dy = lh(m, cur.wrapping_add(2)).wrapping_sub(lh(m, record.wrapping_add(2)));
    let n = mul(dy, time.wrapping_sub(middle));
    let q = quotient(m, n, end.wrapping_sub(middle), 0x800d_f214);
    let y = lhu(m, record.wrapping_add(2)).wrapping_add(q);
    m.sh(cur.wrapping_add(2), y as u16);
    let n = mul(lh(m, cur.wrapping_add(6)), time.wrapping_sub(middle));
    let q = quotient(m, n, end.wrapping_sub(middle), 0x800d_f280);
    m.sh(cur.wrapping_add(6), q as u16);
    1
}

/// 800DF2B0: 1 when the record's slot has finished (mode 2).
pub fn step_finished(m: &mut Machine) -> u32 {
    let block = work(m);
    let record = m.lw(block.wrapping_add(0x3c));
    u32::from(record_mode(m, block, record) == 2)
}

/// 800DF2E8: from start to middle, grow the rectangle horizontally from
/// the record's origin x.
pub fn open_horizontal(m: &mut Machine) -> u32 {
    let block = work(m);
    let (record, time, start, middle, _) = timing(m, block);
    let mode = record_mode(m, block, record);
    let cur = current(m);
    if mode == 0 {
        return 0;
    }
    if mode == 2 {
        return 1;
    }
    if less(time, start) {
        return 0;
    }
    if !less(time, middle) {
        return 1;
    }
    let dx = lh(m, cur).wrapping_sub(lh(m, record));
    let n = mul(dx, time.wrapping_sub(start));
    let q = quotient(m, n, middle.wrapping_sub(start), 0x800d_f3c8);
    let x = lhu(m, record).wrapping_add(q);
    m.sh(cur, x as u16);
    let n = mul(lh(m, cur.wrapping_add(4)), time.wrapping_sub(start));
    let q = quotient(m, n, middle.wrapping_sub(start), 0x800d_f42c);
    m.sh(cur.wrapping_add(4), q as u16);
    1
}

/// 800DF45C: from start to middle, offset the rectangle by the record's
/// origin scaled by the remaining time (middle - time) / (middle - start).
pub fn slide_linear(m: &mut Machine) -> u32 {
    let block = work(m);
    let (record, time, start, middle, _) = timing(m, block);
    let mode = record_mode(m, block, record);
    let cur = current(m);
    if mode == 0 {
        return 0;
    }
    if mode == 2 {
        return 1;
    }
    if less(time, start) {
        return 0;
    }
    if !less(time, middle) {
        return 1;
    }
    let left = middle.wrapping_sub(time);
    let n = mul(lh(m, record), left);
    let q = quotient(m, n, middle.wrapping_sub(start), 0x800d_f538);
    let x = lhu(m, cur).wrapping_add(q);
    m.sh(cur, x as u16);
    let record = m.lw(block.wrapping_add(0x3c));
    let n = mul(lh(m, record.wrapping_add(2)), middle.wrapping_sub(time));
    let q = quotient(m, n, middle.wrapping_sub(start), 0x800d_f5a0);
    let y = lhu(m, cur.wrapping_add(2)).wrapping_add(q);
    m.sh(cur.wrapping_add(2), y as u16);
    1
}

/// 800DF5D8: as [`slide_linear`] with the squares of the remaining time
/// and of the span.
pub fn slide_quadratic(m: &mut Machine) -> u32 {
    let block = work(m);
    let (record, time, start, middle, _) = timing(m, block);
    let mode = record_mode(m, block, record);
    let cur = current(m);
    if mode == 0 {
        return 0;
    }
    if mode == 2 {
        return 1;
    }
    if less(time, start) {
        return 0;
    }
    if !less(time, middle) {
        return 1;
    }
    let left = middle.wrapping_sub(time);
    let left = mul(left, left);
    let span = middle.wrapping_sub(start);
    let span = mul(span, span);
    let n = mul(lh(m, record), left);
    let q = quotient(m, n, span, 0x800d_f6cc);
    let x = lhu(m, cur).wrapping_add(q);
    m.sh(cur, x as u16);
    let record = m.lw(block.wrapping_add(0x3c));
    let n = mul(lh(m, record.wrapping_add(2)), left);
    let q = quotient(m, n, span, 0x800d_f728);
    let y = lhu(m, cur.wrapping_add(2)).wrapping_add(q);
    m.sh(cur.wrapping_add(2), y as u16);
    1
}

/// A step routine by address: the two without a map start natively, the
/// rest through `dispatch::call` with the caller's registers.
pub fn call_step(m: &mut Machine, routine: u32, args: [u32; 4]) -> u32 {
    match routine {
        STEP_ALWAYS => 1,
        STEP_OPEN_WINDOW => open_window(m),
        _ => dispatch::call(m, routine, args),
    }
}

/// The current record's step routine (index record +0xA of [`STEPS`]).
fn step(m: &mut Machine, block: u32, args: [u32; 4]) -> u32 {
    let record = m.lw(block.wrapping_add(0x3c));
    let index = lh(m, record.wrapping_add(0xa));
    let routine = m.lw((index << 2).wrapping_add(STEPS));
    call_step(m, routine, args)
}

// ---- command handlers ----------------------------------------------------------------------------

/// The command's flag test: hidden when flag index `+at` is nonzero and
/// its flag byte differs from the value at `+at + 2`.
fn hidden(m: &mut Machine, command: u32, at: u32) -> bool {
    let index = lh(m, command.wrapping_add(at));
    if index == 0 {
        return false;
    }
    let block = work(m);
    let flags = m.lw(block.wrapping_add(0x5c));
    let value = lh(m, command.wrapping_add(at + 2));
    let flag = lbu(m, flags.wrapping_add(index));
    value != flag
}

/// A halfword pair as the original packs it (`lhu` low | `lh` high << 16).
fn pair(m: &mut Machine, address: u32) -> u32 {
    let low = lhu(m, address);
    let high = lh(m, address.wrapping_add(2));
    low | high << 16
}

/// Link `prim` into ordering-table slot record +0xE with length `words`
/// (the tag keeps the slot's upper byte).
fn link(m: &mut Machine, block: u32, ot: u32, prim: u32, words: u32) {
    let record = m.lw(block.wrapping_add(0x3c));
    let slot = (lh(m, record.wrapping_add(0xe)) << 2).wrapping_add(ot);
    let head = m.lw(slot);
    m.sw(prim, head | words << 24);
    let record = m.lw(block.wrapping_add(0x3c));
    let slot = (lh(m, record.wrapping_add(0xe)) << 2).wrapping_add(ot);
    m.sw(slot, prim & 0x00ff_ffff);
}

/// `addPrim` with both upper bytes kept: the tag's length and the slot's.
fn add_prim(m: &mut Machine, block: u32, ot: u32, prim: u32) {
    let record = m.lw(block.wrapping_add(0x3c));
    let slot = (lh(m, record.wrapping_add(0xe)) << 2).wrapping_add(ot);
    let tag = m.lw(prim);
    let head = m.lw(slot);
    m.sw(prim, (tag & 0xff00_0000) | (head & 0x00ff_ffff));
    let record = m.lw(block.wrapping_add(0x3c));
    let slot = (lh(m, record.wrapping_add(0xe)) << 2).wrapping_add(ot);
    let head = m.lw(slot);
    m.sw(slot, (head & 0xff00_0000) | (prim & 0x00ff_ffff));
}

/// Allocate `bytes` of primitive at the work block's packet pointer and
/// write the command's rectangle (x, y at +0; w, h at `size_at`) to the
/// current rectangle. Returns (block, primitive, current rectangle).
fn begin(m: &mut Machine, command: u32, bytes: u32, size_at: u32) -> (u32, u32, u32) {
    let block = work(m);
    let prim = m.lw(block.wrapping_add(4));
    let cur = current(m);
    m.sw(block.wrapping_add(4), prim.wrapping_add(bytes));
    let origin = pair(m, command);
    m.sw(cur, origin);
    let size = pair(m, command.wrapping_add(size_at));
    m.sw(cur.wrapping_add(4), size);
    (block, prim, cur)
}

/// The command's RGB bytes at `at..at + 3` as a primitive color word.
fn rgb(m: &mut Machine, command: u32, at: u32) -> u32 {
    let r = lbu(m, command.wrapping_add(at));
    let g = lbu(m, command.wrapping_add(at + 1));
    let b = lbu(m, command.wrapping_add(at + 2));
    r | g << 8 | b << 16
}

/// The stepped rectangle's origin plus the cell offset, as an xy word.
fn origin(m: &mut Machine, cur: u32, cell: u32) -> u32 {
    let dx = lh(m, cell.wrapping_add(4));
    let x = lh(m, cur).wrapping_add(dx);
    let dy = lh(m, cell.wrapping_add(6));
    let y = lh(m, cur.wrapping_add(2)).wrapping_add(dy);
    (x & 0xffff) | y << 16
}

/// 800DF760 (handler 0): a flat rectangle (TILE, 0x10 bytes): body +0..+6
/// rectangle, +8..+0xA RGB, +0xB semi-transparency, +0xC/+0xE flag test.
/// Returns the next command (body + 0x14).
pub fn draw_tile(m: &mut Machine, command: u32, ot: u32, cell: u32, a3: u32) -> u32 {
    super::ov2_p2::framed(m, 0x30, |m, _| {
        let next = command.wrapping_add(0x14);
        if hidden(m, command, 0xc) {
            return next;
        }
        let (block, prim, cur) = begin(m, command, 0x10, 4);
        if step(m, block, [block, cur, cell, a3]) == 0 {
            return next;
        }
        let cur = current(m);
        let xy = origin(m, cur, cell);
        m.sw(prim.wrapping_add(8), xy);
        let size = pair(m, cur.wrapping_add(4));
        m.sw(prim.wrapping_add(0xc), size);
        let color = rgb(m, command, 8);
        m.sb(prim.wrapping_add(3), 3);
        m.sw(prim.wrapping_add(4), color);
        m.sb(prim.wrapping_add(7), 0x60);
        let code = if lbu(m, command.wrapping_add(0xb)) != 0 {
            0x62
        } else {
            0x60
        };
        m.sb(prim.wrapping_add(7), code);
        let block = work(m);
        link(m, block, ot, prim, 3);
        next
    })
}

/// A texture coordinate as the original computes it: the value's signed
/// remainder modulo 2^`bits`, sign-extended from 16 bits.
fn remainder(value: u32, bits: u32) -> u32 {
    let v = value as i32;
    let biased = if v < 0 {
        v.wrapping_add((1 << bits) - 1)
    } else {
        v
    };
    sext16(v.wrapping_sub((biased >> bits) << bits) as u32)
}

/// The CLUT halfword of texture position (`x` at +8, `y` at +0xA) as the
/// original packs it, shifted to the upper half.
fn clut(m: &mut Machine, command: u32) -> u32 {
    let y = lh(m, command.wrapping_add(0xa));
    let x = lhu(m, command.wrapping_add(8));
    (y << 6 | (x >> 4) & 0x3f) << 16
}

/// 800DF940 (handler 1): a textured rectangle (SPRT, 0x14 bytes): body
/// +0/+2 position, +4..+6 RGB, +7 semi-transparency, +8/+0xA CLUT
/// position, +0xC/+0xE texture u/v, +0x10/+0x12 size (u width x4),
/// +0x14/+0x16 flag test. Returns body + 0x1C.
pub fn draw_sprite(m: &mut Machine, command: u32, ot: u32, cell: u32, a3: u32) -> u32 {
    super::ov2_p2::framed(m, 0x30, |m, _| {
        let next = command.wrapping_add(0x1c);
        if hidden(m, command, 0x14) {
            return next;
        }
        let (block, prim, cur) = begin(m, command, 0x14, 0x10);
        if step(m, block, [block, cur, cell, a3]) == 0 {
            return next;
        }
        let cur = current(m);
        let xy = origin(m, cur, cell);
        m.sw(prim.wrapping_add(8), xy);
        let w = lh(m, command.wrapping_add(0x10));
        let h = lh(m, command.wrapping_add(0x12));
        m.sw(prim.wrapping_add(0x10), ((w << 2) & 0xffff) | h << 16);
        let u = remainder(lh(m, command.wrapping_add(0xc)), 6) << 2;
        let v = remainder(lh(m, command.wrapping_add(0xe)), 8) << 8;
        let clut = clut(m, command);
        m.sw(prim.wrapping_add(0xc), u | v | clut);
        let color = rgb(m, command, 4);
        m.sb(prim.wrapping_add(3), 4);
        m.sw(prim.wrapping_add(4), color);
        m.sb(prim.wrapping_add(7), 0x64);
        let code = if lbu(m, command.wrapping_add(7)) != 0 {
            0x66
        } else {
            0x64
        };
        m.sb(prim.wrapping_add(7), code);
        let block = work(m);
        link(m, block, ot, prim, 4);
        next
    })
}

/// The texture page halfword of texture position (`x`, `y` at
/// `position`) and semi-transparency `abr`, as the original packs it.
fn tpage(position: u32, y: u32, abr: u32) -> u32 {
    let page = (abr & 3) << 5 | sext16((y & 0x100) << 16 >> 20) | (position & 0x3ff) >> 6;
    page | sext16(y & 0x200) << 2
}

/// 800DFBA8 (handler 2): a draw-mode primitive (DR_MODE, 0xC bytes) for
/// texture page position +0/+2 and semi-transparency +7 (SetDrawMode with
/// dithering and drawing to the display off, no texture window). Returns
/// body + 0xC.
pub fn draw_mode(m: &mut Machine, command: u32, ot: u32) -> u32 {
    super::ov2_p2::framed(m, 0x30, |m, sp| {
        let block = work(m);
        let prim = m.lw(block.wrapping_add(4));
        m.sw(block.wrapping_add(4), prim.wrapping_add(0xc));
        let abr = lbu(m, command.wrapping_add(7));
        let y = lhu(m, command.wrapping_add(2));
        let x = lhu(m, command);
        m.sw(sp + 0x10, 0);
        let page = tpage(x, y, abr);
        sdk::set_draw_mode(m, prim, 0, 0, page, 0);
        let block = work(m);
        add_prim(m, block, ot, prim);
        command.wrapping_add(0xc)
    })
}

/// 800DFCC8 (handler 3): a textured quad (POLY_FT4, 0x28 bytes): body
/// +0/+2 position, +4..+6 RGB, +7 semi-transparency (0xFF: opaque),
/// +8/+0xA CLUT position, +0xC/+0xE texture u/v, +0x10/+0x12 texture
/// size, +0x14/+0x16 rectangle size (negative: mirrored), +0x18/+0x1A flag
/// test, +0x1E texture depth (0: 4-bit, u scaled by 4; 1: 8-bit, by 2).
/// Returns body + 0x20.
///
/// For another depth the original scales u by the caller's `s2`, a
/// register it never sets: the display-list interpreter's list index. The
/// game's lists use depths 0 and 1 only (every list of the hub RAM dumps);
/// the port scales by 0, the oracle's register value.
pub fn draw_quad(m: &mut Machine, command: u32, ot: u32, cell: u32, a3: u32) -> u32 {
    super::ov2_p2::framed(m, 0x38, |m, _| {
        let next = command.wrapping_add(0x20);
        if hidden(m, command, 0x18) {
            return next;
        }
        let block = work(m);
        let prim = m.lw(block.wrapping_add(4));
        m.sw(block.wrapping_add(4), prim.wrapping_add(0x28));
        let abr = lbu(m, command.wrapping_add(7));
        let translucent = abr != 0xff;
        let abr = if translucent { abr } else { 0 };
        let cur = current(m);
        let xy = pair(m, command);
        m.sw(cur, xy);
        let size = pair(m, command.wrapping_add(0x14));
        m.sw(cur.wrapping_add(4), size);
        if step(m, block, [cur, block, cell, a3]) == 0 {
            return next;
        }
        let scale = match lbu(m, command.wrapping_add(0x1e)) {
            0 => 4,
            1 => 2,
            _ => 0,
        };
        let u = remainder(lh(m, command.wrapping_add(0xc)), 6);
        let u0 = if less(lh(m, command.wrapping_add(0x14)), 0) {
            mul(u, scale).wrapping_sub(1)
        } else {
            mul(u, scale)
        };
        let v = lh(m, command.wrapping_add(0xe));
        let v0 = if less(lh(m, command.wrapping_add(0x16)), 0) {
            remainder(v, 8).wrapping_sub(1)
        } else {
            remainder(v, 8)
        };
        let width = mul(lh(m, command.wrapping_add(0x10)), scale);
        let mut du = if lh(m, command.wrapping_add(0x14)) == width {
            width
        } else {
            width.wrapping_sub(1)
        };
        let height = lhu(m, command.wrapping_add(0x12));
        let mut dv = if lh(m, command.wrapping_add(0x16)) == lh(m, command.wrapping_add(0x12)) {
            height
        } else {
            height.wrapping_sub(1)
        };
        if sext16(u0).wrapping_add(sext16(du)) == 0x100 {
            du = du.wrapping_sub(1);
        }
        if sext16(v0).wrapping_add(sext16(dv)) == 0x100 {
            dv = dv.wrapping_sub(1);
        }
        let color = rgb(m, command, 4);
        m.sb(prim.wrapping_add(3), 9);
        m.sw(prim.wrapping_add(4), color);
        m.sb(prim.wrapping_add(7), 0x2c);
        m.sb(prim.wrapping_add(7), if translucent { 0x2e } else { 0x2c });
        let u0 = sext16(u0);
        let v0 = sext16(v0);
        let top = v0 << 8;
        let clut = clut(m, command);
        let bottom = v0.wrapping_add(sext16(dv)) << 8;
        m.sw(prim.wrapping_add(0xc), u0 | top | clut);
        let depth = lbu(m, command.wrapping_add(0x1e));
        let y = lhu(m, command.wrapping_add(0xe));
        let x = lhu(m, command.wrapping_add(0xc));
        m.sw(prim.wrapping_add(0x1c), u0 | bottom);
        let u1 = u0.wrapping_add(sext16(du));
        m.sw(prim.wrapping_add(0x24), u1 | bottom);
        let page = (depth & 3) << 7 | tpage(x, y, abr);
        m.sw(prim.wrapping_add(0x14), u1 | top | page << 16);
        let cur = current(m);
        let corners = [
            (false, false, 8),
            (true, false, 0x10),
            (false, true, 0x18),
            (true, true, 0x20),
        ];
        for (right, down, at) in corners {
            let mut y = lh(m, cur.wrapping_add(2)).wrapping_add(lh(m, cell.wrapping_add(6)));
            if down {
                y = y.wrapping_add(lh(m, cur.wrapping_add(6)));
            }
            let mut x = lh(m, cur).wrapping_add(lh(m, cell.wrapping_add(4)));
            if right {
                x = x.wrapping_add(lh(m, cur.wrapping_add(4)));
            }
            m.sw(prim.wrapping_add(at), y << 16 | (x & 0xffff));
        }
        let block = work(m);
        link(m, block, ot, prim, 9);
        next
    })
}

/// 800E019C (handler 4): skip a command (body + 0x10).
pub fn skip_command(command: u32) -> u32 {
    command.wrapping_add(0x10)
}

/// 800E01AC (handler 5): make this command the current animation record
/// (work block +0x3C). Returns body + 0x10.
pub fn set_record(m: &mut Machine, command: u32) -> u32 {
    let block = work(m);
    m.sw(block.wrapping_add(0x3c), command);
    command.wrapping_add(0x10)
}

/// 800E01C4 (handler 6): a line (LINE_F2, 0x10 bytes) from the stepped
/// rectangle's x, y to its w, h (both plus the cell offset): body as
/// [`draw_tile`]. Returns body + 0x14.
pub fn draw_line(m: &mut Machine, command: u32, ot: u32, cell: u32, a3: u32) -> u32 {
    super::ov2_p2::framed(m, 0x30, |m, _| {
        let next = command.wrapping_add(0x14);
        if hidden(m, command, 0xc) {
            return next;
        }
        let (block, prim, cur) = begin(m, command, 0x10, 4);
        if step(m, block, [block, cur, cell, a3]) == 0 {
            return next;
        }
        let cur = current(m);
        for (at, from, offset) in [(8, 0, 4), (0xa, 2, 6), (0xc, 4, 4), (0xe, 6, 6)] {
            let value = lhu(m, cur.wrapping_add(from));
            let delta = lhu(m, cell.wrapping_add(offset));
            m.sh(prim.wrapping_add(at), value.wrapping_add(delta) as u16);
        }
        let color = rgb(m, command, 8);
        m.sb(prim.wrapping_add(3), 3);
        m.sw(prim.wrapping_add(4), color);
        m.sb(prim.wrapping_add(7), 0x40);
        let code = if lbu(m, command.wrapping_add(0xb)) != 0 {
            0x42
        } else {
            0x40
        };
        m.sb(prim.wrapping_add(7), code);
        let block = work(m);
        add_prim(m, block, ot, prim);
        next
    })
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_efec => 1,
        0x800d_eff4 => open_window(m),
        0x800d_f2b0 => step_finished(m),
        0x800d_f2e8 => open_horizontal(m),
        0x800d_f45c => slide_linear(m),
        0x800d_f5d8 => slide_quadratic(m),
        0x800d_f760 => draw_tile(m, a0, a1, a2, a3),
        0x800d_f940 => draw_sprite(m, a0, a1, a2, a3),
        0x800d_fba8 => draw_mode(m, a0, a1),
        0x800d_fcc8 => draw_quad(m, a0, a1, a2, a3),
        0x800e_019c => skip_command(a0),
        0x800e_01ac => set_record(m, a0),
        0x800e_01c4 => draw_line(m, a0, a1, a2, a3),
        _ => return None,
    })
}
