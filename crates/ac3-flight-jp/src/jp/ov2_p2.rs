//! JP overlay 2, part 2 (routines 800D7DB0..800E14D0), translated from the
//! JP instructions: the hub work block (`[0x80112DC0]`, 0x5718 bytes from
//! the arena) and the accessors of the visual block (`[0x80112DC4]`).
//!
//! Work block layout as these routines use it:
//! * +0x14/+0x18: two 0x40-byte buffers; +0x1C a word cleared at init;
//! * +0x20..+0x28 / +0x2C..+0x34: three pairs of 0xD8-byte (18 records of
//!   12 bytes) and 0x48-byte buffers;
//! * +0x3C: pointer to the embedded record at +0x40;
//! * +0x50/+0x54/+0x58: 16 halfword fade slots each: level, target, mode
//!   (1 rising, 2 risen, 3 falling, 0 off), stepped by [`step_fades`];
//! * +0x5C: 64 flag bytes; +0x60 a word;
//! * +0x70/+0xF0: two 32-word tables; +0x170/+0x172 halfwords;
//! * +0x178 + 0x2AA8 * n (n 0, 1): 0x111 textured quads (0x28 bytes);
//! * +0x56C8 + 0x28 * n: one more quad per set.
//!
//! The visual block holds at +0x16C + 4 * a pointers to 12-byte cell
//! records (+4/+6/+8 halfwords, +0xA a mode halfword), at +0x160/+0x178 word
//! tables, and at +0x184..+0x1A4 a layout table.
//!
//! The other routines of the part live in the `ov2_p2_*` files; every file
//! claims its routines in its own [`dispatch`], which answers only while
//! module 2 is loaded.
use crate::arena;
use crate::common::loaded_module;
use crate::machine::{Fault, Machine};

/// The overlay module of these routines.
pub const MODULE: u32 = 2;
/// Pointer to the hub work block.
pub const WORK: u32 = 0x8011_2dc0;
/// Pointer to the visual block.
pub const VISUAL: u32 = 0x8011_2dc4;

pub(crate) fn work(m: &mut Machine) -> u32 {
    m.lw(WORK)
}
pub(crate) fn visual(m: &mut Machine) -> u32 {
    m.lw(VISUAL)
}
/// Sign extension of a halfword to a register.
pub(crate) fn sext16(value: u32) -> u32 {
    value as u16 as i16 as i32 as u32
}
/// `lh`: a sign-extended halfword.
pub(crate) fn lh(m: &mut Machine, address: u32) -> u32 {
    m.lh(address) as i32 as u32
}
/// `lhu`: a zero-extended halfword.
pub(crate) fn lhu(m: &mut Machine, address: u32) -> u32 {
    m.lhu(address).into()
}
/// `lbu`: a zero-extended byte.
pub(crate) fn lbu(m: &mut Machine, address: u32) -> u32 {
    m.lbu(address).into()
}
/// `slt`: signed less-than.
pub(crate) fn less(a: u32, b: u32) -> bool {
    (a as i32) < (b as i32)
}
/// `mult` / `mflo`: the low word of the signed product.
pub(crate) fn mul(a: u32, b: u32) -> u32 {
    (a as i32).wrapping_mul(b as i32) as u32
}
/// Signed `div` as compiled: `break 7` at `zero_break` for a zero divisor,
/// `break 6` 0x18 bytes later for 0x80000000 / -1 (recorded as
/// `Fault::Trap`), and the quotient the CPU leaves in LO.
pub(crate) fn quotient(m: &mut Machine, n: u32, d: u32, zero_break: u32) -> u32 {
    let (n, d) = (n as i32, d as i32);
    if d == 0 {
        m.raise(Fault::Trap {
            address: zero_break,
            code: 7,
        });
        return if n < 0 { 1 } else { u32::MAX };
    }
    if d == -1 && n == i32::MIN {
        m.raise(Fault::Trap {
            address: zero_break + 0x18,
            code: 6,
        });
        return n as u32;
    }
    n.wrapping_div(d) as u32
}
/// Run `body` inside the original stack frame of `size` bytes.
pub(crate) fn framed<R>(
    m: &mut Machine,
    size: u32,
    body: impl FnOnce(&mut Machine, u32) -> R,
) -> R {
    let sp = m.push_frame(size);
    let result = body(m, sp);
    m.pop_frame(size);
    result
}
/// The fifth and sixth arguments of a dispatched call (`sp+0x10/+0x14`).
pub(crate) fn stack_args<const N: usize>(m: &mut Machine) -> [u32; N] {
    let sp = m.sp;
    std::array::from_fn(|i| m.lw(sp.wrapping_add(0x10 + 4 * i as u32)))
}

