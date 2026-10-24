//! JP overlay 2, part 5: the mission list with its disc prompt
//! (800FD770..800FE668), the mission/item unlock tables (800FE668..800FEEE0)
//! and the disc-check steps (800FEEE0..800FF100).
//!
//! The disc-prompt block `[0x80112E1C]` (0x44 bytes, [`select_initialize`])
//! holds +0 the state, +1 the result (1 chosen, 2 cancelled), +2 the request
//! (1 confirm, 2 cancel), +4 the column, +5 the row, +6 the list of the
//! missions with a rank (count +0x42), four per row. [`select_update`] runs
//! the state routine `[0x80111758 + 4 * state]`:
//!
//! | State | Routine | Rust |
//! | ---: | --- | --- |
//! | 0 | `800FD958` | [`list_open`] |
//! | 1 | `800FD9CC` | [`list_update`]: the cursor, confirm/cancel |
//! | 2 | `800FDE78` | [`list_close`] |
//! | 3 | `800FDF38` | [`difficulty_open`] |
//! | 4 | `800FDF64` | [`difficulty_update`]: context +0x394 0..2 |
//! | 5 | `800FE0D4` | [`difficulty_close`] |
//! | 6 | `800FE180` | [`disc_check_open`] |
//! | 7 | `800FE1E8` | [`disc_check_update`]: the steps at 0x80112790 |
//!
//! (the table goes on with 800FE444, 800FE564 and 800FE584).
//!
//! The disc-check block `[0x8011278C]` (0x80112E20) holds +0 the message
//! (0 right disc on both, 1 no disc, 2/4 and 3/5 the two "insert" prompts, 6
//! right disc), +4 a disc-swapped flag and +8 the chosen mission; the disc of
//! each mission is the byte table 0x80112750.
use super::ov2_p5::{
    MODULE, RECORD, alloc, call, ctx, framed, hub, lbu, lh, lhu, lw, sb, sh, slt, sra, sw,
};
use crate::common::loaded_module;
use crate::machine::Machine;

/// `[0x80112E1C]`: the disc-prompt block (see the module doc).
pub const DISC: u32 = 0x8011_2e1c;
/// `[0x80112DD4]`: the list's row-change delay (frames without the arrows).
const DELAY: u32 = 0x8011_2dd4;
/// `[0x8011278C]`: the disc-check block (see the module doc).
const CHECK: u32 = 0x8011_278c;
/// 0x80112790: the four disc-check step routines (800FEF08, 800FEF24,
/// 800FF098, 800FF0BC), indexed by `[HUB]+0x264`.
const CHECK_STEPS: u32 = 0x8011_2790;
/// 0x80112750: the disc of each mission (1, 2; 3 both).
const DISC_OF: u32 = 0x8011_2750;
/// 0x80111758: the state routines of the disc-prompt block.
const STATES: u32 = 0x8011_1758;
/// 0x80111784: the 60 mission records (12 bytes): +0 a word (the group),
/// +4 the item count, +5.. up to seven item numbers.
const MISSIONS: u32 = 0x8011_1784;
/// 0x80111A54: the 37 item records (12 bytes): +0 and +1 two counts, +2 and
/// +3 two defaults, +4..+7 and +8..+0xB their lists.
const ITEMS: u32 = 0x8011_1a54;
/// 0x80112228: 62 x 4 bytes, matched by [`mark_by_byte`].
const MARK_BYTES: u32 = 0x8011_2228;
/// 0x80112330: 62 x 8 halfwords, matched by [`mark_by_half`].
const MARK_HALVES: u32 = 0x8011_2330;

/// The 12-byte record `index` of `table` (`(index * 3) << 2`, wrapping).
fn rec12(table: u32, index: u32) -> u32 {
    ((index << 1).wrapping_add(index) << 2).wrapping_add(table)
}

/// `(count - 1) / 4` as compiled (`bgez`, else `count + 2`; `sra 2`): the
/// last row of the list.
fn last_row(count: u32) -> u32 {
    let last = count.wrapping_sub(1);
    let rounded = if (last as i32) < 0 {
        count.wrapping_add(2)
    } else {
        last
    };
    sra(rounded, 2)
}

/// 8002DB4C (resident): request sound effect `id`. Returns its `v0`.
fn sound(m: &mut Machine, id: u32) -> u32 {
    call(m, 0x8002_db4c, [id, 0, 0, 0])
}

/// Store the byte `value` at `[[RECORD]+0x5C] + offset`. Returns the record.
fn mark(m: &mut Machine, offset: u32, value: u32) -> u32 {
    let record = m.lw(RECORD);
    let current = lw(m, record, 0x5c);
    sb(m, current, offset, value);
    current
}

/// The picture of list entry `index`: `[[[HUB]+table] + 4 * [DISC]+6+index]`,
/// with the stack argument `mode` stored at `sp + 0x10` before the last load
/// (the original order).
fn entry_image(m: &mut Machine, sp: u32, index: u32, table: u32, mode: u32) -> u32 {
    let block = m.lw(DISC);
    let h = hub(m);
    let mission = lbu(m, block.wrapping_add(index), 6);
    let pictures = lw(m, h, table);
    sw(m, sp, 0x10, mode);
    lw(m, (mission << 2).wrapping_add(pictures), 0)
}

