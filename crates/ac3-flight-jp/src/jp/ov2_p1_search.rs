//! JP overlay 2, part 1: the Search Engine (hub screen group 2).
//!
//! - 800D0588 (screen 0, start; the second frame after the hub menu
//!   opened it) allocates the search block `S` ([`SEARCH`], 0x260 bytes)
//!   and lists the entries the player owns (context +0xD48, 0x4F flags);
//!   800D07C0 (screen 1) runs the phase `S+0` of [`PHASES`].
//! - The phases: 0 800CF854 open the list, 1 800CF9A8 the list (cursor
//!   and page, confirm loads the entry's file, cancel leaves), 2 800CFF5C
//!   leave the list (to the entry or back to the hub), 3 800D0034 wait for
//!   the entry and install it, 4 800D013C the entry's pages, 5 800D04F4
//!   back to the list.
//! - Drawing: 800CF504 / 800CF564 / 800CF640 the entry icons (800ED000),
//!   800CF6C0 the entry page's eleven text-line sprites and their draw
//!   mode, 800CF6B0 the page index reset.
//!
//! `S`: +0 phase, +1 leave target (0 entry, 1 hub), +2 list page, +3
//! cursor on the page, +4 entry page, +0x200 this frame's primitive
//! buffer (`S+8+0xFC*[context+4]`), +0x204 the listed entry numbers,
//! +0x254 their count, +0x258 the page step, +0x25C the entry's marks.
//! `[0x80112DD4]` ([`SCROLL_TIMER`]) holds the scroll arrows' highlight.
use super::ov2::MODULE;
use super::ov2_p1::{
    ALLOC, ARENA_RESET, FLAGS, LOAD_DONE, QUEUE_FILES, RELOCATE, SET_FLAG, TOTAL_SIZE, at, call,
    context, lt, slot, sound, tag, work,
};
use super::ov2_p1_pages::{CURSOR, INSTALL_BANK, PANEL_START, SET_CURSOR, SET_LAMP};
use crate::common::{RESOURCES, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112DD0]`: the search block `S`.
pub const SEARCH: u32 = 0x8011_2dd0;
/// `[0x80112DD4]`: frames the scroll arrow highlight still shows.
pub const SCROLL_TIMER: u32 = 0x8011_2dd4;
/// 80109D78: the six search phases by `S+0`.
pub const PHASES: u32 = 0x8010_9d78;
/// 80109998: the file list of each search entry (words).
pub const ENTRY_FILES: u32 = 0x8010_9998;
/// 80109A90: twelve-byte records by entry: +0 the page count, then two
/// mark bytes per page (+4 + 2*page, +5 + 2*page).
pub const ENTRY_PAGES: u32 = 0x8010_9a90;
/// 800ED000: draw an icon (`image`, `x`, `y`, `clut`, stack +0x10 mode).
pub const DRAW_ICON: u32 = 0x800e_d000;
/// 800AC7F8: SetDrawMode(`p`, `dfe`, `dtd`, `tpage`, stack +0x10 `tw`).
pub const SET_DRAW_MODE: u32 = 0x800a_c7f8;

fn search(m: &mut Machine) -> u32 {
    m.lw(SEARCH)
}

/// The hub flag bytes (`[[0x80112DC0]+0x5C]`).
pub(crate) fn flag_bytes(m: &mut Machine) -> u32 {
    let flags = m.lw(FLAGS);
    m.lw(at(flags, 0x5c))
}

/// Flag byte `offset` = `value` (the pointers re-read, as compiled).
pub(crate) fn set_flag_byte(m: &mut Machine, offset: u32, value: u8) {
    let bytes = flag_bytes(m);
    m.sb(at(bytes, offset), value);
}

/// An icon call of 800ED000 with the mode word at the frame's +0x10.
pub(crate) fn icon(m: &mut Machine, frame: u32, args: [u32; 4], mode: u32) -> u32 {
    m.sw(at(frame, 0x10), mode);
    call(m, DRAW_ICON, args)
}

/// A scrolling list of entries, four per page: the Search Engine's (`S`)
/// or the Archive's (`A`, `ov2_p1_archive`).
pub(crate) struct List {
    /// The address of the block pointer.
    pub block: u32,
    /// Block offsets of the entry numbers (bytes) and their count (word).
    pub entries: u32,
    pub count: u32,
    /// The second lamp byte of a listed entry.
    pub mark: u8,
    /// The page's icons and the cursor's picture.
    pub page: fn(&mut Machine, u32) -> u32,
    pub picture: fn(&mut Machine, u32) -> u32,
    /// Page turns (R1/L1) play their sound before redrawing (else after).
    pub sound_first: bool,
}

/// The Search Engine's list.
pub(crate) const LIST: List = List {
    block: SEARCH,
    entries: 0x204,
    count: 0x254,
    mark: 1,
    page: page_cf564,
    picture: picture_cf640,
    sound_first: true,
};

/// The four entry lamps (flag bytes 0x35+i, the entry's state from
/// `owned`, and 0x39+i = the list's mark), for the list entries `first +
/// i` (with `paged`, `first` is the list page times 4, re-read each time);
/// past the list end both are 0.
pub(crate) fn lamps(m: &mut Machine, list: &List, owned: u32, paged: bool) {
    for i in 0..4u32 {
        let s = m.lw(list.block);
        let index = if paged {
            i.wrapping_add(u32::from(m.lbu(at(s, 2))) << 2)
        } else {
            i
        };
        let count = m.lw(at(s, list.count));
        if lt(index, count) {
            let entry = u32::from(m.lbu(at(s, index).wrapping_add(list.entries)));
            let flags = m.lw(FLAGS);
            let bytes = m.lw(at(flags, 0x5c));
            let state = m.lbu(at(owned, entry));
            m.sb(at(bytes, i).wrapping_add(0x35), state);
            set_flag_byte(m, i.wrapping_add(0x39), list.mark);
        } else {
            set_flag_byte(m, i.wrapping_add(0x35), 0);
            set_flag_byte(m, i.wrapping_add(0x39), 0);
        }
    }
}

