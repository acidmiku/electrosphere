//! JP overlay 2, part 1: the hub OPTIONS menu (hub screen group 4) and
//! the key-configuration presets.
//!
//! The options block `K` ([`OPTIONS`], the 0x144-byte block of load list
//! 0x73): +0 the chosen screen, +1 the menu phase, +2, +3, +4 the menu
//! item (a 3 x 2 grid: 0 KEY CONFIG, 1 GAME CONFIG, 2 RECORDS, 3 SAVE, 4
//! and 5 the bottom row), +5 the first-visit flag; +0x24.. the button
//! assignment bytes, +0x84.. and +0x9C.. the analog settings, +0xB4/+0xB5
//! the changed marks.
//!
//! - Menu phases (run by 800DE3CC): 800D240C open, 800D24E8 wait for the
//!   panels, 800D2568 the menu input, 800D2BCC open the chosen item.
//! - 800D180C, 800D18E4, 800D19A8, 800D1A3C: the four key presets (JP,
//!   changed from PAL 800DE73C..800DE978); 800D1B04 (exact, PAL 800DEA48)
//!   the controller type index.
use super::ov2::MODULE;
use super::ov2_p1::{SET_FLAG, at, call, context, lh, work};
use super::ov2_p1_pages::{
    CURSOR, INSTALL_BANK, LIST_BLOCK, PANEL_RESTART, PANEL_SLIDE, PANEL_STATE, SET_CURSOR, SET_LAMP,
};
use crate::common::loaded_module;
use crate::machine::Machine;

/// `[0x80112DDC]`: the options block `K` (the load lists' block).
pub const OPTIONS: u32 = LIST_BLOCK;

fn options(m: &mut Machine) -> u32 {
    m.lw(OPTIONS)
}

fn sound(m: &mut Machine, id: u32) -> u32 {
    super::ov2_p1::sound(m, id)
}

/// Byte stores at `base + offset` in the original order.
fn store_bytes(m: &mut Machine, base: u32, bytes: &[(u32, u8)]) {
    for &(offset, value) in bytes {
        m.sb(at(base, offset), value);
    }
}

/// The presets' last two stores: the changed marks +0xB5 and +0xB4
/// (each with the block pointer re-read). Returns the block pointer.
fn mark(m: &mut Machine) -> u32 {
    let k = options(m);
    m.sb(at(k, 0xb5), 1);
    let k = options(m);
    m.sb(at(k, 0xb4), 1);
    k
}

