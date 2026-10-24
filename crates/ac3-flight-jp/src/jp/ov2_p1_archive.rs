//! JP overlay 2, part 1: the Archive (hub screen group 3, the message
//! folder) and two load-phase pairs.
//!
//! - Screens: 0 800D09E8 start (lists the received messages, context
//!   +0xD98 for numbers 0x1A..0xAE), 1 800D0C34 the list (the Search
//!   Engine's scrolling, `ov2_p1_search::scroll`; confirm replays the
//!   message, cancel leaves), 2 800D1214 the replay, 3 800D12E0 back to the
//!   list, 4 800D1258 the replay's start, 5 800D129C leaving; screens 2, 4
//!   and 5 run the phase `W+0x86` of their own tables.
//! - Drawing: 800D0830 the page's two icons per message, 800D0970 the
//!   cursor's picture (both from [`MESSAGES`]).
//! - Load phases of other screens' tables: 800D1418/800D14A0 (list 0x7C)
//!   and 800D14D0/800D1558 (list 0x7D).
//!
//! The archive block `A` ([`ARCHIVE`], 0xBC bytes, allocated by the hub
//! menu): +0 phase, +1, +2 list page, +3 cursor, +4 the listed message
//! numbers, +0xB8 their count.
use super::ov2::MODULE;
use super::ov2_p1::{SET_FLAG, at, call, context, lh, lt, slot, work};
use super::ov2_p1_pages::{ARCHIVE, PANEL_START, SET_CURSOR, SET_LAMP, run_load_phase, start_list};
use super::ov2_p1_search::{DRAW_ICON, List, SCROLL_TIMER, arrows, lamps, scroll, set_flag_byte};
use crate::common::loaded_module;
use crate::machine::Machine;

/// 80109E28: eight-byte records by message number (+5 the picture index).
pub const MESSAGES: u32 = 0x8010_9e28;
/// 8010BA98, 8010B9F0, 8010BA08: the phase tables of screens 2 (replay),
/// 4 (replay start) and 5 (leave), by `W+0x86`.
pub const REPLAY: u32 = 0x8010_ba98;
pub const REPLAY_START: u32 = 0x8010_b9f0;
pub const LEAVE: u32 = 0x8010_ba08;
/// 8002B528 (resident, PAL 8002B100): 1 while the disc cannot stream
/// (replays are refused).
pub const STREAM_BUSY: u32 = 0x8002_b528;

/// The Archive's list.
pub(crate) const LIST: List = List {
    block: ARCHIVE,
    entries: 4,
    count: 0xb8,
    mark: 2,
    page: page_d0830,
    picture: picture_d0970,
    sound_first: false,
};

fn archive(m: &mut Machine) -> u32 {
    m.lw(ARCHIVE)
}

/// 800D0830: the icons of list page `page` (four per page, the last page
/// the remainder): each message's icon (`[W+0x1D4]`, index from 0x1A) at
/// x 0x3C0 and its sender's (`[W+0x1D8]` by [`MESSAGES`] +5) at 0x3E0, one
/// row of 0x12 lines each from y 0x154, mode 6. Returns 0, or the page's
/// first index when it is empty.
pub fn page_d0830(m: &mut Machine, page: u32) -> u32 {
    let frame = m.push_frame(0x40);
    let a = archive(m);
    let count = m.lw(at(a, 0xb8));
    let pages = (count as i32 / 4) as u32;
    let mut result = 1;
    let rows = if lt(page, pages) {
        4
    } else {
        result = pages << 2;
        count.wrapping_sub(pages << 2)
    };
    let mut y = 0x154u32;
    let mut row = 0u32;
    while lt(row, rows) {
        let index = row.wrapping_add(page << 2);
        let a = archive(m);
        let w = work(m);
        let message = u32::from(m.lbu(at(a, index).wrapping_add(4)));
        let images = m.lw(at(w, 0x1d4));
        let image = m.lw(slot(images, message, 0).wrapping_sub(0x68));
        m.sw(at(frame, 0x10), 6);
        call(m, DRAW_ICON, [image, 0x3c0, y, 0x180]);
        row = row.wrapping_add(1);
        let a = archive(m);
        let w = work(m);
        let message = u32::from(m.lbu(at(a, index).wrapping_add(4)));
        let senders = m.lw(at(w, 0x1d8));
        let sender = u32::from(m.lbu(MESSAGES.wrapping_add(message << 3).wrapping_add(5)));
        m.sw(at(frame, 0x10), 6);
        let image = m.lw(slot(senders, sender, 0));
        call(m, DRAW_ICON, [image, 0x3e0, y, 0x180]);
        y = y.wrapping_add(0x12);
        result = 0;
    }
    m.pop_frame(0x40);
    result
}