// ---- 800DE79C / 800DE924: work block initialisation --------------------------------------------

/// 800DE79C: clear the two 32-word tables, the halfwords +0x170/+0x172
/// and flag byte 2, and set up the two quad sets (0x111 quads at +0x178 +
/// 0x2AA8 * n: code 0x2E, texture 0xF0..0xF8 x 0x7E..0x8A, CLUT 0x2D8,
/// page 0x47; one quad at +0x56C8 + 0x28 * n: code 0x2C, texture
/// 0xD4..0xE0 x 0x1B..0x2F, CLUT 0x358, page 7). Returns 0 (the last loop test, the original's `v0`).
pub fn init_quads(m: &mut Machine) -> u32 {
    let mut table = work(m);
    for _ in 0..0x20 {
        m.sw(table.wrapping_add(0xf0), 0);
        m.sw(table.wrapping_add(0x70), 0);
        table = table.wrapping_add(4);
    }
    let block = work(m);
    let flags = m.lw(block.wrapping_add(0x5c));
    m.sh(block.wrapping_add(0x170), 0);
    m.sh(block.wrapping_add(0x172), 0);
    m.sb(flags.wrapping_add(2), 0);
    for set in 0..2u32 {
        let block = work(m);
        let mut quad = block.wrapping_add(0x178 + 0x2aa8 * set);
        for _ in 0..0x111 {
            for (offset, value) in [
                (3, 9),
                (7, 0x2e),
                (4, 0x80),
                (5, 0x80),
                (6, 0x80),
                (0xc, 0xf0),
                (0xd, 0x7e),
                (0x14, 0xf8),
                (0x15, 0x7e),
                (0x1c, 0xf0),
                (0x1d, 0x8a),
                (0x24, 0xf8),
                (0x25, 0x8a),
            ] {
                m.sb(quad.wrapping_add(offset), value);
            }
            m.sh(quad.wrapping_add(0x16), 0x47);
            m.sh(quad.wrapping_add(0xe), 0x2d8);
            quad = quad.wrapping_add(0x28);
        }
        let block = work(m);
        let quad = block.wrapping_add(0x56c8 + 0x28 * set);
        for (offset, value) in [
            (7, 0x2c),
            (0xd, 0x1b),
            (0x15, 0x1b),
            (0x1d, 0x2f),
            (0x25, 0x2f),
        ] {
            m.sb(quad.wrapping_add(offset), value);
        }
        m.sh(quad.wrapping_add(0x16), 7);
        m.sh(quad.wrapping_add(0xe), 0x358);
        for (offset, value) in [
            (3, 9),
            (4, 0x80),
            (5, 0x80),
            (6, 0x80),
            (0xc, 0xd4),
            (0x14, 0xe0),
            (0x1c, 0xd4),
            (0x24, 0xe0),
        ] {
            m.sb(quad.wrapping_add(offset), value);
        }
    }
    0
}

