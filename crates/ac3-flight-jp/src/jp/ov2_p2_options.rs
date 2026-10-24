//! JP overlay 2, part 2: the hub's options menu page, translated from the
//! JP instructions.
//!
//! State record fields ([`STATE`](super::ov2_p2_hub::STATE)): +0xC the
//! selection (0..3), +0xE the level (0..2 in the menu, 3 inside an
//! entry), +0x1C/+0x1D toggles, +0x1E/+0x1F targets, +0x20 a word whose
//! upper half blocks confirming, +0x21..+0x23 sub-page bytes, +0xB4/+0xB5
//! flags, +0xB8 + 4 * selection item tables (16-byte items {fade slot,
//! three words}) and +0xC8 + 4 * selection index tables. Item counts per
//! selection are bytes at [`ITEM_COUNTS`]; the selection-3 extra items are
//! at [`EXTRA_ITEMS`] (20 bytes each). Pad words are context +0x124
//! (option), +0x128 (held), +0x12C (pressed), +0x130 (repeated).
//!
//! The routines of other parts called here take no arguments (by their
//! instructions), apart from the item drawers 800F6FB0/800F711C (a0, a2)
//! and the sound routine (a0).
use super::ov2_p2::{MODULE, framed, lbu, less, lh, lhu, work};
use super::ov2_p2_hub::STATE;
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch;
use crate::machine::{Machine, SmallData};

/// Item counts per selection (bytes).
pub const ITEM_COUNTS: u32 = 0x8010_ac5c;
/// The three extra items of selection 3 (20 bytes each).
pub const EXTRA_ITEMS: u32 = 0x8010_ac20;

/// 8002DB4C (resident, `script`): start sound effect `a0`.
const SOUND: u32 = 0x8002_db4c;
/// Option sub-page handlers and refreshes (overlay 2, part 1; no
/// arguments).
const REFRESH: [u32; 4] = [0x800d_180c, 0x800d_18e4, 0x800d_19a8, 0x800d_1a3c];
const LEAVE_ENTRY: u32 = 0x800d_1f54;
const LEVEL_1: u32 = 0x800d_5bd4;
const PAGES_A: [u32; 3] = [0x800d_7750, 0x800d_7194, 0x800d_6b3c];
const PAGES_B: [u32; 3] = [0x800d_65bc, 0x800d_60fc, 0x800d_5d64];
/// Item drawers (overlay 2, part 5): (item, packet, ordering table + 4).
const DRAW_ITEM: u32 = 0x800f_6fb0;
const DRAW_EXTRA: u32 = 0x800f_711c;

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}
fn flags(m: &mut Machine) -> u32 {
    let block = work(m);
    m.lw(block.wrapping_add(0x5c))
}
fn sound(m: &mut Machine, id: u32) {
    dispatch::call(m, SOUND, [id, 0, 0, 0]);
}
fn call(m: &mut Machine, routine: u32) {
    dispatch::call(m, routine, [0; 4]);
}
fn level(m: &mut Machine) -> u32 {
    let record = state(m);
    lh(m, record.wrapping_add(0xe))
}
fn selection(m: &mut Machine) -> u32 {
    let record = state(m);
    lh(m, record.wrapping_add(0xc))
}
/// Pad word `+offset` of the context.
fn pad(m: &mut Machine, offset: u32) -> u32 {
    let context = m.gw(CONTEXT);
    m.lw(context.wrapping_add(offset))
}
/// A press of `bit` with none of `held` held.
fn press(m: &mut Machine, bit: u32, held: u32) -> bool {
    let context = m.gw(CONTEXT);
    m.lw(context.wrapping_add(0x12c)) & bit != 0 && m.lw(context.wrapping_add(0x128)) & held == 0
}
/// Flag byte `index` = `value` (the work block's flags re-read).
fn put_flag(m: &mut Machine, index: u32, value: u32) {
    let flags = flags(m);
    m.sb(flags.wrapping_add(index), value as u8);
}