/// 800D180C (JP; changed from PAL 800DE73C): preset A: the button bytes
/// +0x24..+0x32, the analog bytes +0x84..+0x87 and +0x9C..+0x9F, and the
/// changed marks. Returns the block pointer.
pub fn preset_a_d180c(m: &mut Machine) -> u32 {
    let k = options(m);
    store_bytes(
        m,
        at(k, 0x24),
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
    let k = options(m);
    store_bytes(m, at(k, 0x84), &[(0, 0), (1, 2), (2, 8), (3, 0xa)]);
    let k = options(m);
    store_bytes(m, at(k, 0x9c), &[(0, 1), (1, 0), (2, 0), (3, 0)]);
    mark(m)
}

/// 800D18E4 (JP): preset B: the button bytes +0x34..+0x42, the analog
/// bytes +0x88..+0x8B and +0xA0..+0xA3, and the changed marks. Returns
/// the block pointer.
pub fn preset_b_d18e4(m: &mut Machine) -> u32 {
    let k = options(m);
    store_bytes(
        m,
        at(k, 0x34),
        &[
            (0, 0x14),
            (1, 0xe),
            (3, 5),
            (4, 4),
            (8, 0xd),
            (9, 0x10),
            (0xa, 7),
            (2, 1),
            (5, 0),
            (6, 0x14),
            (7, 0x14),
            (0xb, 0xc),
            (0xc, 6),
            (0xd, 0x14),
            (0xe, 0x14),
        ],
    );
    let k = options(m);
    store_bytes(m, at(k, 0x88), &[(0, 0xc), (1, 0xc), (2, 0xc), (3, 2)]);
    let k = options(m);
    store_bytes(m, at(k, 0xa0), &[(0, 0), (1, 0), (2, 0), (3, 0)]);
    mark(m)
}

/// 800D19A8 (JP; changed from PAL 800DE8E0): preset C: the button bytes
/// +0x44..+0x52 and the changed marks. Returns the block pointer.
pub fn preset_c_d19a8(m: &mut Machine) -> u32 {
    let k = options(m);
    store_bytes(
        m,
        at(k, 0x44),
        &[
            (0, 4),
            (1, 7),
            (3, 3),
            (4, 2),
            (7, 5),
            (8, 6),
            (9, 0xe),
            (0xb, 0xc),
            (0xa, 0x10),
            (0xc, 0xd),
            (2, 1),
            (5, 0),
            (6, 0x14),
            (0xd, 0x14),
            (0xe, 0x13),
        ],
    );
    mark(m)
}

/// 800D1A3C (JP; changed from PAL 800DE978): preset D: the button bytes
/// +0x54..+0x62, the analog bytes +0x90..+0x93 and +0xA8..+0xAB, and the
/// changed marks. Returns the block pointer.
pub fn preset_d_d1a3c(m: &mut Machine) -> u32 {
    let k = options(m);
    store_bytes(
        m,
        at(k, 0x54),
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
    let k = options(m);
    store_bytes(m, at(k, 0x90), &[(0, 6), (1, 4), (2, 0), (3, 2)]);
    let k = options(m);
    store_bytes(m, at(k, 0xa8), &[(0, 0), (1, 0), (2, 1), (3, 0)]);
    mark(m)
}

/// 800D1B04 (exact, PAL 800DEA48): the index of the lowest of bits 0..5
/// set in `bits`, else -1.
pub fn type_index_d1b04(bits: u32) -> u32 {
    (0..6).find(|&k| bits & (1 << k) != 0).unwrap_or(u32::MAX)
}

/// 800D240C (menu phase 0): install the options bank (`[W+0x98]`), lamp
/// 0 = 3, lamp 2 = 1 on the first visit (`K+5` clear; menu phase 1) or 2
/// (phase 4), cursors 1 and 2 = 1 (0, 0x28), hub flags 1 = 4, 5 = the
/// item and 0x3E = 1. Returns 800DEF94's result.
pub fn open_d240c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let w = work(m);
    let bank = m.lw(at(w, 0x98));
    call(m, INSTALL_BANK, [bank, 0, 0, 0]);
    call(m, SET_LAMP, [0, 3, 0, 0]);
    let k = options(m);
    let phase = if m.lbu(at(k, 5)) == 0 {
        call(m, SET_LAMP, [2, 1, 0, 0]);
        1
    } else {
        call(m, SET_LAMP, [2, 2, 0, 0]);
        4
    };
    let k = options(m);
    m.sb(at(k, 1), phase);
    call(m, SET_CURSOR, [1, 1, 0, 0x28]);
    call(m, SET_CURSOR, [2, 1, 0, 0x28]);
    call(m, SET_FLAG, [1, 4, 0, 0]);
    let k = options(m);
    let item = u32::from(m.lbu(at(k, 4)));
    call(m, SET_FLAG, [5, item, 0, 0]);
    let result = call(m, SET_FLAG, [0x3e, 1, 0, 0]);
    m.pop_frame(0x18);
    result
}

/// 800D24E8 (menu phase 1): count `W+0x1A4` while panel 0's first entry
/// is not at rest; once hub cursor 1 is 2 the menu phase becomes 2. Hub
/// flag 5 = the item. Returns 800DEF94's result.
pub fn wait_d24e8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    if call(m, PANEL_STATE, [0, 1, 0, 0]) == 0 {
        let w = work(m);
        let count = m.lw(at(w, 0x1a4)).wrapping_add(1);
        m.sw(at(w, 0x1a4), count);
    }
    let cursor = call(m, CURSOR, [1, 0, 0, 0]);
    if cursor == 2 {
        let k = options(m);
        m.sb(at(k, 1), cursor as u8);
    }
    let k = options(m);
    let item = u32::from(m.lbu(at(k, 4)));
    let result = call(m, SET_FLAG, [5, item, 0, 0]);
    m.pop_frame(0x18);
    result
}