// ---- the mission list -------------------------------------------------------------

/// 800FD770: load the picture of list entry (row `a0`, column `a1`) from
/// `[HUB]+0x1D4` with 800ED000(picture, 0x3C0, 0x154, 0x180, stack 6).
/// Returns its `v0`.
pub fn entry_picture(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 0x20, |m, sp| {
        let index = a1.wrapping_add(a0 << 2);
        let picture = entry_image(m, sp, index, 0x1d4, 6);
        call(m, 0x800e_d000, [picture, 0x3c0, 0x154, 0x180])
    })
}

/// 800FD7D0: draw row `a0` of the list: clear the four marks
/// `[[RECORD]+0x5C]+0x35..`, then for each entry of the row (4, the rest on
/// the last row) load its picture at y = 0x154 + 0x12 n (800ED000, mode 6)
/// and set its mark to 2. Returns 0, or the shifted rest when it is <= 0.
pub fn draw_row(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x30, |m, sp| {
        let row = a0;
        for i in 0..4u32 {
            mark(m, 0x35 + i, 0);
        }
        let block = m.lw(DISC);
        let count = lh(m, block, 0x42);
        let rounded = if (count as i32) < 0 {
            count.wrapping_add(3)
        } else {
            count
        };
        let rows = sra(rounded, 2);
        let (entries, v0) = if slt(row, rows) {
            (4, 1)
        } else {
            let t = count.wrapping_sub(rows << 2) << 16;
            (sra(t, 16), t)
        };
        if (entries as i32) <= 0 {
            return v0;
        }
        let mut y = 0x154u32;
        let mut i = 0u32;
        loop {
            let index = i.wrapping_add(row << 2);
            let picture = entry_image(m, sp, index, 0x1d4, 6);
            call(m, 0x800e_d000, [picture, 0x3c0, y, 0x180]);
            mark(m, i.wrapping_add(0x35), 2);
            i = i.wrapping_add(1);
            y = y.wrapping_add(0x12);
            if !slt(i, entries) {
                break;
            }
        }
        0
    })
}

/// 800FD8F4: load the large picture of the selected entry (the current row,
/// column `a0`) from `[HUB]+0x1DC` with 800ED000(picture, 0x3C0, 0x100,
/// 0x300, stack 1). Returns its `v0`.
pub fn draw_selected(m: &mut Machine, a0: u32) -> u32 {
    framed(m, 0x20, |m, sp| {
        let block = m.lw(DISC);
        let row = lbu(m, block, 5);
        let index = a0.wrapping_add(row << 2);
        let h = hub(m);
        let mission = lbu(m, block.wrapping_add(index), 6);
        let pictures = lw(m, h, 0x1dc);
        sw(m, sp, 0x10, 1);
        let picture = lw(m, (mission << 2).wrapping_add(pictures), 0);
        call(m, 0x800e_d000, [picture, 0x3c0, 0x100, 0x300])
    })
}

/// 800FD958, state 0: go to state 1, draw the current row and the selected
/// entry, set the mark +0x2F. Returns the record `[[RECORD]+0x5C]`.
pub fn list_open(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let block = m.lw(DISC);
        sb(m, block, 0, 1);
        let block = m.lw(DISC);
        let row = lbu(m, block, 5);
        draw_row(m, row);
        let block = m.lw(DISC);
        let column = lbu(m, block, 4);
        draw_selected(m, column);
        mark(m, 0x2f, 1)
    })
}