/// 800D7DB0: one frame of the options menu: confirm (pad 0x40) opens the
/// selection's entry, the repeated pad bits 2 / 1 / 8 / 4 move the level
/// and the toggles, confirm (0x10) at level 2 runs the selection's
/// refresh or leaves the entry, cancel (0x20) leaves the page; inside an
/// entry (level 3) the sub-page handlers run and flag 7 shows the entry's
/// help text; then the visible items are drawn and the page flags +1..+7,
/// +0x15, +0x3B set. Returns the flag table when the level is below 3,
/// else 0.
pub fn options_menu(m: &mut Machine) -> u32 {
    framed(m, 0x58, |m, sp| {
        let context = m.gw(CONTEXT);
        let record = state(m);
        let pressed = m.lw(context.wrapping_add(0x12c));
        let selected = lh(m, record.wrapping_add(0xc));
        if pressed & 0x40 != 0 && m.lw(context.wrapping_add(0x128)) & 0x2f == 0 {
            confirm(m, record);
        }
        if less(level(m), 3) {
            menu(m, selected);
        } else {
            entry(m);
        }
        leave_or_draw(m, sp);
        set_page_flags(m)
    })
}

/// Confirm (pad 0x40) at levels other than 2 (or with toggle +0x1D = 1)
/// unless word +0x20 has its upper half set: sound 0x3C, clear the press
/// and the targets, level 2, and for selections 0 and 3 cell 9 / 0xB of
/// list 2 at the origin; flag 8 = 0.
fn confirm(m: &mut Machine, record: u32) {
    if lh(m, record.wrapping_add(0xe)) == 2 && lbu(m, record.wrapping_add(0x1d)) != 1 {
        return;
    }
    if m.lw(record.wrapping_add(0x20)) & 0xffff_0000 != 0 {
        return;
    }
    sound(m, 0x3c);
    let context = m.gw(CONTEXT);
    let record = state(m);
    m.sw(context.wrapping_add(0x12c), 0);
    m.sb(record.wrapping_add(0x1e), 0);
    let current = state(m);
    m.sh(record.wrapping_add(0xe), 2);
    m.sb(current.wrapping_add(0x1f), 0);
    let current = state(m);
    m.sb(current.wrapping_add(0x1d), 0);
    let cell = match selection(m) {
        0 => Some(9),
        3 => Some(0xb),
        _ => None,
    };
    if let Some(cell) = cell {
        super::ov2_p2::place_cell(m, 2, cell, 0, 0);
    }
    put_flag(m, 8, 0);
}

/// Levels 0..2: the repeated pad bits 2 (down a level, entering at 3) and
/// 1 (up a level), then confirm (0x10) at level 2.
fn menu(m: &mut Machine, selected: u32) {
    if pad(m, 0x130) & 2 != 0 {
        sound(m, 0x3d);
        let record = state(m);
        let level = lhu(m, record.wrapping_add(0xe)).wrapping_add(1);
        m.sh(record.wrapping_add(0xe), level as u16);
        if super::ov2_p2::sext16(level) == 3 {
            if lbu(m, record.wrapping_add(0x1c)) != 0 {
                m.sb(record.wrapping_add(0x1f), 1);
            } else if lh(m, record.wrapping_add(0xc)) == 1 {
                m.sb(record.wrapping_add(0x1e), 4);
            } else {
                m.sb(record.wrapping_add(0x1e), 1);
            }
        }
    }
    if pad(m, 0x130) & 1 != 0 {
        sound(m, 0x3d);
        let record = state(m);
        let level = lh(m, record.wrapping_add(0xe));
        let raw = lhu(m, record.wrapping_add(0xe));
        if (level as i32) > 0 {
            m.sh(record.wrapping_add(0xe), raw.wrapping_sub(1) as u16);
        } else {
            let toggle = lbu(m, record.wrapping_add(0x1c));
            m.sh(record.wrapping_add(0xe), 3);
            if toggle == 0 {
                m.sb(record.wrapping_add(0x1e), 4);
            } else if toggle == 1 {
                let selection = lh(m, record.wrapping_add(0xc));
                let target = lbu(m, selection.wrapping_add(ITEM_COUNTS));
                m.sb(record.wrapping_add(0x1f), target as u8);
                if selection_of(m) == 3 {
                    super::ov2_p2::set_fade(m, 5, 3, u32::MAX, u32::MAX);
                    super::ov2_p2::set_fade(m, 7, 1, 0, 0xa);
                }
            }
        }
    }
    let record = state(m);
    let level = lh(m, record.wrapping_add(0xe));
    if level == 2 && lbu(m, record.wrapping_add(0x1d)) == 1 {
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x12c)) & 0x10 == 0 {
            return;
        }
        let held = m.lw(context.wrapping_add(0x128));
        if held & 0x2f == 0 && held & 0x40 == 0 {
            sound(m, 0x3c);
            for (selection, routine) in REFRESH.into_iter().enumerate() {
                if selection_of(m) == selection as u32 {
                    call(m, routine);
                }
            }
            return;
        }
    }
    if !press(m, 0x10, 0x2f) {
        return;
    }
    if level_of(m) != 2 {
        return;
    }
    sound(m, 0x3c);
    call(m, LEAVE_ENTRY);
    if selected == 0 || selected == 2 {
        super::ov2_p2::set_cell_preset(m, 0, 1);
        let record = state(m);
        m.sb(record, 0);
    }
    if selected == 1 || selected == 3 {
        super::ov2_p2::set_word_178(m, 2, 8);
        let record = state(m);
        m.sb(record, 2);
    }
    let record = state(m);
    m.sb(record.wrapping_add(1), 0);
    let record = state(m);
    m.sw(record.wrapping_add(0x10), 0);
}