/// The list's scroll arrows: without a highlight, hub flag 0x31 shows
/// them (bit 1 more pages, bit 0 not the first) and flag byte 0x33
/// clears; otherwise the highlight counts down.
pub(crate) fn arrows(m: &mut Machine, list: &List) {
    let timer = m.lw(SCROLL_TIMER);
    if timer != 0 {
        m.sw(SCROLL_TIMER, timer.wrapping_sub(1));
        return;
    }
    let s = m.lw(list.block);
    let count = m.lw(at(s, list.count));
    let page = u32::from(m.lbu(at(s, 2)));
    let last = (count.wrapping_sub(1) as i32 / 4) as u32;
    let mut bits = if lt(page, last) { 2 } else { 0 };
    if page != 0 {
        bits |= 1;
    }
    call(m, SET_FLAG, [0x31, bits, 0, 0]);
    set_flag_byte(m, 0x33, 0);
}

/// The list's moves (pressed bits of `context+0x130`): down (0x2) and up
/// (0x1) move the cursor across pages, R1 (0x200) and L1 (0x100) turn
/// pages (the cursor kept in the list); each move plays sound 0x3D and
/// redraws, a page change also highlights its arrow.
pub(crate) fn scroll(m: &mut Machine, list: &List) {
    // down: the next entry, across to the next page
    let context = context(m);
    if m.lw(at(context, 0x130)) & 2 != 0 {
        let s = m.lw(list.block);
        let page = u32::from(m.lbu(at(s, 2)));
        let cursor = m.lbu(at(s, 3));
        let count = m.lw(at(s, list.count));
        let index = u32::from(cursor).wrapping_add(page << 2);
        if lt(index, count.wrapping_sub(1)) {
            if cursor < 3 {
                m.sb(at(s, 3), cursor + 1);
            } else {
                m.sb(at(s, 3), 0);
                let s = m.lw(list.block);
                let page = m.lbu(at(s, 2)).wrapping_add(1);
                m.sb(at(s, 2), page);
                let s = m.lw(list.block);
                let page = u32::from(m.lbu(at(s, 2)));
                (list.page)(m, page);
                highlight(m, 2);
            }
            sound(m, 0x3d);
            let s = m.lw(list.block);
            let cursor = u32::from(m.lbu(at(s, 3)));
            (list.picture)(m, cursor);
        }
    }
    // up: the previous entry, across to the previous page
    let context = self::context(m);
    if m.lw(at(context, 0x130)) & 1 != 0 {
        let s = m.lw(list.block);
        let cursor = m.lbu(at(s, 3));
        let moved = if cursor != 0 {
            m.sb(at(s, 3), cursor - 1);
            true
        } else if m.lbu(at(s, 2)) != 0 {
            m.sb(at(s, 3), 3);
            let s = m.lw(list.block);
            let page = m.lbu(at(s, 2)).wrapping_sub(1);
            m.sb(at(s, 2), page);
            let s = m.lw(list.block);
            let page = u32::from(m.lbu(at(s, 2)));
            (list.page)(m, page);
            highlight(m, 1);
            true
        } else {
            false
        };
        if moved {
            sound(m, 0x3d);
            let s = m.lw(list.block);
            let cursor = u32::from(m.lbu(at(s, 3)));
            (list.picture)(m, cursor);
        }
    }
    // R1: the next page, the cursor clamped to the list
    let context = self::context(m);
    if m.lw(at(context, 0x130)) & 0x200 != 0 {
        let s = m.lw(list.block);
        let count = m.lw(at(s, list.count));
        let page = m.lbu(at(s, 2));
        let last = (count.wrapping_sub(1) as i32 / 4) as u32;
        if lt(u32::from(page), last) {
            m.sb(at(s, 2), page.wrapping_add(1));
            let s = m.lw(list.block);
            let page = u32::from(m.lbu(at(s, 2)));
            let cursor = u32::from(m.lbu(at(s, 3)));
            let count = m.lw(at(s, list.count));
            if !lt(cursor.wrapping_add(page << 2), count) {
                let last = count.wrapping_sub(1);
                let pages = (last as i32 / 4) as u32;
                m.sb(at(s, 3), last.wrapping_sub(pages << 2) as u8);
            }
            turn(m, list, 2);
        }
    }
    // L1: the previous page
    let context = self::context(m);
    if m.lw(at(context, 0x130)) & 0x100 != 0 {
        let s = m.lw(list.block);
        let page = m.lbu(at(s, 2));
        if page != 0 {
            m.sb(at(s, 2), page - 1);
            turn(m, list, 1);
        }
    }
}