/// 800DE924: allocate the work block and its buffers from the arena,
/// clear them, set the visual layout table (+0x184..+0x1A2), run
/// [`init_quads`] and clear word +0x1C. Returns the work block.
pub fn init_work(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = arena::alloc(m, 0x5718);
        m.sw(WORK, block);
        for pair in 0..3u32 {
            let records = arena::alloc(m, 0xd8);
            let slot = work(m).wrapping_add(4 * pair);
            m.sw(slot.wrapping_add(0x20), records);
            let other = arena::alloc(m, 0x48);
            let slot = work(m).wrapping_add(4 * pair);
            m.sw(slot.wrapping_add(0x2c), other);
            let mut offset = 0u32;
            for _ in 0..0x12 {
                let records = m.lw(slot.wrapping_add(0x20));
                m.sh(offset.wrapping_add(records), 0);
                let records = m.lw(slot.wrapping_add(0x20));
                m.sh(offset.wrapping_add(records).wrapping_add(2), 0);
                offset += 0xc;
            }
        }
        for n in 0..2u32 {
            let buffer = arena::alloc(m, 0x40);
            let slot = work(m).wrapping_add(4 * n);
            m.sw(slot.wrapping_add(0x14), buffer);
        }
        let layout = visual(m);
        let block = work(m);
        m.sw(block.wrapping_add(0x60), 0);
        for (offset, value) in [
            (0x184, 0x30),
            (0x188, 0x56),
            (0x18c, 0x7c),
            (0x186, 0x83),
            (0x18a, 0x58),
            (0x18e, 0x83),
            (0x190, 0xa2),
            (0x192, 0x58),
        ] {
            m.sh(layout.wrapping_add(offset), value);
        }
        let mut at = layout;
        for _ in 0..4 {
            m.sh(at.wrapping_add(0x194), 0xe00);
            m.sh(at.wrapping_add(0x19c), 0);
            at = at.wrapping_add(2);
        }
        let block = work(m);
        let record = block.wrapping_add(0x40);
        m.sw(block.wrapping_add(0x3c), record);
        m.sh(record.wrapping_add(0xe), 1);
        let record = m.lw(block.wrapping_add(0x3c));
        m.sh(record.wrapping_add(0xa), 0);
        // fade levels, modes, targets: each pointer is stored (block
        // re-read) before the next allocation, in the original's delay slots
        let levels = arena::alloc(m, 0x20);
        let block = work(m);
        m.sw(block.wrapping_add(0x50), levels);
        let modes = arena::alloc(m, 0x20);
        let block = work(m);
        m.sw(block.wrapping_add(0x58), modes);
        let targets = arena::alloc(m, 0x20);
        let block = work(m);
        m.sw(block.wrapping_add(0x54), targets);
        for slot in 0..0x10u32 {
            let offset = slot << 1;
            for field in [0x50, 0x58, 0x54] {
                let array = m.lw(block.wrapping_add(field));
                m.sh(offset.wrapping_add(array), 0);
            }
        }
        let flags = arena::alloc(m, 0x40);
        let block = work(m);
        m.sw(block.wrapping_add(0x5c), flags);
        for index in 0..0x40u32 {
            let block = work(m);
            let flags = m.lw(block.wrapping_add(0x5c));
            m.sb(flags.wrapping_add(index), 0);
        }
        init_quads(m);
        let block = work(m);
        m.sw(block.wrapping_add(0x1c), 0);
        block
    })
}

// ---- 800DE8F0..800DEDF0: visual block accessors --------------------------------------------------

/// 800DE8F0 (exact PAL 800F1BA0): the current ordering table, visual
/// block +8.
pub fn current_ot(m: &mut Machine) -> u32 {
    let block = visual(m);
    m.lw(block.wrapping_add(8))
}

/// 800DE908: visual block word +0x160 + 4 * (`index` & 0xFF) = `value`.
/// Returns the block plus 4 * index.
pub fn set_word_160(m: &mut Machine, value: u32, index: u32) -> u32 {
    let at = visual(m).wrapping_add((index & 0xff) << 2);
    m.sw(at.wrapping_add(0x160), value);
    at
}

/// The cell record `cell` of list `list` (visual block +0x16C + 4 * list,
/// 12-byte records), and the list's slot.
fn cell_slot(m: &mut Machine, list: u32) -> u32 {
    visual(m).wrapping_add(list << 2)
}

/// 800DEB5C: cell `cell` of list `list`: mode +0xA = `mode`, +4 = `x`,
/// +6 = `y`, +8 = `z` (halfwords; the pointer is re-read before each
/// store). Returns the list's record pointer.
pub fn set_cell(m: &mut Machine, list: u32, cell: u32, mode: u32, x: u32, y: u32, z: u32) -> u32 {
    let slot = cell_slot(m, list);
    let offset = cell.wrapping_mul(12);
    let mut records = 0;
    for (field, value) in [(0xa, mode), (4, x), (6, y), (8, z)] {
        records = m.lw(slot.wrapping_add(0x16c));
        m.sh(
            offset.wrapping_add(records).wrapping_add(field),
            value as u16,
        );
    }
    records
}

/// [`set_cell`] with its fifth and sixth arguments in the caller's frame,
/// as the original passes them.
fn set_cell_framed(
    m: &mut Machine,
    list: u32,
    cell: u32,
    mode: u32,
    x: u32,
    y: u32,
    z: u32,
) -> u32 {
    framed(m, 0x20, |m, sp| {
        m.sw(sp + 0x10, y);
        m.sw(sp + 0x14, z);
        set_cell(m, list, cell, mode, x, y, z)
    })
}