/// 800D0970: the picture of list entry `cursor` of the current page
/// (`[W+0x1DC]` by [`MESSAGES`] +5, at (0x3C0, 0x100), clut 0x300, mode
/// 1). Returns 800ED000's result.
pub fn picture_d0970(m: &mut Machine, cursor: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let a = archive(m);
    let first = u32::from(m.lbu(at(a, 2))) << 2;
    let index = cursor.wrapping_add(first);
    let message = u32::from(m.lbu(at(a, index).wrapping_add(4)));
    let w = work(m);
    let picture = u32::from(m.lbu(MESSAGES.wrapping_add(message << 3).wrapping_add(5)));
    let pictures = m.lw(at(w, 0x1dc));
    m.sw(at(frame, 0x10), 1);
    let image = m.lw(slot(pictures, picture, 0));
    let result = call(m, DRAW_ICON, [image, 0x3c0, 0x100, 0x300]);
    m.pop_frame(0x20);
    result
}

/// 800D09E8 (screen 0, start): counts `W+0x270` and on the second frame
/// resets the archive block and the load phase, screen 1, flag byte 5 =
/// 1, panel 0 entry 3 starts, lamp 0 = 0xB, lists the received messages
/// (context +0xD98, numbers 0x1A..0xAE), draws the page and the picture,
/// flag byte 0x32 = 1 (0x33/0x34 cleared), lights the first four lamps,
/// hub flag 0x31 = 0 and no arrow highlight. Returns 0x80110000 (the
/// last `lui`), or 0 on the first frame.
pub fn start_d09e8(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let w = work(m);
    let context = context(m);
    let frames = m.lw(at(w, 0x270)).wrapping_add(1);
    m.sw(at(w, 0x270), frames);
    let received = at(context, 0xd98);
    let result = if lt(frames, 2) {
        0
    } else {
        let a = archive(m);
        m.sw(at(w, 0x270), 0);
        m.sh(at(w, 0x1c6), 0);
        m.sb(a, 0);
        for offset in [1, 3, 2] {
            let a = archive(m);
            m.sb(at(a, offset), 0);
        }
        let w = work(m);
        m.sb(at(w, 0x7d), 1);
        set_flag_byte(m, 5, 1);
        call(m, PANEL_START, [0, 3, 0, 0]);
        call(m, SET_LAMP, [0, 0xb, 0, 0]);
        let a = archive(m);
        m.sw(at(a, 0xb8), 0);
        for number in 0x1au32..0xaf {
            let context = self::context(m);
            if m.lbu(at(context, number).wrapping_add(0xd98)) != 0 {
                let a = archive(m);
                let count = m.lw(at(a, 0xb8));
                m.sb(at(a, count).wrapping_add(4), number as u8);
                let a = archive(m);
                let count = m.lw(at(a, 0xb8)).wrapping_add(1);
                m.sw(at(a, 0xb8), count);
            }
        }
        let a = archive(m);
        let page = u32::from(m.lbu(at(a, 2)));
        page_d0830(m, page);
        let a = archive(m);
        let cursor = u32::from(m.lbu(at(a, 3)));
        picture_d0970(m, cursor);
        set_flag_byte(m, 0x32, 1);
        set_flag_byte(m, 0x33, 0);
        set_flag_byte(m, 0x34, 0);
        lamps(m, &LIST, received, false);
        call(m, SET_FLAG, [0x31, 0, 0, 0]);
        m.sw(SCROLL_TIMER, 0);
        0x8011_0000
    };
    m.pop_frame(0x20);
    result
}