/// After a page turn: sound 0x3D (before or after the redraw, by the
/// list), the page's icons and the cursor's picture, the arrow highlight.
fn turn(m: &mut Machine, list: &List, arrow: u8) {
    if list.sound_first {
        sound(m, 0x3d);
    }
    let s = m.lw(list.block);
    let page = u32::from(m.lbu(at(s, 2)));
    (list.page)(m, page);
    let s = m.lw(list.block);
    let cursor = u32::from(m.lbu(at(s, 3)));
    (list.picture)(m, cursor);
    if !list.sound_first {
        sound(m, 0x3d);
    }
    highlight(m, arrow);
}

/// 800CF504: the icon of list entry `cursor` on page `page` (picture
/// `[W+0x1D4]`, at (0x3C0, 0x154) with clut 0x180, mode 6). Returns
/// 800ED000's result.
pub fn icon_cf504(m: &mut Machine, page: u32, cursor: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let index = cursor.wrapping_add(page << 2);
    let s = search(m);
    let w = work(m);
    let entry = u32::from(m.lbu(at(s, index).wrapping_add(0x204)));
    let images = m.lw(at(w, 0x1d4));
    let image = m.lw(slot(images, entry, 0));
    let result = icon(m, frame, [image, 0x3c0, 0x154, 0x180], 6);
    m.pop_frame(0x20);
    result
}

/// 800CF564: the icons of list page `page` (four per page, the last page
/// the remainder), one row of 0x12 lines each from y 0x154. Returns 0, or
/// the page's first index when it is empty.
pub fn page_cf564(m: &mut Machine, page: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let s = search(m);
    let count = m.lw(at(s, 0x254));
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
        let s = search(m);
        let index = row.wrapping_add(page << 2);
        if lt(index, m.lw(at(s, 0x254))) {
            let w = work(m);
            let entry = u32::from(m.lbu(at(s, index).wrapping_add(0x204)));
            let images = m.lw(at(w, 0x1d4));
            let image = m.lw(slot(images, entry, 0));
            icon(m, frame, [image, 0x3c0, y, 0x180], 6);
        }
        row = row.wrapping_add(1);
        y = y.wrapping_add(0x12);
        result = 0;
    }
    m.pop_frame(0x30);
    result
}