/// 800DEBB8: preset cell positions by `preset` (0..0x12, the original's
/// jump table at 0x800CC884): mode 0 and (x, y, z) from the table, else
/// zero; then visual block word +0x1A4 = 0. Returns the visual block.
pub fn set_cell_preset(m: &mut Machine, list: u32, preset: u32) -> u32 {
    let (x, y, z): (i32, i32, i32) = match preset {
        0 => (0, 0x37, 0xa),
        1 => (-0x149, 0, 0x19),
        5 => (0, -0x1f8, 0x1f),
        8 | 13 => (0x145, 0, -0x19),
        15 => (0, 0x200, -0x1f),
        _ => (0, 0, 0),
    };
    set_cell_framed(m, list, preset, 0, x as u32, y as u32, z as u32);
    let block = visual(m);
    m.sw(block.wrapping_add(0x1a4), 0);
    block
}

/// 800DECA8: cell mode 1 at the origin.
pub fn reset_cell(m: &mut Machine, list: u32, cell: u32) -> u32 {
    set_cell_framed(m, list, cell, 1, 0, 0, 0)
}

/// 800DECD4: cell mode 2 keeping x and y, z negated.
pub fn mirror_cell(m: &mut Machine, list: u32, cell: u32) -> u32 {
    framed(m, 0x20, |m, sp| {
        let slot = cell_slot(m, list);
        let records = m.lw(slot.wrapping_add(0x16c));
        let record = cell.wrapping_mul(12).wrapping_add(records);
        let y = sext16(m.lhu(record.wrapping_add(6)).into());
        let z = m.lhu(record.wrapping_add(8));
        let x = sext16(m.lhu(record.wrapping_add(4)).into());
        let z = sext16(0u32.wrapping_sub(z.into()));
        m.sw(sp + 0x10, y);
        m.sw(sp + 0x14, z);
        set_cell(m, list, cell, 2, x, y, z)
    })
}

/// 800DED38: the mode of cell `cell` of list `list` (sign-extended).
pub fn cell_mode(m: &mut Machine, list: u32, cell: u32) -> u32 {
    let slot = cell_slot(m, list);
    let records = m.lw(slot.wrapping_add(0x16c));
    let record = cell.wrapping_mul(12).wrapping_add(records);
    sext16(m.lhu(record.wrapping_add(0xa)).into())
}

/// 800DED68: cell mode 1 at (`x`, `y`, 0) (both sign-extended halfwords).
pub fn place_cell(m: &mut Machine, list: u32, cell: u32, x: u32, y: u32) -> u32 {
    set_cell_framed(m, list, cell, 1, sext16(x), sext16(y), 0)
}

/// 800DEDA0: copy the cell's x and y halfwords to `out`. Returns 1.
pub fn cell_position(m: &mut Machine, list: u32, cell: u32, out: u32) -> u32 {
    let slot = cell_slot(m, list);
    let offset = cell.wrapping_mul(12);
    let records = m.lw(slot.wrapping_add(0x16c));
    let x = m.lhu(offset.wrapping_add(records).wrapping_add(4));
    m.sh(out, x);
    let records = m.lw(slot.wrapping_add(0x16c));
    let y = m.lhu(offset.wrapping_add(records).wrapping_add(6));
    m.sh(out.wrapping_add(2), y);
    1
}

/// 800DEDF0: visual block word +0x178 + 4 * `index` = `value`. Returns
/// the block plus 4 * index.
pub fn set_word_178(m: &mut Machine, index: u32, value: u32) -> u32 {
    let at = visual(m).wrapping_add(index << 2);
    m.sw(at.wrapping_add(0x178), value);
    at
}

// ---- 800DEE08..800DEFB4: fades and flags -------------------------------------------------------

/// 800DEE08: fade slot `slot`: mode = `mode`; target = `target` unless
/// negative; level = `level` unless negative. Returns the last slot
/// address written.
pub fn set_fade(m: &mut Machine, slot: u32, mode: u32, level: u32, target: u32) -> u32 {
    let block = work(m);
    let offset = slot << 1;
    let mut at = offset.wrapping_add(m.lw(block.wrapping_add(0x58)));
    m.sh(at, mode as u16);
    if (target as i32) >= 0 {
        at = offset.wrapping_add(m.lw(block.wrapping_add(0x54)));
        m.sh(at, target as u16);
    }
    if (level as i32) >= 0 {
        let block = work(m);
        at = offset.wrapping_add(m.lw(block.wrapping_add(0x50)));
        m.sh(at, level as u16);
    }
    at
}