fn selection_of(m: &mut Machine) -> u32 {
    selection(m)
}
fn level_of(m: &mut Machine) -> u32 {
    level(m)
}

/// Level 3: the sub-page handler of +0x22 (toggle +0x1C off) or +0x23
/// (on); with the toggle on, flag 7 = the entry's help text index.
fn entry(m: &mut Machine) {
    let record = state(m);
    if lbu(m, record.wrapping_add(0x1c)) == 0 {
        let page = lbu(m, record.wrapping_add(0x22));
        if let Some(&routine) = PAGES_A.get(page as usize) {
            call(m, routine);
        }
        return;
    }
    let page = lbu(m, record.wrapping_add(0x23));
    if let Some(&routine) = PAGES_B.get(page as usize) {
        call(m, routine);
    }
    let record = state(m);
    let page = lbu(m, record.wrapping_add(0x23));
    let mut text = match page {
        1 => lbu(m, record.wrapping_add(0x21)).wrapping_add(0x30),
        0 | 2 => {
            let selection = lh(m, record.wrapping_add(0xc));
            let target = lbu(m, record.wrapping_add(0x1f));
            let indexes = m.lw(record.wrapping_add(selection << 2).wrapping_add(0xc8));
            let index = lbu(m, target.wrapping_add(indexes).wrapping_sub(1));
            let item = index.wrapping_add(selection << 4);
            lbu(m, record.wrapping_add(item).wrapping_add(0x24)).wrapping_add(0x30)
        }
        _ => 0x30,
    };
    let record = state(m);
    if lbu(m, record.wrapping_add(0x23)) == 2 {
        for (from, plain, alternate) in [(0x3e, 0x58, 0x5d), (0x3f, 0x58, 0x5d), (0x43, 0x59, 0x5e)]
        {
            if text == from {
                text = if pad(m, 0x124) & 0x10 != 0 {
                    alternate
                } else {
                    plain
                };
            }
        }
    } else {
        if text == 0x3e && lbu(m, record.wrapping_add(0xb5)) == 1 {
            text = 0x5b;
        }
        if text == 0x3f {
            let record = state(m);
            if lbu(m, record.wrapping_add(0xb5)) == 1 {
                text = 0x5c;
            }
        }
        if text == 0x43 {
            let record = state(m);
            if lbu(m, record.wrapping_add(0xb4)) == 1 {
                text = 0x5a;
            }
        }
    }
    put_flag(m, 7, text);
}