/// 800CF640: the large picture of list entry `cursor` of the current page
/// (`[W+0x1DC]`, at (0x3C0, 0x100) with clut 0x300, mode 1). Past the list
/// end nothing is drawn and the result is the page's first index; else
/// 800ED000's result.
pub fn picture_cf640(m: &mut Machine, cursor: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let s = search(m);
    let first = u32::from(m.lbu(at(s, 2))) << 2;
    let count = m.lw(at(s, 0x254));
    let index = cursor.wrapping_add(first);
    let result = if lt(index, count) {
        let w = work(m);
        let entry = u32::from(m.lbu(at(s, index).wrapping_add(0x204)));
        let images = m.lw(at(w, 0x1dc));
        let image = m.lw(slot(images, entry, 0));
        icon(m, frame, [image, 0x3c0, 0x100, 0x300], 1)
    } else {
        first
    };
    m.pop_frame(0x20);
    result
}

/// 800CF6B0: entry page `S+4` = 0. Returns `S`.
pub fn reset_page_cf6b0(m: &mut Machine) -> u32 {
    let s = search(m);
    m.sb(at(s, 4), 0);
    s
}

/// 800CF6C0: the entry page: eleven sprites (code 0x64, 0x90 x 0x12 at x
/// 0x65 from y 0x78 every 0x18, texture rows 0, 0x12, ... and clut
/// `(0xD2 + page) << 6 | 0x3F`) in `S+0x200`, then the draw mode of the
/// page's texture page at +0xF0, each linked into the first slot of the
/// frame's table `[W+8]`. Returns the last tag word written.
pub fn page_lines_cf6c0(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x28);
    let (mut y, mut v, mut offset) = (0x78u32, 0u32, 0u32);
    for _ in 0..0xb {
        let s = search(m);
        let base = m.lw(at(s, 0x200));
        let prim = at(base, offset);
        m.sb(at(prim, 3), 4);
        m.sb(at(prim, 7), 0x64);
        m.sh(at(prim, 8), 0x65);
        m.sh(at(prim, 0x10), 0x90);
        m.sh(at(prim, 0xa), y as u16);
        y = y.wrapping_add(0x18);
        m.sb(at(prim, 4), 0x80);
        m.sb(at(prim, 5), 0x80);
        m.sb(at(prim, 6), 0x80);
        m.sb(at(prim, 0xc), 0);
        m.sb(at(prim, 0xd), v as u8);
        let s = search(m);
        v = v.wrapping_add(0x12);
        m.sh(at(prim, 0x12), 0x12);
        let page = u32::from(m.lbu(at(s, 4)));
        let w = work(m);
        let clut = (page.wrapping_add(0xd2) << 6) | 0x3f;
        m.sh(at(prim, 0xe), clut as u16);
        link_first(m, w, prim);
        offset = offset.wrapping_add(0x14);
    }
    let s = search(m);
    let base = m.lw(at(s, 0x200));
    let page = u32::from(m.lbu(at(s, 4)));
    m.sw(at(frame, 0x10), 0);
    let mode = at(base, 0xf0);
    let tpage = ((((page >> 1) << 6).wrapping_add(0x280) & 0x3ff) >> 6) | 0x10;
    call(m, SET_DRAW_MODE, [mode, 0, 0, tpage]);
    let w = work(m);
    let result = link_first(m, w, mode);
    m.pop_frame(0x28);
    result
}

/// `addPrim(table[0], prim)` into the frame's table `[W+8]` (the table
/// pointer re-read); returns the slot's new tag.
fn link_first(m: &mut Machine, w: u32, prim: u32) -> u32 {
    let table = m.lw(at(w, 8));
    let head = m.lw(prim);
    let first = m.lw(table);
    m.sw(prim, tag(head, first));
    let table = m.lw(at(w, 8));
    let first = m.lw(table);
    let value = tag(first, prim);
    m.sw(table, value);
    value
}