/// The menu's grid moves of item `item` for the pressed bits of
/// `context+0x130` (up/down 0x3, right 0x8, left 0x4), each re-read from
/// the context; `wide` is context option bit 0x10, which changes three of
/// the moves. Items 4 and 5 first clear hub flag 0x3E. Returns whether a
/// move was taken.
fn grid_move(m: &mut Machine, item: u8, wide: bool) -> bool {
    let pressed = |m: &mut Machine, bits: u32| {
        let context = context(m);
        m.lw(at(context, 0x130)) & bits != 0
    };
    let set = |m: &mut Machine, value: u8| {
        let k = options(m);
        m.sb(at(k, 4), value);
    };
    let mut moved = false;
    match item {
        0 => {
            if pressed(m, 3) && !wide {
                moved = true;
                set(m, 2);
            }
            if pressed(m, 8) {
                moved = true;
                set(m, 1);
            }
            if pressed(m, 4) {
                moved = true;
                set(m, 4);
            }
        }
        1 => {
            if pressed(m, 3) {
                moved = true;
                set(m, 3);
            }
            if pressed(m, 8) {
                moved = true;
                set(m, 4);
            }
            if pressed(m, 4) {
                set(m, 0);
                moved = true;
            }
        }
        2 => {
            if pressed(m, 3) {
                set(m, 0);
                moved = true;
            }
            if pressed(m, 8) {
                moved = true;
                set(m, 3);
            }
            if pressed(m, 4) {
                moved = true;
                set(m, 5);
            }
        }
        3 => {
            if pressed(m, 3) {
                moved = true;
                set(m, 1);
            }
            if pressed(m, 8) {
                moved = true;
                set(m, 5);
            }
            if pressed(m, 4) {
                moved = true;
                set(m, if wide { 5 } else { 2 });
            }
        }
        4 => {
            call(m, SET_FLAG, [0x3e, 0, 0, 0]);
            if pressed(m, 8) {
                set(m, 0);
                moved = true;
            }
            if pressed(m, 4) {
                moved = true;
                set(m, 1);
            }
        }
        5 => {
            call(m, SET_FLAG, [0x3e, 0, 0, 0]);
            if pressed(m, 8) {
                moved = true;
                set(m, if wide { 3 } else { 2 });
            }
            if pressed(m, 4) {
                moved = true;
                set(m, 3);
            }
        }
        _ => {}
    }
    moved
}

/// Whether item `item` is on the bottom row (4 or 5).
fn bottom(item: u8) -> bool {
    item.wrapping_sub(4) < 2
}

/// 800D2568 (menu phase 2): hub flag 0x3E = 1, then the item's grid move
/// (sound 0x3D for an up/down move off the bottom row and for a left or
/// right move), hub flag 5 = the item. Confirm (triangle 0x40 on the
/// bottom row, else 0x10; none of 0x2F held; not while `W+0x90` is 5)
/// sets menu phase 3: cursors 1, 2 (and 0xF) = 3, panel 0 restarts, a
/// RECORDS choice (2) loads (load phase 1), sounds 0x3C and 0x5B. Cancel
/// (0x20, none of 0x5F) sets phase 3 with `K+3` = 1 (leave), cursors 1 and
/// 2 = 3, sounds 0x3B and 0x5B, and clears the pressed bits. Returns the
/// context pointer after a cancel, else the last button test's value.
pub fn menu_d2568(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let context = context(m);
    let options_bits = m.lw(at(context, 0x96c));
    let wide = options_bits & 0x10 != 0;
    call(m, SET_FLAG, [0x3e, 1, 0, 0]);
    let k = options(m);
    let item = m.lbu(at(k, 4));
    let moved = item < 6 && grid_move(m, item, wide);
    let k = options(m);
    if !bottom(m.lbu(at(k, 4))) {
        let context = self::context(m);
        if m.lw(at(context, 0x130)) & 3 != 0 && moved {
            sound(m, 0x3d);
        }
    }
    let context = self::context(m);
    if m.lw(at(context, 0x130)) & 0xc != 0 && moved {
        sound(m, 0x3d);
    }
    let k = options(m);
    let item = u32::from(m.lbu(at(k, 4)));
    call(m, SET_FLAG, [5, item, 0, 0]);
    let k = options(m);
    let item = m.lbu(at(k, 4));
    let context = self::context(m);
    let confirm = if bottom(item) { 0x40 } else { 0x10 };
    let confirmed =
        m.lw(at(context, 0x12c)) & confirm != 0 && m.lw(at(context, 0x128)) & 0x2f == 0 && {
            let w = work(m);
            m.lw(at(w, 0x90)) != 5
        };
    if confirmed {
        m.sb(at(k, 1), 3);
        let k = options(m);
        m.sb(at(k, 3), 0);
        call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
        if bottom(item) {
            call(m, PANEL_RESTART, [0, 1, 0, 0]);
            call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
            call(m, SET_CURSOR, [0xf, 3, u32::MAX, u32::MAX]);
        } else {
            let k = options(m);
            if m.lbu(at(k, 4)) == 2 {
                let w = work(m);
                m.sh(at(w, 0x1c6), 1);
            }
            let k = options(m);
            if m.lbu(at(k, 4)) == 3 {
                call(m, SET_CURSOR, [0xf, 3, u32::MAX, u32::MAX]);
            }
            call(m, PANEL_RESTART, [0, 1, 0, 0]);
            call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        }
        sound(m, 0x3c);
        sound(m, 0x5b);
    }
    let context = self::context(m);
    let cancel = m.lw(at(context, 0x12c)) & 0x20;
    let result = if cancel == 0 {
        0
    } else {
        let held = m.lw(at(context, 0x128)) & 0x5f;
        if held != 0 {
            held
        } else {
            let k = options(m);
            m.sb(at(k, 1), 3);
            let k = options(m);
            m.sb(at(k, 3), 1);
            call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
            call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
            sound(m, 0x3b);
            sound(m, 0x5b);
            let context = self::context(m);
            m.sw(at(context, 0x12c), 0);
            context
        }
    };
    m.pop_frame(0x20);
    result
}