/// Cancel (pad 0x20 with none of 0x5F held) leaves the page (sound 0x3B,
/// cell preset 1, state page 0); with toggle +0x1C on the selection's
/// visible items are drawn (and selection 3's extra items); then the
/// repeated pad bits 8 / 4 step the level or flip toggle +0x1D.
fn leave_or_draw(m: &mut Machine, sp: u32) {
    if press(m, 0x20, 0x5f) {
        sound(m, 0x3b);
        super::ov2_p2::set_cell_preset(m, 0, 1);
        let record = state(m);
        m.sb(record, 0);
        let record = state(m);
        m.sb(record.wrapping_add(1), 0);
        let record = state(m);
        let context = m.gw(CONTEXT);
        m.sw(record.wrapping_add(0x10), 0);
        m.sw(context.wrapping_add(0x12c), 0);
    }
    let record = state(m);
    if lbu(m, record.wrapping_add(0x1c)) == 1 {
        for at in 0x1c..0x21 {
            m.sb(sp + at, 0);
        }
        let selection = lh(m, record.wrapping_add(0xc));
        if lbu(m, selection.wrapping_add(ITEM_COUNTS)) != 0 {
            let mut index = 0u32;
            loop {
                let record = state(m);
                let selection = lh(m, record.wrapping_add(0xc));
                let items = m.lw(record.wrapping_add(selection << 2).wrapping_add(0xb8));
                let item = items.wrapping_add(index << 4);
                draw_item(m, sp, item, 0x10, DRAW_ITEM, 3);
                let selection = selection_of(m);
                let count = lbu(m, selection.wrapping_add(ITEM_COUNTS));
                index += 1;
                if !less(index, count) {
                    break;
                }
            }
        }
        if selection_of(m) == 3 {
            for at in 0x38..0x3d {
                m.sb(sp + at, 0);
            }
            for index in 0..3 {
                draw_item(m, sp, EXTRA_ITEMS + 0x14 * index, 0x28, DRAW_EXTRA, 4);
            }
        }
    }
    for bit in [8, 4] {
        if pad(m, 0x130) & bit == 0 {
            continue;
        }
        let record = state(m);
        if less(lh(m, record.wrapping_add(0xe)), 3) && m.lw(record.wrapping_add(0xc)) != 0x0001_0002
        {
            sound(m, 0x3d);
        }
        let record = state(m);
        if lh(m, record.wrapping_add(0xe)) == 0 {
            m.sw(record.wrapping_add(0x10), 0);
            super::ov2_p2::reset_cell(m, 2, 2);
            super::ov2_p2::reset_cell(m, 2, 3);
            super::ov2_p2::set_word_178(m, 2, 0xc);
            put_flag(m, 7, 0);
            put_flag(m, 0x14, 0);
        }
        if level_of(m) == 1 {
            call(m, LEVEL_1);
        }
        let record = state(m);
        if lh(m, record.wrapping_add(0xe)) == 2 {
            let toggle = lbu(m, record.wrapping_add(0x1d));
            m.sb(record.wrapping_add(0x1d), 1u32.wrapping_sub(toggle) as u8);
        } else if bit == 4 {
            let record = state(m);
            m.sb(record.wrapping_add(0x1d), 0);
            return;
        }
    }
    if level_of(m) != 2 {
        let record = state(m);
        m.sb(record.wrapping_add(0x1d), 0);
    }
}

/// Draw one item: unless its fade slot (word +0) is nonzero and not
/// finished (mode 2), copy its `words` words from +4 to the frame at
/// `local` and run `drawer` (frame struct, packet [state +8], ordering
/// table [visual + 8] + 4); the returned packet pointer goes to state +8.
fn draw_item(m: &mut Machine, sp: u32, item: u32, local: u32, drawer: u32, words: u32) {
    let slot = m.lw(item);
    if slot != 0 && super::ov2_p2::fade_mode(m, slot) != 2 {
        return;
    }
    let record = state(m);
    let packet = m.lw(record.wrapping_add(8));
    for n in 0..words {
        let value = m.lw(item.wrapping_add(4 + 4 * n));
        if n == words - 1 {
            let visual = super::ov2_p2::visual(m);
            let ot = m.lw(visual.wrapping_add(8)).wrapping_add(4);
            m.sw(sp + local + 4 * n, value);
            let next = dispatch::call(m, drawer, [sp + local, packet, ot, 0]);
            let record = state(m);
            m.sw(record.wrapping_add(8), next);
            return;
        }
        m.sw(sp + local + 4 * n, value);
    }
}