/// 800CF854 (phase 0): open the list: phase 1, the page's icons, flag
/// bytes 2 and 0x32..0x34 cleared, hub flag 0x31 = 0, no arrow
/// highlight, the lamps of the first four entries. Returns 0.
pub fn open_cf854(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let context = context(m);
    let s = search(m);
    m.sb(s, 1);
    let s = search(m);
    let page = u32::from(m.lbu(at(s, 2)));
    let owned = at(context, 0xd48);
    page_cf564(m, page);
    for offset in [2, 0x32, 0x33, 0x34] {
        set_flag_byte(m, offset, 0);
    }
    call(m, SET_FLAG, [0x31, 0, 0, 0]);
    m.sw(SCROLL_TIMER, 0);
    lamps(m, &LIST, owned, false);
    m.pop_frame(0x20);
    0
}

/// After a scroll: flag byte 0x33 = `arrow`, the arrow highlight for 12
/// frames.
pub(crate) fn highlight(m: &mut Machine, arrow: u8) {
    set_flag_byte(m, 0x33, arrow);
    m.sw(SCROLL_TIMER, 0xc);
}

/// 800CF9A8 (phase 1): the list. Without an arrow highlight, hub flag
/// 0x31 shows the scroll arrows (bit 1 more pages, bit 0 not the first)
/// and flag byte 0x33 clears; otherwise the highlight counts down. Down
/// (0x2) and up (0x1) move the cursor across pages, R1 (0x200) and L1
/// (0x100) turn pages (the cursor kept in the list); each move plays
/// sound 0x3D and redraws. Confirm (pressed 0x10, none of 0x2F held)
/// plays 0x3C, resets the entry page, sets phase 2 (to the entry) and
/// queues the entry's file ([`ENTRY_FILES`]); cancel (0x20, none of 0x5F)
/// plays 0x3B and sets phase 2 back to the hub. Then flag byte 0x34 is
/// the cursor and the lamps show the page. Returns 0.
pub fn list_cf9a8(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    arrows(m, &LIST);
    scroll(m, &LIST);
    // confirm: load the entry
    let context = self::context(m);
    if m.lw(at(context, 0x12c)) & 0x10 != 0 && m.lw(at(context, 0x128)) & 0x2f == 0 {
        sound(m, 0x3c);
        reset_page_cf6b0(m);
        let s = search(m);
        m.sb(s, 2);
        let s = search(m);
        m.sb(at(s, 1), 0);
        let s = search(m);
        let page = u32::from(m.lbu(at(s, 2)));
        let cursor = u32::from(m.lbu(at(s, 3)));
        let index = cursor.wrapping_add(page << 2);
        let entry = u32::from(m.lbu(at(s, index).wrapping_add(0x204)));
        let resources = m.gw(RESOURCES);
        let id = m.lw(slot(ENTRY_FILES, entry, 0));
        m.sw(at(resources, 0x60), id);
        let bytes = call(m, TOTAL_SIZE, [at(resources, 0x60), 1, 0, 0]);
        let buffer = call(m, ALLOC, [bytes, 0, 0, 0]);
        let resources = m.gw(RESOURCES);
        let context = self::context(m);
        m.sw(at(context, 0x44), 1);
        call(m, QUEUE_FILES, [at(resources, 0x60), 1, buffer, LOAD_DONE]);
    }
    // cancel: back to the hub
    let context = self::context(m);
    if m.lw(at(context, 0x12c)) & 0x20 != 0 && m.lw(at(context, 0x128)) & 0x5f == 0 {
        sound(m, 0x3b);
        let s = search(m);
        m.sb(s, 2);
        let s = search(m);
        m.sb(at(s, 1), 1);
    }
    let context = self::context(m);
    let s = search(m);
    let bytes = flag_bytes(m);
    let cursor = m.lbu(at(s, 3));
    let owned = at(context, 0xd48);
    m.sb(at(bytes, 0x34), cursor);
    lamps(m, &LIST, owned, true);
    m.pop_frame(0x20);
    0
}