/// 800FD9CC, state 1: the list cursor. Shows the row arrows
/// (800DEF94(0x31, bit 1 more rows below | bit 0 rows above)) once the
/// delay `[0x80112DD4]` ran out, moves with the pad edges at context +0x130
/// (bit 1 next entry, bit 0 previous entry, 0x200 next row, 0x100 previous
/// row; sound 0x3D, the row marks +0x33, delay 12), and on confirm
/// (+0x12C bit 4) or cancel (bit 5) stores the request and goes to state 2.
/// Returns the column (also stored in the record +0x34).
pub fn list_update(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let delay = m.lw(DELAY);
        if delay == 0 {
            let block = m.lw(DISC);
            let count = lh(m, block, 0x42);
            let row = lbu(m, block, 5);
            let mut arrows = if slt(row, last_row(count)) { 2 } else { 0 };
            if row != 0 {
                arrows |= 1;
            }
            call(m, 0x800d_ef94, [0x31, arrows, row, 0]);
            mark(m, 0x33, 0);
        } else {
            m.sw(DELAY, delay.wrapping_sub(1));
        }
        // bit 1: the next entry (after column 3 the first of the next row)
        let context = ctx(m);
        if lw(m, context, 0x130) & 2 != 0 {
            let block = m.lw(DISC);
            if lbu(m, block, 4) < 3 {
                let row = lbu(m, block, 5);
                let column = lbu(m, block, 4);
                let count = lh(m, block, 0x42);
                if slt(column.wrapping_add(row << 2), count.wrapping_sub(1)) {
                    sb(m, block, 4, column.wrapping_add(1));
                    sound(m, 0x3d);
                }
            } else {
                let count = lh(m, block, 0x42);
                let row = lbu(m, block, 5);
                if slt(row, last_row(count)) {
                    sb(m, block, 4, 0);
                    let block = m.lw(DISC);
                    let row = lbu(m, block, 5).wrapping_add(1);
                    sb(m, block, 5, row);
                    let block = m.lw(DISC);
                    let row = lbu(m, block, 5);
                    draw_row(m, row);
                    mark(m, 0x33, 2);
                    m.sw(DELAY, 0xc);
                    sound(m, 0x3d);
                }
            }
            let block = m.lw(DISC);
            let column = lbu(m, block, 4);
            draw_selected(m, column);
        }
        // bit 0: the previous entry (before column 0 the last of the row above)
        let context = ctx(m);
        if lw(m, context, 0x130) & 1 != 0 {
            let block = m.lw(DISC);
            let column = lbu(m, block, 4);
            if column != 0 {
                sb(m, block, 4, column.wrapping_sub(1));
                sound(m, 0x3d);
            } else if lbu(m, block, 5) != 0 {
                sb(m, block, 4, 3);
                let block = m.lw(DISC);
                let row = lbu(m, block, 5).wrapping_sub(1);
                sb(m, block, 5, row);
                let block = m.lw(DISC);
                let row = lbu(m, block, 5);
                draw_row(m, row);
                mark(m, 0x33, 1);
                m.sw(DELAY, 0xc);
                sound(m, 0x3d);
            }
            let block = m.lw(DISC);
            let column = lbu(m, block, 4);
            draw_selected(m, column);
        }
        // 0x200: the next row (the column clamped to the last entry)
        let context = ctx(m);
        if lw(m, context, 0x130) & 0x200 != 0 {
            let block = m.lw(DISC);
            let count = lh(m, block, 0x42);
            let row = lbu(m, block, 5);
            if slt(row, last_row(count)) {
                sb(m, block, 5, row.wrapping_add(1));
                let block = m.lw(DISC);
                let row = lbu(m, block, 5);
                let column = lbu(m, block, 4);
                let count = lh(m, block, 0x42);
                let last = count.wrapping_sub(1);
                if !slt(column.wrapping_add(row << 2), last) {
                    sb(m, block, 4, last.wrapping_sub(last_row(count) << 2));
                }
                sound(m, 0x3d);
                mark(m, 0x33, 2);
                m.sw(DELAY, 0xc);
            }
            let block = m.lw(DISC);
            let row = lbu(m, block, 5);
            draw_row(m, row);
            let block = m.lw(DISC);
            let column = lbu(m, block, 4);
            draw_selected(m, column);
        }
        // 0x100: the previous row
        let context = ctx(m);
        if lw(m, context, 0x130) & 0x100 != 0 {
            let block = m.lw(DISC);
            let row = lbu(m, block, 5);
            if row != 0 {
                sb(m, block, 5, row.wrapping_sub(1));
                sound(m, 0x3d);
                mark(m, 0x33, 1);
                m.sw(DELAY, 0xc);
            }
            let block = m.lw(DISC);
            let row = lbu(m, block, 5);
            draw_row(m, row);
            let block = m.lw(DISC);
            let column = lbu(m, block, 4);
            draw_selected(m, column);
        }
        let context = ctx(m);
        if lw(m, context, 0x12c) & 0x10 != 0 && lw(m, context, 0x128) & 0x2f == 0 {
            let block = m.lw(DISC);
            sh(m, block, 2, 1);
            sb(m, block, 0, 2);
            sound(m, 0x3c);
        }
        let context = ctx(m);
        if lw(m, context, 0x12c) & 0x20 != 0 && lw(m, context, 0x128) & 0x5f == 0 {
            let block = m.lw(DISC);
            sh(m, block, 2, 2);
            sb(m, block, 0, 2);
            sound(m, 0x3b);
        }
        let record = m.lw(RECORD);
        let block = m.lw(DISC);
        let current = lw(m, record, 0x5c);
        let column = lbu(m, block, 4);
        sb(m, current, 0x34, column);
        column
    })
}

/// 800FDE78, state 2: on confirm go to state 3 (800DEDF0(0, 0x08000003),
/// the selected entry's picture); on cancel set the result 2; clear the
/// record marks +0x31..+0x38 and +0x2F. Returns the record.
pub fn list_close(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = m.lw(DISC);
        if lh(m, block, 2) == 1 {
            sb(m, block, 0, 3);
            call(m, 0x800d_edf0, [0, 0x0800_0003, block, 0]);
            let block = m.lw(DISC);
            let row = lbu(m, block, 5);
            let column = lbu(m, block, 4);
            entry_picture(m, row, column);
        }
        let block = m.lw(DISC);
        let request = lh(m, block, 2);
        if request == 2 {
            sb(m, block, 1, request);
        }
        for offset in 0x31..0x39u32 {
            mark(m, offset, 0);
        }
        mark(m, 0x2f, 0)
    })
}

/// 800FDF38, state 3: go to state 4, set the record mark +0x2F. Returns 1.
pub fn difficulty_open(m: &mut Machine) -> u32 {
    let block = m.lw(DISC);
    sb(m, block, 0, 4);
    mark(m, 0x2f, 1);
    1
}