/// The page flags: +1 the level, +0x3B selection 2, +2 level nonzero, +3
/// and +4 the +0x1C toggle (2 at level 1), +5 and +6 the +0x1D toggle (2
/// at level 2), +0x15 the help line (0xB level 0, 0xC/0xD level 1, 9/0xA
/// or 0x10 level 2), +7 cleared below level 3. Returns the flag table or
/// 0.
fn set_page_flags(m: &mut Machine) -> u32 {
    let flags = self::flags(m);
    let record = state(m);
    let level = lbu(m, record.wrapping_add(0xe));
    m.sb(flags.wrapping_add(1), level as u8);
    let value = u32::from(selection_of(m) == 2);
    put_flag(m, 0x3b, value);
    let value = u32::from(level_of(m) != 0);
    put_flag(m, 2, value);
    for (toggle, on, at) in [(0x1c, 0, 3), (0x1c, 1, 4), (0x1d, 0, 5), (0x1d, 1, 6)] {
        let record = state(m);
        let wanted_level = if toggle == 0x1c { 1 } else { 2 };
        let value = if lbu(m, record.wrapping_add(toggle)) == on {
            if lh(m, record.wrapping_add(0xe)) == wanted_level {
                if toggle == 0x1c { 2 } else { wanted_level }
            } else {
                0
            }
        } else {
            1
        };
        put_flag(m, at, value);
    }
    put_flag(m, 0x15, 0);
    if level_of(m) == 0 {
        put_flag(m, 0x15, 0xb);
    }
    let record = state(m);
    if lh(m, record.wrapping_add(0xe)) == 1 {
        let value = if lbu(m, record.wrapping_add(0x1c)) == 0 {
            0xc
        } else {
            0xd
        };
        put_flag(m, 0x15, value);
    }
    let record = state(m);
    if lh(m, record.wrapping_add(0xe)) == 2 {
        let value = if lbu(m, record.wrapping_add(0x1d)) != 0 {
            0x10
        } else if pad(m, 0x124) & 0x10 != 0 {
            0xa
        } else {
            9
        };
        put_flag(m, 0x15, value);
    }
    if less(level_of(m), 3) {
        let flags = self::flags(m);
        m.sb(flags.wrapping_add(7), 0);
        flags
    } else {
        0
    }
}

/// Option page loaders and handlers (overlay 2, part 1; no arguments).
const LOAD_PAGE: u32 = 0x800d_3240;
const TOGGLE_OFF: u32 = 0x800d_4ca4;
const TOGGLE_ON: u32 = 0x800d_4394;
const PAGE_0: u32 = 0x800d_4f78;
const CLOSE: u32 = 0x800d_1b6c;
/// 800ECF88 (overlay 2, part 4): open selection handler `a0`.
const SELECT: u32 = 0x800e_cf88;

/// Reset cells 2 and 3 of list 2 and put word 0xC into slot 2 (state
/// word +0x10 cleared first, in the first call's delay slot).
fn reset_cells(m: &mut Machine, record: u32) {
    m.sw(record.wrapping_add(0x10), 0);
    super::ov2_p2::reset_cell(m, 2, 2);
    super::ov2_p2::reset_cell(m, 2, 3);
    super::ov2_p2::set_word_178(m, 2, 0xc);
}