/// 800CFF5C (phase 2): to the entry (`S+1` = 0): phase 3, panel 0 entry
/// 4 starts, cursor 0xB = 1 (0, 0x19), lamp 0 = 0x4011, the entry's icon,
/// flag byte 3 cleared (returns the flag bytes' address). Back to the hub
/// (1): lamp 0 = 3, screen group and screen 0, load phase 0 and the arena
/// reset to the page buffer (returns 80027538's result). Otherwise 1.
pub fn leave_cff5c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = search(m);
    let target = m.lbu(at(s, 1));
    let result = if target == 0 {
        m.sb(s, 3);
        call(m, PANEL_START, [0, 4, 0, 0]);
        call(m, SET_CURSOR, [0xb, 1, 0, 0x19]);
        call(m, SET_LAMP, [0, 0x4011, 0, 0]);
        let s = search(m);
        let page = u32::from(m.lbu(at(s, 2)));
        let cursor = u32::from(m.lbu(at(s, 3)));
        icon_cf504(m, page, cursor);
        let bytes = flag_bytes(m);
        m.sb(at(bytes, 3), 0);
        bytes
    } else if target == 1 {
        call(m, SET_LAMP, [0, 3, 0, 0]);
        let w = work(m);
        m.sb(at(w, 0x7c), 0);
        let w = work(m);
        m.sb(at(w, 0x7d), 0);
        let w = work(m);
        let buffer = m.lw(at(w, 0x108));
        m.sh(at(w, 0x1c6), 0);
        call(m, ARENA_RESET, [buffer, 0, 0, 0])
    } else {
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800D0034 (phase 3): mark the entry read (context +0xD48 = 2), hub flag
/// 0x31 = 0, `S+0x25C` = 0; once the file is loaded (`context+0x44` clear)
/// install its bank and release it, `S+0x258` = 0, flag bytes 1 and 2
/// cleared, cursors 3 and 4 = 0 and phase 4 (returns 4); while loading
/// returns 0x800C0000 (the delay slot's `lui`).
pub fn install_d0034(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let s = search(m);
    let page = u32::from(m.lbu(at(s, 2)));
    let cursor = u32::from(m.lbu(at(s, 3)));
    let index = (page << 2).wrapping_add(cursor);
    let context = context(m);
    let entry = u32::from(m.lbu(at(s, index).wrapping_add(0x204)));
    m.sb(at(context, entry).wrapping_add(0xd48), 2);
    call(m, SET_FLAG, [0x31, 0, 0, 0]);
    let s = search(m);
    let context = self::context(m);
    m.sw(at(s, 0x25c), 0);
    let result = if m.lw(at(context, 0x44)) != 0 {
        0x800c_0000
    } else {
        let resources = m.gw(RESOURCES);
        let raw = m.lw(at(resources, 0x60));
        let block = call(m, RELOCATE, [raw, 0, 0, 0]);
        let bank = m.lw(block);
        call(m, INSTALL_BANK, [bank, 0, 0, 0]);
        let bank = m.lw(block);
        call(m, ARENA_RESET, [bank, 0, 0, 0]);
        let s = search(m);
        m.sw(at(s, 0x258), 0);
        set_flag_byte(m, 1, 0);
        set_flag_byte(m, 2, 0);
        call(m, SET_CURSOR, [3, 0, 0, 0]);
        call(m, SET_CURSOR, [4, 0, 0, 0]);
        let s = search(m);
        m.sb(s, 4);
        4
    };
    m.pop_frame(0x20);
    result
}

/// 800D013C (phase 4): the entry's pages ([`ENTRY_PAGES`]). Hub flag
/// 0x31 shows the page arrows and flag byte 0x33 clears; an entry with
/// pages is drawn (800CF6C0), down/up (pressed 0x2/0x1) turn its page
/// (sound 0x3D, `S+0x258` the step, flag byte 0x33 the arrow), cursor 5
/// starts once, and the page's two marks (flag bytes 1 and 2) start
/// cursors 3 and 4 and set `S+0x25C`, shown in flag byte 3; an entry
/// without pages shows 3. Cancel (0x20, none of 0x5F) plays 0x3B and
/// sets phase 5 (returns 5); otherwise returns the last test's value.
pub fn pages_d013c(m: &mut Machine) -> u32 {
    m.push_frame(0x28);
    let s = search(m);
    let page = u32::from(m.lbu(at(s, 2)));
    let cursor = u32::from(m.lbu(at(s, 3)));
    let index = (page << 2).wrapping_add(cursor);
    let entry = u32::from(m.lbu(at(s, index).wrapping_add(0x204)));
    let record = ENTRY_PAGES.wrapping_add(entry.wrapping_mul(12));
    let bytes = flag_bytes(m);
    let pages = m.lw(record);
    m.sb(at(bytes, 0x33), 0);
    let s = search(m);
    let shown = u32::from(m.lbu(at(s, 4)));
    let last = pages.wrapping_sub(1);
    let mut arrows = u32::from(lt(shown, last)) << 1;
    if shown != 0 {
        arrows |= 1;
    }
    call(m, SET_FLAG, [0x31, arrows, 0, 0]);
    if m.lw(record) == 0 {
        set_flag_byte(m, 3, 3);
    } else {
        page_lines_cf6c0(m);
        let context = context(m);
        if m.lw(at(context, 0x12c)) & 2 != 0 {
            let s = search(m);
            if lt(u32::from(m.lbu(at(s, 4))), last) {
                let bytes = flag_bytes(m);
                m.sw(at(s, 0x258), 1);
                m.sb(at(bytes, 0x33), 2);
                sound(m, 0x3d);
                let s = search(m);
                let shown = m.lbu(at(s, 4)).wrapping_add(1);
                m.sb(at(s, 4), shown);
                set_flag_byte(m, 0x33, 2);
            }
        }
        let context = self::context(m);
        if m.lw(at(context, 0x12c)) & 1 != 0 {
            let s = search(m);
            let shown = m.lbu(at(s, 4));
            if shown != 0 {
                let bytes = flag_bytes(m);
                m.sw(at(s, 0x258), 2);
                m.sb(at(bytes, 0x33), 1);
                sound(m, 0x3d);
                let s = search(m);
                let shown = m.lbu(at(s, 4)).wrapping_sub(1);
                m.sb(at(s, 4), shown);
                set_flag_byte(m, 0x33, 1);
            }
        }
        if call(m, CURSOR, [5, 0, 0, 0]) == 0 {
            call(m, SET_CURSOR, [5, 1, 0, 0xa]);
        }
        for (mark, cursor, value) in [(0u32, 3u32, 1u32), (1, 4, 2)] {
            let base = record.wrapping_add(mark);
            let s = search(m);
            let bytes = flag_bytes(m);
            let shown = u32::from(m.lbu(at(s, 4)));
            let value_at = (shown << 1).wrapping_add(base).wrapping_add(4);
            let marked = m.lbu(value_at);
            m.sb(at(bytes, mark + 1), marked);
            let s = search(m);
            let shown = u32::from(m.lbu(at(s, 4)));
            let marked = m.lbu((shown << 1).wrapping_add(base).wrapping_add(4));
            if marked != 0 && call(m, CURSOR, [cursor, 0, 0, 0]) == 0 {
                call(m, SET_CURSOR, [cursor, 1, 0, 0xa]);
                let s = search(m);
                m.sw(at(s, 0x25c), value);
            }
        }
        let flags = m.lw(FLAGS);
        let s = search(m);
        let bytes = m.lw(at(flags, 0x5c));
        let marks = m.lbu(at(s, 0x25c));
        m.sb(at(bytes, 3), marks);
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
            sound(m, 0x3b);
            let s = search(m);
            m.sb(s, 5);
            5
        }
    };
    m.pop_frame(0x28);
    result
}