/// 800D0C34 (screen 1, the list): lamp 0 = 0xB; the scroll arrows and
/// moves of the list; unless the disc is busy (8002B528 is 1), confirm
/// (pressed 0x10, none of 0x2F held) clears cursors 1..5, sets hub flag
/// 0x30, the replay's message (`W+0x124`), its phase 0 and screen 4;
/// cancel (0x20, none of 0x5F) screen 5 with phase 0. Then flag byte 0x34
/// is the cursor and the lamps show the page. Returns 0.
pub fn list_d0c34(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    // 8002B528 reads no argument register
    let busy = call(m, STREAM_BUSY, [0; 4]);
    call(m, SET_LAMP, [0, 0xb, 0, 0]);
    arrows(m, &LIST);
    scroll(m, &LIST);
    let context = context(m);
    if m.lw(at(context, 0x12c)) & 0x10 != 0 && m.lw(at(context, 0x128)) & 0x2f == 0 && busy != 1 {
        for cursor in 1..=5 {
            call(m, SET_CURSOR, [cursor, 0, 0, 0]);
        }
        call(m, SET_FLAG, [0x30, 1, 0, 0]);
        let w = work(m);
        let a = archive(m);
        m.sh(at(w, 0x86), 0);
        let page = u32::from(m.lbu(at(a, 2)));
        let cursor = u32::from(m.lbu(at(a, 3)));
        let message = m.lbu(at(a, cursor.wrapping_add(page << 2)).wrapping_add(4));
        m.sb(at(w, 0x7d), 4);
        m.sh(at(w, 0x124), u16::from(message));
    }
    let context = self::context(m);
    if m.lw(at(context, 0x12c)) & 0x20 != 0 && m.lw(at(context, 0x128)) & 0x5f == 0 && busy != 1 {
        let w = work(m);
        m.sb(at(w, 0x7d), 5);
        let w = work(m);
        m.sh(at(w, 0x86), 0);
    }
    let context = self::context(m);
    let a = archive(m);
    let flags = m.lw(super::ov2_p1::FLAGS);
    let bytes = m.lw(at(flags, 0x5c));
    let cursor = m.lbu(at(a, 3));
    let received = at(context, 0xd98);
    m.sb(at(bytes, 0x34), cursor);
    lamps(m, &LIST, received, true);
    m.pop_frame(0x20);
    0
}

/// Screens 2, 4 and 5 (800D1214, 800D1258, 800D129C): the phase `W+0x86`
/// of `table` (the argument registers pass through); returns its result.
fn phase(m: &mut Machine, table: u32, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let w = work(m);
    let index = lh(m, at(w, 0x86));
    let handler = m.lw(slot(table, index, 0));
    let result = call(m, handler, args);
    m.pop_frame(0x18);
    result
}

/// 800D1214 (screen 2): the replay's phase of [`REPLAY`].
pub fn replay_d1214(m: &mut Machine, args: [u32; 4]) -> u32 {
    phase(m, REPLAY, args)
}

/// 800D1258 (screen 4): the replay start's phase of [`REPLAY_START`].
pub fn replay_start_d1258(m: &mut Machine, args: [u32; 4]) -> u32 {
    phase(m, REPLAY_START, args)
}

/// 800D129C (screen 5): the leaving phase of [`LEAVE`].
pub fn leave_d129c(m: &mut Machine, args: [u32; 4]) -> u32 {
    phase(m, LEAVE, args)
}