/// 800DEE60: flag byte `index`.
pub fn flag(m: &mut Machine, index: u32) -> u32 {
    let block = work(m);
    let flags = m.lw(block.wrapping_add(0x5c));
    m.lbu(flags.wrapping_add(index)).into()
}

/// 800DEE84: fade mode of slot `slot` (sign-extended).
pub fn fade_mode(m: &mut Machine, slot: u32) -> u32 {
    let block = work(m);
    let modes = m.lw(block.wrapping_add(0x58));
    sext16(m.lhu((slot << 1).wrapping_add(modes)).into())
}

/// 800DEEA8: fade level of slot `slot` (sign-extended).
pub fn fade_level(m: &mut Machine, slot: u32) -> u32 {
    let block = work(m);
    let levels = m.lw(block.wrapping_add(0x50));
    sext16(m.lhu((slot << 1).wrapping_add(levels)).into())
}

/// 800DEECC: step the 16 fade slots: a rising slot (mode 1) gains 1 until
/// it reaches its target, then becomes mode 2; a falling one (mode 3)
/// loses 1 while positive, then becomes mode 0. Returns 0 (the last loop
/// test, the original's `v0`).
pub fn step_fades(m: &mut Machine) -> u32 {
    let block = work(m);
    for slot in 0..0x10u32 {
        let offset = slot << 1;
        let mode = offset.wrapping_add(m.lw(block.wrapping_add(0x58)));
        if m.lh(mode) == 1 {
            let levels = m.lw(block.wrapping_add(0x50));
            let targets = m.lw(block.wrapping_add(0x54));
            let level = offset.wrapping_add(levels);
            let target = offset.wrapping_add(targets);
            let below = m.lh(level) < m.lh(target);
            let value = m.lhu(level);
            if below {
                m.sh(level, value.wrapping_add(1));
            } else {
                m.sh(mode, 2);
            }
        }
        let mode = offset.wrapping_add(m.lw(block.wrapping_add(0x58)));
        if m.lh(mode) == 3 {
            let levels = m.lw(block.wrapping_add(0x50));
            let level = offset.wrapping_add(levels);
            let positive = m.lh(level) > 0;
            let value = m.lhu(level);
            if positive {
                m.sh(level, value.wrapping_sub(1));
            } else {
                m.sh(mode, 0);
            }
        }
    }
    0
}

/// 800DEF94: flag byte `index` = `value`. Returns its address.
pub fn set_flag(m: &mut Machine, index: u32, value: u32) -> u32 {
    let block = work(m);
    let at = m.lw(block.wrapping_add(0x5c)).wrapping_add(index);
    m.sb(at, value as u8);
    at
}

/// 800DEFB4: clear the 64 flag bytes (the pointer re-read for each).
/// Returns 0 (the last loop test, the original's `v0`).
pub fn clear_flags(m: &mut Machine) -> u32 {
    for index in 0..0x40u32 {
        let block = work(m);
        let flags = m.lw(block.wrapping_add(0x5c));
        m.sb(flags.wrapping_add(index), 0);
    }
    0
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_e79c => init_quads(m),
        0x800d_e8f0 => current_ot(m),
        0x800d_e908 => set_word_160(m, a0, a2),
        0x800d_e924 => init_work(m),
        0x800d_eb5c => {
            let [y, z] = stack_args(m);
            set_cell(m, a0, a1, a2, a3, y, z)
        }
        0x800d_ebb8 => set_cell_preset(m, a0, a1),
        0x800d_eca8 => reset_cell(m, a0, a1),
        0x800d_ecd4 => mirror_cell(m, a0, a1),
        0x800d_ed38 => cell_mode(m, a0, a1),
        0x800d_ed68 => place_cell(m, a0, a1, a2, a3),
        0x800d_eda0 => cell_position(m, a0, a1, a2),
        0x800d_edf0 => set_word_178(m, a0, a1),
        0x800d_ee08 => set_fade(m, a0, a1, a2, a3),
        0x800d_ee60 => flag(m, a0),
        0x800d_ee84 => fade_mode(m, a0),
        0x800d_eea8 => fade_level(m, a0),
        0x800d_eecc => step_fades(m),
        0x800d_ef94 => set_flag(m, a0, a1),
        0x800d_efb4 => clear_flags(m),
        _ => return None,
    })
}