/// 800D04F4 (phase 5): back to the list: phase 0, panel 0 entry 3
/// starts, lamp 0 = 0xB, cursors 5, 3 and 4 = 0, hub flag 1 = the hub
/// selection + 1. Returns 800DEF94's result.
pub fn back_d04f4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let s = search(m);
    m.sb(s, 0);
    call(m, PANEL_START, [0, 3, 0, 0]);
    call(m, SET_LAMP, [0, 0xb, 0, 0]);
    call(m, SET_CURSOR, [5, 0, 0, 0]);
    call(m, SET_CURSOR, [3, 0, 0, 0]);
    call(m, SET_CURSOR, [4, 0, 0, 0]);
    let w = work(m);
    let selection = u32::from(m.lbu(at(w, 0x7e)));
    let result = call(m, SET_FLAG, [1, selection.wrapping_add(1), 0, 0]);
    m.pop_frame(0x18);
    result
}

/// 800D0588 (screen 0, start): counts `W+0x270` and on the second frame
/// allocates the search block (phases and pages 0), screen 1, flag byte 5
/// cleared, panel 0 entry 3 starts, lamp 0 = 0xB, lists the owned entries
/// (context +0xD48, 0x4F), draws the first page and picture, clears flag
/// bytes 0x32..0x34 and lights the first four lamps. Returns 0, or the
/// context pointer on the first frame.
pub fn start_d0588(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let w = work(m);
    let context = context(m);
    let frames = m.lw(at(w, 0x270)).wrapping_add(1);
    m.sw(at(w, 0x270), frames);
    let owned = at(context, 0xd48);
    let result = if lt(frames, 2) {
        context
    } else {
        m.sw(at(w, 0x270), 0);
        let block = call(m, ALLOC, [0x260, 0, 0, 0]);
        m.sw(SEARCH, block);
        m.sb(block, 0);
        for offset in [1, 3, 2] {
            let s = search(m);
            m.sb(at(s, offset), 0);
        }
        let w = work(m);
        m.sb(at(w, 0x7d), 1);
        set_flag_byte(m, 5, 0);
        call(m, PANEL_START, [0, 3, 0, 0]);
        call(m, SET_LAMP, [0, 0xb, 0, 0]);
        let s = search(m);
        m.sw(at(s, 0x254), 0);
        for i in 0..0x4fu32 {
            let s = search(m);
            m.sb(at(s, i).wrapping_add(0x204), 0);
            if m.lbu(at(owned, i)) != 0 {
                let s = search(m);
                let count = m.lw(at(s, 0x254));
                m.sb(at(s, count).wrapping_add(0x204), i as u8);
                let s = search(m);
                let count = m.lw(at(s, 0x254)).wrapping_add(1);
                m.sw(at(s, 0x254), count);
            }
        }
        let s = search(m);
        let page = u32::from(m.lbu(at(s, 2)));
        page_cf564(m, page);
        let s = search(m);
        let cursor = u32::from(m.lbu(at(s, 3)));
        picture_cf640(m, cursor);
        for offset in [0x32, 0x33, 0x34] {
            set_flag_byte(m, offset, 0);
        }
        lamps(m, &LIST, owned, false);
        0
    };
    m.pop_frame(0x20);
    result
}