/// 800D12E0 (screen 3, back to the list): once the loader is idle
/// (`W+0x1C6` = 3), flag bytes 0x32 = 1, 0x33 = 0 and 0x34 the cursor,
/// the page and picture redrawn, screen 1, lamp 0 = 0xB, cursor 1 = 1 (0,
/// 0x78), hub flag 0x32 = 1, context +0x95E = 0 and hub flag 1 = the hub
/// selection + 1. Then the load phase `W+0x1C6` with `a0` = 0; returns
/// its `v0` (the idle stub's own address in phase 3).
pub fn back_d12e0(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let w = work(m);
    if lh(m, at(w, 0x1c6)) == 3 {
        set_flag_byte(m, 0x32, 1);
        set_flag_byte(m, 0x33, 0);
        let flags = m.lw(super::ov2_p1::FLAGS);
        let a = archive(m);
        let bytes = m.lw(at(flags, 0x5c));
        let cursor = m.lbu(at(a, 3));
        m.sb(at(bytes, 0x34), cursor);
        let a = archive(m);
        let page = u32::from(m.lbu(at(a, 2)));
        page_d0830(m, page);
        let a = archive(m);
        let cursor = u32::from(m.lbu(at(a, 3)));
        picture_d0970(m, cursor);
        let w = work(m);
        m.sb(at(w, 0x7d), 1);
        call(m, SET_LAMP, [0, 0xb, 0, 0]);
        call(m, SET_CURSOR, [1, 1, 0, 0x78]);
        call(m, SET_FLAG, [0x32, 1, 0, 0]);
        let context = context(m);
        m.sb(at(context, 0x95e), 0);
        let w = work(m);
        let selection = u32::from(m.lbu(at(w, 0x7e)));
        call(m, SET_FLAG, [1, selection.wrapping_add(1), 0, 0]);
    }
    let result = run_load_phase(m);
    m.pop_frame(0x20);
    result
}

/// 800D1418 (a load phase): start list 0x7C; the phase advances
/// (returned).
pub fn start_d1418(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let phase = start_list(m, 0x7c, false);
    m.pop_frame(0x18);
    phase
}

/// 800D14D0 (a load phase): start list 0x7D; the phase advances
/// (returned).
pub fn start_d14d0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let phase = start_list(m, 0x7d, false);
    m.pop_frame(0x18);
    phase
}

/// 800D14A0 and 800D1558 (load phases): once loaded (`context+0x44` not
/// 1), back to load phase 0 and return the work block pointer; while
/// loading return 0x80110000 (the delay slot's `lui`).
fn finish(m: &mut Machine) -> u32 {
    let context = context(m);
    if m.lw(at(context, 0x44)) == 1 {
        return 0x8011_0000;
    }
    let w = work(m);
    m.sh(at(w, 0x1c6), 0);
    w
}

/// 800D14A0: see [`finish`] (list 0x7C).
pub fn finish_d14a0(m: &mut Machine) -> u32 {
    finish(m)
}

/// 800D1558: see [`finish`] (list 0x7D).
pub fn finish_d1558(m: &mut Machine) -> u32 {
    finish(m)
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800d_0830 => page_d0830(m, args[0]),
        0x800d_0970 => picture_d0970(m, args[0]),
        0x800d_09e8 => start_d09e8(m),
        0x800d_0c34 => list_d0c34(m),
        0x800d_1214 => replay_d1214(m, args),
        0x800d_1258 => replay_start_d1258(m, args),
        0x800d_129c => leave_d129c(m, args),
        0x800d_12e0 => back_d12e0(m),
        0x800d_1418 => start_d1418(m),
        0x800d_14a0 => finish_d14a0(m),
        0x800d_14d0 => start_d14d0(m),
        0x800d_1558 => finish_d1558(m),
        _ => return None,
    })
}