/// 800FDF64, state 4: the difficulty (context +0x394, 0..2) with the pad
/// edges at context +0x130 (bit 1 up, bit 0 down; sound 0x3D); confirm
/// (bit 4 without +0x128 bit 5) or cancel (bit 5 without +0x128 bit 4)
/// stores the request and goes to state 5. Returns the difficulty byte (also
/// stored in the record +0x34).
pub fn difficulty_update(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        mark(m, 0x33, 0);
        let context = ctx(m);
        if lw(m, context, 0x130) & 2 != 0 {
            let level = lw(m, context, 0x394);
            if slt(level, 2) {
                sw(m, context, 0x394, level.wrapping_add(1));
                sound(m, 0x3d);
            }
        }
        let context = ctx(m);
        if lw(m, context, 0x130) & 1 != 0 {
            let level = lw(m, context, 0x394);
            if (level as i32) > 0 {
                sw(m, context, 0x394, level.wrapping_sub(1));
                sound(m, 0x3d);
            }
        }
        let context = ctx(m);
        if lw(m, context, 0x130) & 0x10 != 0 && lw(m, context, 0x128) & 0x20 == 0 {
            let block = m.lw(DISC);
            sh(m, block, 2, 1);
            sb(m, block, 0, 5);
            sound(m, 0x3c);
        }
        let context = ctx(m);
        if lw(m, context, 0x130) & 0x20 != 0 && lw(m, context, 0x128) & 0x10 == 0 {
            let block = m.lw(DISC);
            sh(m, block, 2, 2);
            sb(m, block, 0, 5);
            sound(m, 0x3b);
        }
        let record = m.lw(RECORD);
        let context = ctx(m);
        let current = lw(m, record, 0x5c);
        let level = lbu(m, context, 0x394);
        sb(m, current, 0x34, level);
        level
    })
}

/// 800FE0D4, state 5: on confirm the next state (6); on cancel redraw the
/// list (800DEDF0(0, 0x04000003)) and go back to state 0; clear the record
/// mark +0x2F. Returns the record.
pub fn difficulty_close(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = m.lw(DISC);
        match lh(m, block, 2) {
            1 => {
                let state = lbu(m, block, 0).wrapping_add(1);
                sb(m, block, 0, state);
            }
            2 => {
                let row = lbu(m, block, 5);
                draw_row(m, row);
                let block = m.lw(DISC);
                let column = lbu(m, block, 4);
                draw_selected(m, column);
                call(m, 0x800d_edf0, [0, 0x0400_0003, 0, 0]);
                let block = m.lw(DISC);
                sb(m, block, 0, 0);
            }
            _ => {}
        }
        mark(m, 0x2f, 0)
    })
}

/// 800FE180, state 6: reset the disc-check step `[HUB]+0x264`, store the
/// selected mission in the disc-check block ([`check_set_mission`]) and go
/// to the next state. Returns the new state (`lbu + 1`).
pub fn disc_check_open(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let h = hub(m);
        let block = m.lw(DISC);
        sw(m, h, 0x264, 0);
        let row = lbu(m, block, 5);
        let column = lbu(m, block, 4);
        let index = column.wrapping_add(row << 2);
        let mission = lbu(m, block.wrapping_add(index), 6);
        check_set_mission(m, mission);
        let block = m.lw(DISC);
        let state = lbu(m, block, 0).wrapping_add(1);
        sb(m, block, 0, state);
        state
    })
}

/// 800FE1E8, state 7: run the disc-check step `[0x80112790 + 4 *
/// [HUB]+0x264]` (its bit 0: next step, bit 1: result 1 and return, bit 2:
/// back to step 1), then show the message [`check_message`] (800DEDF0(0,
/// 0x0A000003) and the record +0x3D); while an "insert" prompt shows,
/// cancel (+0x12C bit 5 alone) with a readable disc (8002B504 not 4 or -1)
/// sets the result 2 when 8002B46C allows it (sound 0x3B). Returns the
/// original's `v0` on every path.
pub fn disc_check_update(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let disc = call(m, 0x8002_b504, [0; 4]);
        let h = hub(m);
        let step = lw(m, h, 0x264);
        let routine = lw(m, CHECK_STEPS, step << 2);
        let flags = call(m, routine, [CHECK_STEPS, 0, 0, 0]);
        if flags & 1 != 0 {
            let h = hub(m);
            let step = lw(m, h, 0x264).wrapping_add(1);
            sw(m, h, 0x264, step);
        }
        if flags & 2 != 0 {
            let block = m.lw(DISC);
            sb(m, block, 1, 1);
            return 1;
        }
        if flags & 4 != 0 {
            let h = hub(m);
            sw(m, h, 0x264, 1);
        }
        let message = check_message(m);
        if message != 0 {
            call(m, 0x800d_edf0, [0, 0x0a00_0003, 0, 0]);
        }
        // Jump table 0x800CD6A4 (message - 1, 6 entries): 1 -> 800FE2D0
        // (+0x3D = 4), 2 -> 800FE2E8 (0), 3 -> 800FE300 (1), 4 -> 800FE318
        // (2), 5 -> 800FE390 (3), each of 2..5 then the cancel check, 6 ->
        // 800FE418 (5); the others leave `lui v0,0x800d` in v0.
        match message {
            1 => {
                mark(m, 0x3d, 4);
                4
            }
            6 => {
                mark(m, 0x3d, 5);
                5
            }
            2..=5 => {
                mark(m, 0x3d, message - 2);
                let context = ctx(m);
                if lw(m, context, 0x12c) & 0x20 == 0 {
                    return 0;
                }
                if lw(m, context, 0x128) & 0x5f != 0 {
                    return 4;
                }
                if disc == 4 || disc == u32::MAX {
                    return u32::MAX;
                }
                if call(m, 0x8002_b46c, [0; 4]) != 0 {
                    let block = m.lw(DISC);
                    sb(m, block, 1, 2);
                }
                sound(m, 0x3b)
            }
            _ => 0x800d_0000,
        }
    })
}