/// 800D8CA8 (hub update mode 13): the options page: a pad change
/// (context +0x156 bits 0xB) requests a selection (state +0xE0) and resets
/// the cells; page word +0x10 selects the loader (0: 800D3240), the
/// toggle handlers and the menu ([`options_menu`] for 1, 800D4F78 for 0);
/// a requested selection follows the pad type (+0x124: 4/2 -> 0, 0x10 ->
/// 1, 1 -> 2, 8 -> 3, 0x20 -> 5; none -> 4), selections below 4 open
/// their handler (800ECF88), close the menu and restart fades 1 and 2.
/// Returns 0 after a requested selection, else the last call's value or
/// the selection byte.
pub fn options_page(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let context = m.gw(CONTEXT);
        if lhu(m, context.wrapping_add(0x156)) & 0xb != 0 {
            let record = state(m);
            m.sw(record.wrapping_add(0xe0), 1);
            reset_cells(m, record);
        }
        let record = state(m);
        if m.lw(record.wrapping_add(0x10)) == 0 {
            call(m, LOAD_PAGE);
        }
        let record = state(m);
        if m.lw(record.wrapping_add(0x10)) == 1 {
            if lbu(m, record.wrapping_add(0x1c)) == 0 {
                call(m, TOGGLE_OFF);
            }
            let record = state(m);
            if lbu(m, record.wrapping_add(0x1c)) == 1 {
                call(m, TOGGLE_ON);
            }
        }
        let record = state(m);
        match m.lw(record.wrapping_add(0x10)) {
            0 => call(m, PAGE_0),
            1 => {
                options_menu(m);
            }
            _ => {}
        }
        let record = state(m);
        if m.lw(record.wrapping_add(0xe0)) != 0 {
            let context = m.gw(CONTEXT);
            let change = lh(m, context.wrapping_add(0x156));
            let raw = lhu(m, context.wrapping_add(0x156));
            if change == 0 {
                for (bit, value) in [(4, 0), (2, 0), (0x10, 1), (1, 2), (8, 3), (0x20, 5)] {
                    if pad(m, 0x124) & bit != 0 {
                        let current = if bit == 4 { record } else { state(m) };
                        m.sh(current.wrapping_add(0xc), value);
                    }
                }
                let record = state(m);
                if less(lh(m, record.wrapping_add(0xc)), 4) {
                    let block = work(m);
                    m.sw(record.wrapping_add(0xe4), 1);
                    let flags = m.lw(block.wrapping_add(0x5c));
                    m.sb(flags.wrapping_add(0xb), 4);
                    let record = state(m);
                    m.sw(record.wrapping_add(0xe0), 0);
                }
                let context = m.gw(CONTEXT);
                let record = state(m);
                let pads = m.lw(context.wrapping_add(0x124));
                m.sw(record.wrapping_add(0xe8), pads);
                if m.lw(context.wrapping_add(0x124)) == 0 {
                    m.sh(record.wrapping_add(0xc), 4);
                }
                return 0;
            }
            if raw & 8 != 0 {
                put_flag(m, 0xb, 5);
            }
        } else {
            let context = m.gw(CONTEXT);
            let changed = lh(m, context.wrapping_add(0x156)) != 0
                || m.lw(context.wrapping_add(0x124)) != m.lw(record.wrapping_add(0xe8));
            if changed {
                m.sw(record.wrapping_add(0xe0), 1);
            }
        }
        let context = m.gw(CONTEXT);
        let record = state(m);
        let pads = m.lw(context.wrapping_add(0x124));
        m.sw(record.wrapping_add(0xe8), pads);
        if m.lw(context.wrapping_add(0x124)) == 0 {
            m.sh(record.wrapping_add(0xc), 4);
        }
        let record = state(m);
        if m.lw(record.wrapping_add(0xe4)) == 0 {
            let flags = flags(m);
            let selection = lbu(m, record.wrapping_add(0xc));
            m.sb(flags.wrapping_add(0xb), selection as u8);
            return selection;
        }
        reset_cells(m, record);
        let record = state(m);
        let selection = lh(m, record.wrapping_add(0xc));
        if less(selection, 4) {
            let visual = super::ov2_p2::visual(m);
            let handlers = m.lw(visual.wrapping_add(0x1bc));
            let handler = m.lw((selection << 2).wrapping_add(handlers));
            dispatch::call(m, SELECT, [handler, 0, 0, 0]);
        }
        put_flag(m, 0xb, 4);
        let first = state(m);
        m.sb(first.wrapping_add(0x1f), 0);
        let record = state(m);
        m.sw(first.wrapping_add(0xe4), 0);
        m.sb(record.wrapping_add(0x1e), 0);
        for offset in [0x22, 0x23, 0x20, 0x21] {
            let record = state(m);
            m.sb(record.wrapping_add(offset), 0);
        }
        let record = state(m);
        m.sh(record.wrapping_add(0xe), 0);
        call(m, CLOSE);
        put_flag(m, 0x3e, 0);
        put_flag(m, 0x3f, 0);
        super::ov2_p2::set_fade(m, 1, 3, u32::MAX, u32::MAX);
        super::ov2_p2::set_fade(m, 2, 3, u32::MAX, u32::MAX)
    })
}

/// 800D911C (hub update mode 14): the key-config colors (state +0xEC..
/// +0xFB: four entries of 0x88, 0x80, 0x10, 0xF0; +0x11A.. two of 0xF0,
/// 0x6E, 0), clear +0xFC and step the state. Returns the new step.
pub fn init_key_colors(m: &mut Machine) -> u32 {
    for index in 0..4u32 {
        for (offset, value) in [(0xec, 0x88), (0xf0, 0x80), (0xf4, 0x10), (0xf8, 0xf0)] {
            let record = state(m);
            m.sb(record.wrapping_add(index).wrapping_add(offset), value);
        }
    }
    for (offset, value) in [
        (0x11a, 0xf0),
        (0x11b, 0x6e),
        (0x11c, 0),
        (0x11e, 0xf0),
        (0x11f, 0x6e),
        (0x120, 0),
    ] {
        let record = state(m);
        m.sb(record.wrapping_add(offset), value);
    }
    let record = state(m);
    m.sb(record.wrapping_add(0x121), 0);
    m.sb(record.wrapping_add(0x11d), 0);
    let record = state(m);
    let step = lbu(m, record.wrapping_add(1)).wrapping_add(1);
    m.sw(record.wrapping_add(0xfc), 0);
    m.sb(record.wrapping_add(1), step as u8);
    step
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    Some(match target {
        0x800d_7db0 => options_menu(m),
        0x800d_8ca8 => options_page(m),
        0x800d_911c => init_key_colors(m),
        _ => return None,
    })
}