/// 800D2BCC (menu phase 3): once hub cursor 1 is 0 and the loader idle
/// (load phase 0), open the choice: on the first visit (`K+5` = 1) menu
/// phase 4; leaving (`K+3` = 1) `K+2` = 1; otherwise the chosen screen
/// `K+0` (1 KEY CONFIG, 3 GAME CONFIG, 5 RECORDS, 6 SAVE with lamps 2 = 0
/// and 0 = 0x8000 and panel 0's entry 0xF sliding, 0xA the bottom row
/// with lamps 2 = 0 and 0 = 0), each with `W+0x86` = 0 where it has
/// phases, cursors 1 and 2 = 1 (0, 0x28), menu phase 0 and sound 0x5A.
/// Returns the sound's handle, or the last value the original tested.
pub fn choose_d2bcc(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let result = choose(m);
    m.pop_frame(0x20);
    result
}

fn choose(m: &mut Machine) -> u32 {
    let cursor = call(m, CURSOR, [1, 0, 0, 0]);
    if cursor != 0 {
        return cursor;
    }
    let w = work(m);
    let phase = lh(m, at(w, 0x1c6));
    if phase != 0 {
        return phase;
    }
    let k = options(m);
    if m.lbu(at(k, 5)) == 1 {
        m.sb(at(k, 1), 4);
        return 4;
    }
    let leave = m.lbu(at(k, 3));
    if leave == 1 {
        m.sb(at(k, 2), 1);
        return 1;
    }
    if leave != 0 {
        return u32::from(leave);
    }
    if m.lbu(at(k, 4)) == 0 {
        m.sb(k, 1);
    }
    let k = options(m);
    if m.lbu(at(k, 4)) == 1 {
        m.sb(k, 3);
    }
    let k = options(m);
    if m.lbu(at(k, 4)) == 2 {
        m.sb(k, 5);
    }
    let k = options(m);
    if m.lbu(at(k, 4)) == 3 {
        let w = work(m);
        m.sh(at(w, 0x86), 0);
        m.sb(k, 6);
        call(m, SET_LAMP, [2, 0, 0, 0]);
        call(m, SET_LAMP, [0, 0x8000, 0, 0]);
        call(m, PANEL_SLIDE, [0, 0xf, 0, 0]);
    }
    let k = options(m);
    if bottom(m.lbu(at(k, 4))) {
        let w = work(m);
        m.sh(at(w, 0x86), 0);
        m.sb(k, 0xa);
        call(m, SET_LAMP, [2, 0, 0, 0]);
        call(m, SET_LAMP, [0, 0, 0, 0]);
    }
    call(m, SET_CURSOR, [1, 1, 0, 0x28]);
    let k = options(m);
    m.sb(at(k, 1), 0);
    call(m, SET_CURSOR, [2, 1, 0, 0x28]);
    sound(m, 0x5a)
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800d_180c => preset_a_d180c(m),
        0x800d_18e4 => preset_b_d18e4(m),
        0x800d_19a8 => preset_c_d19a8(m),
        0x800d_1a3c => preset_d_d1a3c(m),
        0x800d_1b04 => type_index_d1b04(args[0]),
        0x800d_240c => open_d240c(m),
        0x800d_24e8 => wait_d24e8(m),
        0x800d_2568 => menu_d2568(m),
        0x800d_2bcc => choose_d2bcc(m),
        _ => return None,
    })
}