/// 800FE444: allocate the disc-prompt block (0x44 bytes) and clear it, list
/// the missions with a rank on any difficulty (context +0x95C +0x2C0,
/// +0x2FC, +0x338, +0x374), 800DEDF0(0, 0x04000003), then
/// [`next_substate`]. Returns the new `[HUB]+0x86`.
pub fn select_initialize(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = ctx(m);
        let results = context.wrapping_add(0x95c);
        let block = alloc(m, 0x44);
        m.sw(DISC, block);
        sb(m, block, 0, 0);
        let first = m.lw(DISC);
        sb(m, first, 1, 0);
        let second = m.lw(DISC);
        sb(m, second, 4, 0);
        let third = m.lw(DISC);
        sh(m, second, 2, 0);
        sb(m, third, 5, 0);
        let fourth = m.lw(DISC);
        sh(m, fourth, 0x42, 0);
        for mission in 0..0x3cu32 {
            let at = results.wrapping_add(mission);
            let ranked = [0x2c0, 0x2fc, 0x338, 0x374]
                .into_iter()
                .any(|o| lbu(m, at, o) != 0);
            if ranked {
                let block = m.lw(DISC);
                let count = lh(m, block, 0x42);
                sb(m, block.wrapping_add(count), 6, mission);
                let block = m.lw(DISC);
                let count = lhu(m, block, 0x42).wrapping_add(1);
                sh(m, block, 0x42, count);
            }
        }
        call(m, 0x800d_edf0, [0, 0x0400_0003, 0, 0]);
        next_substate(m)
    })
}

/// 800FE564 (JP; PAL 800D0C68, changed): count `[HUB]+0x86` up. Returns the
/// new value (`lhu + 1`).
pub fn next_substate(m: &mut Machine) -> u32 {
    let h = hub(m);
    let next = lhu(m, h, 0x86).wrapping_add(1);
    sh(m, h, 0x86, next);
    next
}

/// 800FE584: run the state routine `[0x80111758 + 4 * [DISC]+0]`; on the
/// result 1 set `[HUB]+0x84` = 2, +0x86 = 0 and the chosen mission (context
/// +0x38C), on the result 2 clear `[HUB]+0x7C`/`+0x7D`; both free
/// `[HUB]+0x28` (80027538). Returns that call's `v0`, else 2.
pub fn select_update(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let block = m.lw(DISC);
        let state = lbu(m, block, 0);
        let routine = lw(m, STATES, state << 2);
        call(m, routine, [0; 4]);
        let block = m.lw(DISC);
        match lbu(m, block, 1) {
            1 => {
                let h = hub(m);
                let context = ctx(m);
                sh(m, h, 0x84, 2);
                sh(m, h, 0x86, 0);
                let row = lbu(m, block, 5);
                let column = lbu(m, block, 4);
                let index = column.wrapping_add(row << 2);
                let mission = lbu(m, block.wrapping_add(index), 6);
                sh(m, context, 0x38c, mission);
                let buffer = lw(m, h, 0x28);
                call(m, 0x8002_7538, [buffer, 0, 0, 0])
            }
            2 => {
                for offset in [0x7c, 0x7d] {
                    let h = hub(m);
                    sb(m, h, offset, 0);
                }
                let h = hub(m);
                let buffer = lw(m, h, 0x28);
                call(m, 0x8002_7538, [buffer, 0, 0, 0])
            }
            _ => 2,
        }
    })
}

// ---- the unlock tables ------------------------------------------------------------

/// 800FE668: item `a1` relative to the first item of its group
/// ([`item_group`]`(a0, a1)`; jump table 0x800CD6BC: group 0 -> 800FE6B0
/// (-0xA), 1 -> 800FE6A8 (-1), 2 -> 800FE6B8 (-0x12), 3 -> 800FE6C0
/// (-0x1A), 4 -> 800FE6C8 (-0x1F); other groups leave `a1`).
pub fn index_in_group(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let first = match item_group(m, a0, a1) {
            0 => 0xa,
            1 => 1,
            2 => 0x12,
            3 => 0x1a,
            4 => 0x1f,
            _ => 0,
        };
        a1.wrapping_sub(first)
    })
}

/// 800FE6E0: the group of item `a1` when context +0x96C bit 4 is set (signed:
/// < 0xA -> 1, < 0x12 -> 0, < 0x1A -> 2, < 0x1F -> 3, else 4), else the
/// word +0 of mission record `a0` (0 when `a0` >= 0x3C).
pub fn item_group(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    let context = ctx(m);
    if lw(m, context, 0x96c) & 0x10 != 0 {
        return if slt(a1, 0xa) {
            1
        } else if slt(a1, 0x12) {
            0
        } else if slt(a1, 0x1a) {
            2
        } else if slt(a1, 0x1f) {
            3
        } else {
            4
        };
    }
    let mission = if a0 < 0x3c { a0 } else { 0 };
    lw(m, rec12(MISSIONS, mission), 0)
}