/// 800D07C0 (screen 1, tick): `S+0x200` = this frame's primitive buffer
/// (`S+8+0xFC*[context+4]`), then the phase `S+0` of [`PHASES`] with
/// `a0` = `S` (the other argument registers pass through). Returns the
/// phase's result.
pub fn tick_d07c0(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let context = context(m);
    let index = m.lw(at(context, 4));
    let s = search(m);
    let buffer = at(s, index.wrapping_mul(0xfc)).wrapping_add(8);
    m.sw(at(s, 0x200), buffer);
    let phase = u32::from(m.lbu(s));
    let handler = m.lw(slot(PHASES, phase, 0));
    let result = call(m, handler, [s, args[1], args[2], args[3]]);
    m.pop_frame(0x18);
    result
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800c_f504 => icon_cf504(m, args[0], args[1]),
        0x800c_f564 => page_cf564(m, args[0]),
        0x800c_f640 => picture_cf640(m, args[0]),
        0x800c_f6b0 => reset_page_cf6b0(m),
        0x800c_f6c0 => page_lines_cf6c0(m),
        0x800c_f854 => open_cf854(m),
        0x800c_f9a8 => list_cf9a8(m),
        0x800c_ff5c => leave_cff5c(m),
        0x800d_0034 => install_d0034(m),
        0x800d_013c => pages_d013c(m),
        0x800d_04f4 => back_d04f4(m),
        0x800d_0588 => start_d0588(m),
        0x800d_07c0 => tick_d07c0(m, args),
        _ => return None,
    })
}