/// 800FE764: with context +0x96C bit 4, the number of unlocked items
/// (context +0x974 + n == 1) in group `a1` (jump table 0x800CD6D4: 0 ->
/// 800FE7B4 0xA..0x12, 1 -> 800FE7A8 1..0xA, 2 -> 800FE7C0 0x12..0x1A, 3 ->
/// 800FE7CC 0x1A..0x1F, 4 -> 800FE7D8 0x1F..0x25; else 0); without it the
/// item count of mission record `a0` (unchecked).
pub fn items_unlocked(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    let context = ctx(m);
    if lw(m, context, 0x96c) & 0x10 != 0 {
        let (first, end): (u32, u32) = match a1 {
            0 => (0xa, 0x12),
            1 => (1, 0xa),
            2 => (0x12, 0x1a),
            3 => (0x1a, 0x1f),
            4 => (0x1f, 0x25),
            _ => return 0,
        };
        let context = ctx(m);
        let mut unlocked = 0u32;
        for item in first..end {
            if lbu(m, context.wrapping_add(item), 0x974) == 1 {
                unlocked += 1;
            }
        }
        return unlocked;
    }
    lbu(m, rec12(MISSIONS, a0), 4)
}

/// 800FE864: 0 with context +0x96C bit 4, else [`index_in_group`] of item
/// slot `a1` (0 when >= 7) of mission record `a0` (0 when >= 0x3C).
pub fn mission_item_index(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = ctx(m);
        if lw(m, context, 0x96c) & 0x10 != 0 {
            return 0;
        }
        let mission = if a0 < 0x3c { a0 } else { 0 };
        let slot = if a1 < 7 { a1 } else { 0 };
        let item = lbu(m, rec12(MISSIONS, mission).wrapping_add(slot), 5);
        index_in_group(m, mission, item)
    })
}

/// 800FE8D8: with context +0x96C bit 4 the halfword `[HUB]+0x1E0 + 2 a1`
/// (`a1` 0 when >= 0x40, signed), else item slot `a1` (0 when >= 7) of
/// mission record `a0` (unchecked).
pub fn mission_item(m: &mut Machine, a0: u32, a1: u32) -> u32 {
    let context = ctx(m);
    if lw(m, context, 0x96c) & 0x10 != 0 {
        let slot = if a1 < 0x40 { a1 } else { 0 };
        let h = hub(m);
        return lh(m, h.wrapping_add(slot << 1), 0x1e0);
    }
    let slot = if a1 < 7 { a1 } else { 0 };
    lbu(m, rec12(MISSIONS, a0).wrapping_add(slot), 5)
}

/// 800FE950: unlock the items of mission `a0` (0 when >= 0x3C): context
/// +0x974 + item = 1. Returns 0, or the record address when it has none.
pub fn unlock_mission_items(m: &mut Machine, a0: u32) -> u32 {
    let mission = if a0 < 0x3c { a0 } else { 0 };
    let record = rec12(MISSIONS, mission);
    let count = lbu(m, record, 4);
    if count == 0 {
        return record;
    }
    for i in 0..count {
        let context = ctx(m);
        let item = lbu(m, record.wrapping_add(i), 5);
        sb(m, context.wrapping_add(item), 0x974, 1);
    }
    0
}

/// 800FE9C0: the variant count of item `a1` (0 when >= 0x25) for mission
/// `a0`: with `a2` == 0 the item's byte +0; else 1 for mission 0xD, the
/// byte +1 for 0x2A, and otherwise the byte +1 plus 1 with context +0x96C
/// bit 4 (plus 2 when `[HUB]+0x26C` bit 3 is set and the mission is not
/// 0x20/0x2F/0x35/0x37), or plus 1 for the missions 0xE/0x1C/0x1D/0x2B/0x2C.
pub fn variant_count(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    let item = if a1 < 0x25 { a1 } else { 0 };
    if a2 == 0 {
        return lbu(m, rec12(ITEMS, item), 0);
    }
    if a0 == 0xd {
        return 1;
    }
    let count = lbu(m, rec12(ITEMS, item), 1);
    if a0 == 0x2a {
        return count;
    }
    let context = ctx(m);
    if lw(m, context, 0x96c) & 0x10 != 0 {
        // the delay slot of the first compare adds 1 on every path
        let count = count + 1;
        if matches!(a0, 0x20 | 0x2f | 0x35 | 0x37) {
            return count;
        }
        let h = hub(m);
        if lw(m, h, 0x26c) & 8 == 0 {
            count
        } else {
            count + 1
        }
    } else if matches!(a0, 0x0e | 0x1c | 0x1d | 0x2b | 0x2c) {
        count + 1
    } else {
        count
    }
}

/// 800FEAD4: the default variant slot of item `a1` (0 when >= 0x25): with
/// `a2` == 0 the first of +4..+7 equal to the byte +2; else 0 for mission
/// 0xD, the byte +1 for the missions 0xE/0x1C/0x1D/0x2B/0x2C, otherwise the
/// first of +8..+0xB equal to the byte +3. 0 when none matches.
pub fn default_variant(m: &mut Machine, a0: u32, a1: u32, a2: u32) -> u32 {
    let item = if a1 < 0x25 { a1 } else { 0 };
    let record = rec12(ITEMS, item);
    if a2 == 0 {
        let wanted = lbu(m, record, 2);
        for slot in 0..4u32 {
            if lbu(m, record.wrapping_add(slot), 4) == wanted {
                return slot;
            }
        }
        return 0;
    }
    if a0 == 0xd {
        return 0;
    }
    if matches!(a0, 0x0e | 0x1c | 0x1d | 0x2b | 0x2c) {
        return lbu(m, record, 1);
    }
    let wanted = lbu(m, record, 3);
    for slot in 0..4u32 {
        if lbu(m, record.wrapping_add(slot), 8) == wanted {
            return slot;
        }
    }
    0
}

/// 800FEBB8: variant `a3` of item `a1` (0 when >= 0x25): with `a2` == 0 the
/// byte +4 + `a3` (`a3` 0 when >= 4); else 8 for mission 0xD, 6 when `a3`
/// is the byte +1 plus 1, 7 when it is the byte +1, otherwise the byte +8 +
/// `a3` (0 when >= 4); plus 6 when `a2` == 2.
pub fn variant(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
    let item = if a1 < 0x25 { a1 } else { 0 };
    if a2 == 0 {
        let slot = if a3 < 4 { a3 } else { 0 };
        return lbu(m, rec12(ITEMS, item).wrapping_add(slot), 4);
    }
    let value = if a0 == 0xd {
        8
    } else {
        let record = rec12(ITEMS, item);
        let count = lbu(m, record, 1);
        if a3 == count + 1 {
            6
        } else if a3 == count {
            7
        } else {
            let slot = if a3 < 4 { a3 } else { 0 };
            lbu(m, record.wrapping_add(slot), 8)
        }
    };
    if a2 == 2 { value + 6 } else { value }
}

/// 800FEC84: after mission `a0` (0 when >= 0x3C): set context +0x9C2 for
/// mission 0xD or +0x9C1 for 0xE/0x1C/0x1D/0x2B/0x2C (jump table
/// 0x800CD6EC, `a0 - 0xD`: 0 -> 800FECC0, 1/15/16/30/31 -> 800FECD4, the
/// others 800FECE4), then for each item 1..=0x24 of the mission record set
/// the bytes context +0x9B4 + (+4.. list) and +0x9BA + (+8.. list) of the
/// item record to 1. Returns `count + 12 * mission` (the last delay slot),
/// or the record address when it has no items.
pub fn unlock_variants(m: &mut Machine, a0: u32) -> u32 {
    let mission = if a0 < 0x3c { a0 } else { 0 };
    match mission {
        0x0d => {
            let context = ctx(m);
            sb(m, context, 0x9c2, 1);
        }
        0x0e | 0x1c | 0x1d | 0x2b | 0x2c => {
            let context = ctx(m);
            sb(m, context, 0x9c1, 1);
        }
        _ => {}
    }
    let record = rec12(MISSIONS, mission);
    let count = lbu(m, record, 4);
    if count == 0 {
        return record;
    }
    for i in 0..count {
        let item = lbu(m, record.wrapping_add(i), 5);
        if item.wrapping_sub(1) >= 0x24 {
            continue;
        }
        let parts = rec12(ITEMS, item);
        let first = lbu(m, parts, 0);
        for n in 0..first {
            let context = ctx(m);
            let part = lbu(m, parts.wrapping_add(n), 4);
            sb(m, context.wrapping_add(part), 0x9b4, 1);
        }
        let second = lbu(m, parts, 1);
        for n in 0..second {
            let context = ctx(m);
            let part = lbu(m, parts.wrapping_add(n), 8);
            sb(m, part.wrapping_add(context), 0x9ba, 1);
        }
    }
    count.wrapping_add(rec12(0, mission))
}

/// 800FEDEC: for each of the 62 entries `n` whose four bytes at
/// `0x80112228 + 4n` contain `a0`, set context +0xD48 + n (when 0) to 1.
/// Returns 1.
pub fn mark_by_byte(m: &mut Machine, a0: u32) -> u32 {
    for entry in 0..0x3eu32 {
        for slot in 0..4u32 {
            if lbu(m, MARK_BYTES, (entry << 2) + slot) == a0 {
                let context = ctx(m);
                let at = context.wrapping_add(entry);
                if lbu(m, at, 0xd48) == 0 {
                    sb(m, at, 0xd48, 1);
                }
            }
        }
    }
    1
}

/// 800FEE68: for each of the 62 entries `n` whose eight halfwords at
/// 0x80112330 + 16n contain `a0` (signed), set context +0xD48 + n (when 0)
/// to 1. Returns 0 (the last loop compare).
pub fn mark_by_half(m: &mut Machine, a0: u32) -> u32 {
    for entry in 0..0x3eu32 {
        for slot in 0..8u32 {
            if lh(m, MARK_HALVES, (entry << 4) + (slot << 1)) == a0 {
                let context = ctx(m);
                let at = context.wrapping_add(entry);
                if lbu(m, at, 0xd48) == 0 {
                    sb(m, at, 0xd48, 1);
                }
            }
        }
    }
    0
}

// ---- the disc check ---------------------------------------------------------------

/// 800FEEE0 (JP; PAL 800E182C, changed): the disc-check message
/// `[[0x8011278C]+0]`.
pub fn check_message(m: &mut Machine) -> u32 {
    let block = m.lw(CHECK);
    lw(m, block, 0)
}

/// 800FEEF8 (JP; PAL 800CBA5C, changed): store the mission `a0` at
/// `[0x8011278C]+8`. Returns the block.
pub fn check_set_mission(m: &mut Machine, a0: u32) -> u32 {
    let block = m.lw(CHECK);
    sw(m, block, 8, a0);
    block
}

/// 800FEF08 (JP; PAL 800D0254, changed), disc-check step 0: clear the
/// message and the swapped flag. Returns 1 (next step).
pub fn check_reset(m: &mut Machine) -> u32 {
    let block = m.lw(CHECK);
    sw(m, block, 4, 0);
    sw(m, block, 0, 0);
    1
}

/// 800FEF24, disc-check step 1: compare the disc 8002B504 with the disc of
/// the chosen mission (0x80112750): no disc -> message 1, returns 0; a
/// mission on both discs or disc 3 -> message 0, returns 2; the right disc
/// (1 or 2) -> message 6, returns 2; the wrong disc or an unreadable one
/// (4, -1) -> the "insert" message (2/3, or 4/5 once swapped), 8002B428,
/// returns 1; any other value returns 0.
pub fn check_disc(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let disc = call(m, 0x8002_b504, [0; 4]);
        if disc == 0 {
            let block = m.lw(CHECK);
            sw(m, block, 0, 1);
            return 0;
        }
        let block = m.lw(CHECK);
        let mission = lw(m, block, 8);
        let wanted = lbu(m, mission.wrapping_add(DISC_OF), 0);
        if wanted == 3 || disc == 3 {
            sw(m, block, 0, 0);
            return 2;
        }
        if disc == 4 || disc == u32::MAX {
            if wanted == 1 {
                let message = if lw(m, block, 4) != 0 { 4 } else { 2 };
                sw(m, block, 0, message);
            }
            let block = m.lw(CHECK);
            let mission = lw(m, block, 8);
            if lbu(m, mission.wrapping_add(DISC_OF), 0) == 2 {
                let message = if lw(m, block, 4) != 0 { 5 } else { 3 };
                sw(m, block, 0, message);
            }
        } else if disc == 1 {
            if wanted == 1 {
                sw(m, block, 0, 6);
                return 2;
            }
            let message = if lw(m, block, 4) != 0 { 5 } else { 3 };
            sw(m, block, 0, message);
        } else if disc == 2 {
            if wanted == 2 {
                sw(m, block, 0, 6);
                return 2;
            }
            let message = if lw(m, block, 4) != 0 { 4 } else { 2 };
            sw(m, block, 0, message);
        } else {
            return 0;
        }
        call(m, 0x8002_b428, [0; 4]);
        1
    })
}

/// 800FF098 (JP; PAL 800E46EC, near), disc-check step 2: 1 (next step) when
/// 8002B528 returns 3, else 0.
pub fn check_lid_open(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| u32::from(call(m, 0x8002_b528, [0; 4]) == 3))
}

/// 800FF0BC (JP; PAL 800E47AC, changed), disc-check step 3: when 8002B528
/// returns 1, set the message 1 and the swapped flag and return 4 (back to
/// step 1), else 0.
pub fn check_lid_closed(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let state = call(m, 0x8002_b528, [0; 4]);
        if state != 1 {
            return 0;
        }
        let block = m.lw(CHECK);
        sw(m, block, 0, state);
        sw(m, block, 4, state);
        4
    })
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800f_d770 => entry_picture(m, a0, a1),
        0x800f_d7d0 => draw_row(m, a0),
        0x800f_d8f4 => draw_selected(m, a0),
        0x800f_d958 => list_open(m),
        0x800f_d9cc => list_update(m),
        0x800f_de78 => list_close(m),
        0x800f_df38 => difficulty_open(m),
        0x800f_df64 => difficulty_update(m),
        0x800f_e0d4 => difficulty_close(m),
        0x800f_e180 => disc_check_open(m),
        0x800f_e1e8 => disc_check_update(m),
        0x800f_e444 => select_initialize(m),
        0x800f_e564 => next_substate(m),
        0x800f_e584 => select_update(m),
        0x800f_e668 => index_in_group(m, a0, a1),
        0x800f_e6e0 => item_group(m, a0, a1),
        0x800f_e764 => items_unlocked(m, a0, a1),
        0x800f_e864 => mission_item_index(m, a0, a1),
        0x800f_e8d8 => mission_item(m, a0, a1),
        0x800f_e950 => unlock_mission_items(m, a0),
        0x800f_e9c0 => variant_count(m, a0, a1, a2),
        0x800f_ead4 => default_variant(m, a0, a1, a2),
        0x800f_ebb8 => variant(m, a0, a1, a2, a3),
        0x800f_ec84 => unlock_variants(m, a0),
        0x800f_edec => mark_by_byte(m, a0),
        0x800f_ee68 => mark_by_half(m, a0),
        0x800f_eee0 => check_message(m),
        0x800f_eef8 => check_set_mission(m, a0),
        0x800f_ef08 => check_reset(m),
        0x800f_ef24 => check_disc(m),
        0x800f_f098 => check_lid_open(m),
        0x800f_f0bc => check_lid_closed(m),
        _ => return None,
    })
}
