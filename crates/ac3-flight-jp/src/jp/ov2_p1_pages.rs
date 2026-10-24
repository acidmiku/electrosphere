//! JP overlay 2, part 1: the hub menu (screen group 0) and the page
//! loaders.
//!
//! - 800CF364 / 800CF494: the hub menu's start and tick (screen group 0,
//!   screens 0 and 1 of [`SCREENS`](super::ov2_p1::SCREENS)). The tick runs
//!   the menu phase `W+0x1C4` of [`MENU`] (800CED38 wait, 800CEDA8 input,
//!   800CF1DC confirm) and then the load phase `W+0x1C6` of [`LOADS`].
//! - The load phases (`W+0x1C6`): 0 800CE554 start the page's files, 1
//!   800CE6B0 wait, 2 800CE6EC relocate, 3 idle ([`LOAD_IDLE`]); 4/5
//!   800CE75C/800CE7EC (list 0x79), 6/7 800CE894/800CE924 (0x7A), 8/9
//!   800CE9E4/800CEA6C (0x7B), 10/11 800CEAFC/800CEB94 (0x73 with a
//!   0x144-byte block at `[0x80112DDC]`), 12/13 800CEC70/800CED08 (0x7D).
//!   Each "start" queues the list (`context+0x44` = 1 until it is loaded)
//!   and advances the phase; each "finish" waits for `context+0x44` and
//!   relocates the loaded directory into the work block, then returns to
//!   idle (3).
//!
//! The results are the original `v0`, including the values the delay
//! slots leave (`0x80110000`, `0x800C0000`, the work block pointer):
//! 800CF494 returns the load phase's `v0`, which its caller ignores.
use super::ov2::MODULE;
use super::ov2_p1::{
    ALLOC, ARENA_RESET, LOAD_DONE, PLAY_SOUND, QUEUE_FILES, RELOCATE, SET_FLAG, TOTAL_SIZE, at,
    call, context, lh, slot, sound, work,
};
use crate::common::{RESOURCES, loaded_module};
use crate::machine::{Machine, SmallData};

/// 80109884: the hub menu phases by `W+0x1C4` (800CED38, 800CEDA8,
/// 800CF1DC).
pub const MENU: u32 = 0x8010_9884;
/// 8010984C: the load phases by `W+0x1C6` (see the module docs).
pub const LOADS: u32 = 0x8010_984c;
/// 800CE754: the load table's idle entry (phase 3), `jr ra; nop`: an 8-byte
/// map function (`tools/jp/fix_map.py`), claimed by [`dispatch`]. It
/// returns with `v0` unchanged, which after `jalr v0` (the table's only
/// use) is its own address.
pub const LOAD_IDLE: u32 = 0x800c_e754;
/// 80109840: the file list of each hub page (halfwords 0x75, 0x74, 0x77,
/// 0x76, 0x78), by the page of 800FE6E0.
pub const PAGE_FILES: u32 = 0x8010_9840;
/// 801127AC: twelve-byte records by mission (`context+0x38C`); a first
/// halfword of -1 means no briefing page.
pub const MISSION_PAGES: u32 = 0x8011_27ac;
/// `[0x80112DDC]`: the 0x144-byte block of load lists 0x73 and 0x7D.
pub const LIST_BLOCK: u32 = 0x8011_2ddc;
/// `[0x80112DB8]`: the archive block (0xBC bytes, allocated on ARCHIVE).
pub const ARCHIVE: u32 = 0x8011_2db8;

/// 800FE6E0: the hub page of mission `id` (`index`: the mission index of
/// the second campaign with option bit 0x10).
pub const PAGE_OF: u32 = 0x800f_e6e0;
/// 800FE764: the page's entry value (`id`, `page`), kept at `W+0xE4`.
pub const PAGE_VALUE: u32 = 0x800f_e764;
/// 800ECDA8: install a loaded bank (`block`).
pub const INSTALL_BANK: u32 = 0x800e_cda8;
/// 800DE908: `W+0x160[index & 0xFF]` = `value` (`value`, `slot`, `index`).
pub const SET_SLOT: u32 = 0x800d_e908;
/// 800DED38: the halfword +0xA of entry `index` of panel `panel`
/// (`[W+0x16C+4*panel]`, 12-byte entries): the panel's state.
pub const PANEL_STATE: u32 = 0x800d_ed38;
/// 800DEE84: hub cursor halfword `index` (`[[0x80112DC0]+0x58]`).
pub const CURSOR: u32 = 0x800d_ee84;
/// 800DEE08: hub cursor `index` = `value`, with the two optional
/// halfwords `a2`/`a3` (negative: unchanged).
pub const SET_CURSOR: u32 = 0x800d_ee08;
/// 800DEDF0: `W+0x178[index]` = `value`.
pub const SET_LAMP: u32 = 0x800d_edf0;
/// 800DE79C: reset the hub panel primitives.
pub const PANELS_RESET: u32 = 0x800d_e79c;
/// 800E11F4: the minimap cursor (`x`, `y`) at the mission's cell.
pub const MINIMAP_CURSOR: u32 = 0x800e_11f4;
/// 800DECA8: panel `panel` entry `entry` start (800DEB5C with 1, 0, 0, 0).
pub const PANEL_START: u32 = 0x800d_eca8;
/// 800DEBB8: panel `panel` entry `entry` slide by its table.
pub const PANEL_SLIDE: u32 = 0x800d_ebb8;
/// 800DEB5C: panel entry (`panel`, `entry`, `a2`, `a3`, stack +0x10,
/// +0x14).
pub const PANEL_SET: u32 = 0x800d_eb5c;
/// 800DECD4: panel entry restart (`panel`, `entry`).
pub const PANEL_RESTART: u32 = 0x800d_ecd4;
/// 800F3218: restart the five background sounds at `volume`.
pub const SOUNDS_RESTART: u32 = 0x800f_3218;
/// 800E18B8, 800E1334, 800E14D0, 800E1258: the hub menu's cursor, minimap
/// and panel animation steps.
pub const MENU_CURSOR: u32 = 0x800e_18b8;
pub const MINIMAP_STEP: u32 = 0x800e_1334;
pub const MINIMAP_DRAW: u32 = 0x800e_14d0;
pub const MINIMAP_FADE: u32 = 0x800e_1258;
/// 80100994: start the scene's zoom (`[SCENE]+0x18` = 3).
pub const SCENE_ZOOM: u32 = 0x8010_0994;

/// `W+0x1C6` += 1 (`lhu; addiu; sh`); returns the new value.
fn advance(m: &mut Machine) -> u32 {
    let w = work(m);
    let phase = u32::from(m.lhu(at(w, 0x1c6))).wrapping_add(1);
    m.sh(at(w, 0x1c6), phase as u16);
    phase
}

/// The "start" phases: resources +0x60 = `id`; a buffer of the list's
/// total size; with `keep`, the buffer is also `W+0x108`; `context+0x44`
/// = 1 and the load is queued (callback 8009B904); the phase advances.
pub(crate) fn start_list(m: &mut Machine, id: u32, keep: bool) -> u32 {
    let resources = m.gw(RESOURCES);
    m.sw(at(resources, 0x60), id);
    let bytes = call(m, TOTAL_SIZE, [at(resources, 0x60), 1, 0, 0]);
    let buffer = call(m, ALLOC, [bytes, 0, 0, 0]);
    let resources = m.gw(RESOURCES);
    if keep {
        let w = work(m);
        m.sw(at(w, 0x108), buffer);
    }
    let context = context(m);
    m.sw(at(context, 0x44), 1);
    call(m, QUEUE_FILES, [at(resources, 0x60), 1, buffer, LOAD_DONE]);
    advance(m)
}

/// 800CE554 (load phase 0, `mode` 0; 1 reloads into `W+0x108`): the page
/// (`W+0xC4` with option bit 0x10, else 800FE6E0 of `context+0x38C`) gives
/// the file list of [`PAGE_FILES`]; mode 0 allocates its buffer
/// (`W+0x108`). `W+0xE4` = 800FE764 of the mission and page; the list is
/// queued (`context+0x44` = 1) and the phase advances (returned).
///
/// For another `mode` the original passes its caller's `s2` as the
/// buffer; its one caller (800CF494) passes 0, so this models modes 0
/// and 1 only (other modes queue to address 0).
pub fn load_ce554(m: &mut Machine, mode: u32) -> u32 {
    m.push_frame(0x28);
    let context = context(m);
    let page = if m.lw(at(context, 0x96c)) & 0x10 != 0 {
        let w = work(m);
        m.lw(at(w, 0xc4))
    } else {
        let id = lh(m, at(context, 0x38c));
        call(m, PAGE_OF, [id, 0, 0, 0])
    };
    let mut buffer = 0;
    if mode == 0 {
        let resources = m.gw(RESOURCES);
        let id = lh(m, PAGE_FILES.wrapping_add(page << 1));
        m.sw(at(resources, 0x60), id);
        let bytes = call(m, TOTAL_SIZE, [at(resources, 0x60), 1, 0, 0]);
        buffer = call(m, ALLOC, [bytes, 0, 0, 0]);
        let w = work(m);
        m.sw(at(w, 0x108), buffer);
    }
    if mode == 1 {
        let resources = m.gw(RESOURCES);
        let id = lh(m, PAGE_FILES.wrapping_add(page << 1));
        let w = work(m);
        m.sw(at(resources, 0x60), id);
        buffer = m.lw(at(w, 0x108));
    }
    let context = self::context(m);
    let id = lh(m, at(context, 0x38c));
    let value = call(m, PAGE_VALUE, [id, page, 0, 0]);
    let w = work(m);
    let resources = m.gw(RESOURCES);
    let context = self::context(m);
    m.sw(at(w, 0xe4), value);
    m.sw(at(context, 0x44), 1);
    call(m, QUEUE_FILES, [at(resources, 0x60), 1, buffer, LOAD_DONE]);
    let phase = advance(m);
    m.pop_frame(0x28);
    phase
}

/// 800CE6B0 (load phase 1): advance once `context+0x44` is clear (and
/// return the phase); while loading, return 0x80110000 (the delay slot's
/// `lui`).
pub fn wait_ce6b0(m: &mut Machine) -> u32 {
    let context = context(m);
    if m.lw(at(context, 0x44)) != 0 {
        return 0x8011_0000;
    }
    advance(m)
}

/// 800CE6EC (load phase 2): relocate the loaded page directory into
/// `W+0x24` (its first word to `W+0xB8`) and its second entry into
/// `W+0xF4`; idle (returns 3).
pub fn relocate_ce6ec(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let resources = m.gw(RESOURCES);
    let block = m.lw(at(resources, 0x60));
    let block = call(m, RELOCATE, [block, 0, 0, 0]);
    let w = work(m);
    m.sw(at(w, 0x24), block);
    let first = m.lw(block);
    let handle = m.lw(at(w, 0x24));
    m.sw(at(w, 0xb8), first);
    let second = m.lw(at(handle, 4));
    let second = call(m, RELOCATE, [second, 0, 0, 0]);
    let w = work(m);
    m.sw(at(w, 0xf4), second);
    m.sh(at(w, 0x1c6), 3);
    m.pop_frame(0x18);
    3
}

/// 800CE75C (load phase 4): start list 0x79 into `W+0x108`.
pub fn start_ce75c(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let phase = start_list(m, 0x79, true);
    m.pop_frame(0x18);
    phase
}

/// 800CE894 (load phase 6): start list 0x7A into `W+0x108`.
pub fn start_ce894(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let phase = start_list(m, 0x7a, true);
    m.pop_frame(0x18);
    phase
}

/// 800CE9E4 (load phase 8): start list 0x7B.
pub fn start_ce9e4(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let phase = start_list(m, 0x7b, false);
    m.pop_frame(0x18);
    phase
}

/// 800CEAFC (load phase 10) and 800CEC70 (12): a 0x144-byte block at
/// `[0x80112DDC]`, then start list 0x73 / 0x7D.
fn start_block(m: &mut Machine, id: u32) -> u32 {
    m.push_frame(0x18);
    let block = call(m, ALLOC, [0x144, 0, 0, 0]);
    m.sw(LIST_BLOCK, block);
    let phase = start_list(m, id, false);
    m.pop_frame(0x18);
    phase
}

/// 800CEAFC (load phase 10): see [`start_block`] (list 0x73).
pub fn start_ceafc(m: &mut Machine) -> u32 {
    start_block(m, 0x73)
}

/// 800CEC70 (load phase 12): see [`start_block`] (list 0x7D).
pub fn start_cec70(m: &mut Machine) -> u32 {
    start_block(m, 0x7d)
}

/// The "finish" phases 800CE7EC, 800CE924, 800CEA6C: while
/// `context+0x44` is 1 return 0x800C0000 (the delay slot's `lui`).
/// Otherwise `W+0x28` = the loaded block, relocated; with `bank`, install
/// and release its bank entry (800ECDA8, then reset the arena to it);
/// relocate the entries `targets` (block offset, work offset); idle
/// (returns 3).
fn finish(m: &mut Machine, bank: Option<u32>, targets: &[(u32, u32)]) -> u32 {
    m.push_frame(0x20);
    let context = context(m);
    let result = if m.lw(at(context, 0x44)) == 1 {
        0x800c_0000
    } else {
        let resources = m.gw(RESOURCES);
        let raw = m.lw(at(resources, 0x60));
        let w = work(m);
        m.sw(at(w, 0x28), raw);
        let block = call(m, RELOCATE, [raw, 0, 0, 0]);
        if let Some(offset) = bank {
            let entry = m.lw(at(block, offset));
            call(m, INSTALL_BANK, [entry, 0, 0, 0]);
            let entry = m.lw(at(block, offset));
            call(m, ARENA_RESET, [entry, 0, 0, 0]);
        }
        for &(from, to) in targets {
            let entry = m.lw(at(block, from));
            let entry = call(m, RELOCATE, [entry, 0, 0, 0]);
            let w = work(m);
            m.sw(at(w, to), entry);
        }
        let w = work(m);
        m.sh(at(w, 0x1c6), 3);
        3
    };
    m.pop_frame(0x20);
    result
}

/// 800CE7EC (load phase 5): finish list 0x79: bank +8; +0 to `W+0x1D4`,
/// +4 to `W+0x1DC`.
pub fn finish_ce7ec(m: &mut Machine) -> u32 {
    finish(m, Some(8), &[(0, 0x1d4), (4, 0x1dc)])
}

/// 800CE924 (load phase 7): finish list 0x7A: bank +0xC; +0, +4, +8 to
/// `W+0x1D4`, `W+0x1D8`, `W+0x1DC`.
pub fn finish_ce924(m: &mut Machine) -> u32 {
    finish(m, Some(0xc), &[(0, 0x1d4), (4, 0x1d8), (8, 0x1dc)])
}

/// 800CEA6C (load phase 9): finish list 0x7B: +0 to `W+0x1D4`, +4 to
/// `W+0x1DC`.
pub fn finish_cea6c(m: &mut Machine) -> u32 {
    finish(m, None, &[(0, 0x1d4), (4, 0x1dc)])
}

/// 800CEB94 (load phase 11): finish list 0x73: while loading return 1.
/// With context +0x95F clear, idle (returns 3). Otherwise `W+0x1C` = the
/// relocated block, its second entry relocated into `W+0x160[2]`
/// (800DE908 with 0x16), `W+0x98` and `W+0xA0..0xA8` from the block
/// (+8, +0x10, +0x14, +0x18) and the phase advances (returned).
pub fn finish_ceb94(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = context(m);
    let result = if m.lw(at(context, 0x44)) == 1 {
        1
    } else if m.lbu(at(context, 0x95f)) == 0 {
        let w = work(m);
        m.sh(at(w, 0x1c6), 3);
        3
    } else {
        let resources = m.gw(RESOURCES);
        let raw = m.lw(at(resources, 0x60));
        let block = call(m, RELOCATE, [raw, 0, 0, 0]);
        let w = work(m);
        m.sw(at(w, 0x1c), block);
        let entry = m.lw(at(block, 4));
        let entry = call(m, RELOCATE, [entry, 0, 0, 0]);
        call(m, SET_SLOT, [entry, 0x16, 2, 0]);
        let w = work(m);
        let block = m.lw(at(w, 0x1c));
        let again = m.lw(at(w, 0x1c));
        let value = m.lw(at(block, 8));
        m.sw(at(w, 0x98), value);
        let value = m.lw(at(again, 0x10));
        m.sw(at(w, 0xa0), value);
        let value = m.lw(at(again, 0x14));
        m.sw(at(w, 0xa4), value);
        let phase = u32::from(m.lhu(at(w, 0x1c6))).wrapping_add(1);
        let value = m.lw(at(again, 0x18));
        m.sh(at(w, 0x1c6), phase as u16);
        m.sw(at(w, 0xa8), value);
        phase
    };
    m.pop_frame(0x18);
    result
}

/// 800CED08 (load phase 13): finish list 0x7D: idle once loaded (returns
/// 3); while loading return 0x80110000 (the delay slot's `lui`).
pub fn finish_ced08(m: &mut Machine) -> u32 {
    let context = context(m);
    if m.lw(at(context, 0x44)) == 1 {
        return 0x8011_0000;
    }
    let w = work(m);
    m.sh(at(w, 0x1c6), 3);
    3
}

/// 800CED38 (menu phase 0): count `W+0x1A4` while panel 0's first entry
/// is not at rest (800DED38 is 0), and advance the menu phase once hub
/// cursor 1 is 2 (returns the phase); otherwise 0x80110000.
pub fn wait_ced38(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    if call(m, PANEL_STATE, [0, 1, 0, 0]) == 0 {
        let w = work(m);
        let count = m.lw(at(w, 0x1a4)).wrapping_add(1);
        m.sw(at(w, 0x1a4), count);
    }
    let result = if call(m, CURSOR, [1, 0, 0, 0]) == 2 {
        let w = work(m);
        let phase = u32::from(m.lhu(at(w, 0x1c4))).wrapping_add(1);
        m.sh(at(w, 0x1c4), phase as u16);
        phase
    } else {
        0x8011_0000
    };
    m.pop_frame(0x18);
    result
}

/// A menu move (800CEDA8): with the loader idle (`W+0x1C6` = 3) and panel
/// 0's first entry not sliding (800DED38 is not 2), and the loader still
/// idle, the selection `W+0x7E` becomes `step(selection)` and sound 0x3D
/// plays. Returns whether the move was taken (the original goes on to the
/// next test only when the loader is not idle).
fn menu_move(m: &mut Machine, step: impl Fn(u8) -> u8) -> MoveResult {
    let w = work(m);
    let phase = lh(m, at(w, 0x1c6));
    if phase != 3 {
        return MoveResult::NotIdle;
    }
    if call(m, PANEL_STATE, [0, 1, 0, 0]) != 2 {
        let w = work(m);
        if lh(m, at(w, 0x1c6)) == phase {
            let selection = m.lbu(at(w, 0x7e));
            m.sb(at(w, 0x7e), step(selection));
            sound(m, 0x3d);
        }
    }
    MoveResult::Done
}

enum MoveResult {
    NotIdle,
    Done,
}

/// 800CEDA8 (menu phase 1): the hub menu input. Confirm (pressed 0x10,
/// none of 0x2F held) with the loader idle advances the menu phase and
/// starts the selection's load: MISSION (`W+0x7E` 0) restarts panel 0
/// (without option bit 0x10) and reloads (load phase 8) for mission 0x3B
/// or page 3; SEARCH (1) idles with option bit 0x10, else frees the page
/// buffer and loads list 0x79 (phase 4); ARCHIVE (2) frees it, allocates
/// the archive block and loads list 0x7A (phase 6); OPTIONS (3) frees it
/// and loads list 0x73 (phase 10). The cursors 1 and 2 become 3, MISSION
/// starts the scene zoom, and sound 0x3C plays. Right (0x8) and left
/// (0x4) move the selection: over four items, or two with option bit
/// 0x10. Then the minimap and panel steps; returns 800E1258's result.
pub fn input_ceda8(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    // 800E18B8 reads no argument register
    call(m, MENU_CURSOR, [0; 4]);
    let context = context(m);
    let confirm = m.lw(at(context, 0x12c)) & 0x10 != 0 && m.lw(at(context, 0x128)) & 0x2f == 0;
    if confirm {
        let w = work(m);
        let phase = lh(m, at(w, 0x1c6));
        if phase == 3 {
            confirm_menu(m, context, w);
        }
    }
    let context = self::context(m);
    let pressed = m.lw(at(context, 0x130));
    if m.lw(at(context, 0x96c)) & 0x10 != 0 {
        let mut moved = false;
        if pressed & 8 != 0 {
            moved = matches!(
                menu_move(m, |s| if s != 0 { 0 } else { 1 }),
                MoveResult::Done
            );
        }
        if !moved {
            let context = self::context(m);
            if m.lw(at(context, 0x130)) & 4 != 0 {
                menu_move(m, |s| if s != 0 { s.wrapping_sub(1) } else { 1 });
            }
        }
    } else {
        let mut moved = false;
        if pressed & 8 != 0 {
            moved = matches!(
                menu_move(m, |s| if s < 3 { s + 1 } else { 0 }),
                MoveResult::Done
            );
        }
        if !moved {
            let context = self::context(m);
            if m.lw(at(context, 0x130)) & 4 != 0 {
                menu_move(m, |s| if s != 0 { s.wrapping_sub(1) } else { 3 });
            }
        }
    }
    // 800E1334, 800E14D0 and 800E1258 read no argument register
    call(m, MINIMAP_STEP, [0; 4]);
    call(m, MINIMAP_DRAW, [0; 4]);
    let result = call(m, MINIMAP_FADE, [0; 4]);
    m.pop_frame(0x20);
    result
}

/// The confirm of 800CEDA8 (see there); `w` and `context` as loaded.
fn confirm_menu(m: &mut Machine, context: u32, w: u32) {
    let phase = m.lhu(at(w, 0x1c4)).wrapping_add(1);
    let selection = m.lbu(at(w, 0x7e));
    m.sh(at(w, 0x1c4), phase);
    if selection == 0 {
        if m.lw(at(context, 0x96c)) & 0x10 == 0 {
            call(m, PANEL_RESTART, [0, 1, 0, 0]);
        }
        let w = work(m);
        if m.lbu(at(w, 0x7e)) == 0 {
            let context = self::context(m);
            if lh(m, at(context, 0x38c)) == 0x3b || lh(m, at(context, 0x954)) == 3 {
                m.sh(at(w, 0x1c6), 8);
            }
        }
    }
    let w = work(m);
    if m.lbu(at(w, 0x7e)) == 1 {
        let context = self::context(m);
        if m.lw(at(context, 0x96c)) & 0x10 != 0 {
            m.sh(at(w, 0x1c6), 3);
        } else {
            let buffer = m.lw(at(w, 0x108));
            call(m, ARENA_RESET, [buffer, 0, 0, 0]);
            let w = work(m);
            m.sh(at(w, 0x1c6), 4);
        }
    }
    let w = work(m);
    if m.lbu(at(w, 0x7e)) == 2 {
        let buffer = m.lw(at(w, 0x108));
        call(m, ARENA_RESET, [buffer, 0, 0, 0]);
        let block = call(m, ALLOC, [0xbc, 0, 0, 0]);
        m.sw(ARCHIVE, block);
        let w = work(m);
        m.sh(at(w, 0x1c6), 6);
    }
    call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
    call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
    let w = work(m);
    if m.lbu(at(w, 0x7e)) == 0 {
        call(m, SCENE_ZOOM, [0; 4]);
    }
    let w = work(m);
    if m.lbu(at(w, 0x7e)) == 3 {
        let buffer = m.lw(at(w, 0x108));
        call(m, ARENA_RESET, [buffer, 0, 0, 0]);
        let w = work(m);
        m.sh(at(w, 0x1c6), 0xa);
    }
    sound(m, 0x3c);
}

/// 800CF1DC (menu phase 2): once hub cursor 1 is 0 and the loader idle,
/// open the selection: MISSION sets screen group 1 and, when the mission
/// has a briefing page ([`MISSION_PAGES`]), slides panel 0's entries 5..7
/// in (entry 7 with 0x1FA at stack +0x10), lamp 0 = 0xE1, cursor 1 = 1
/// (0, 0x28) and sound 0x5A; SEARCH sets group 2, or with option bit 0x10
/// `W+0x7F` = 1 and context option bit 2 (leave the hub); ARCHIVE group
/// 3; OPTIONS group 4 and sound 0x5A. Screen `W+0x7D` = 0. Returns the
/// work block pointer (the original's last load), 0x80110000 while the
/// cursor moves and 3 while the loader is busy.
pub fn open_cf1dc(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x20);
    let result = open(m, frame);
    m.pop_frame(0x20);
    result
}

fn open(m: &mut Machine, frame: u32) -> u32 {
    if call(m, CURSOR, [1, 0, 0, 0]) != 0 {
        return 0x8011_0000;
    }
    let w = work(m);
    let phase = lh(m, at(w, 0x1c6));
    if phase != 3 {
        return 3;
    }
    let mut play = false;
    match m.lbu(at(w, 0x7e)) {
        0 => {
            m.sb(at(w, 0x7c), 1);
            let context = context(m);
            let mission = lh(m, at(context, 0x38c));
            let record = MISSION_PAGES.wrapping_add(mission.wrapping_mul(12));
            if lh(m, record) != u32::MAX {
                call(m, PANEL_SLIDE, [0, 5, 0, 0]);
                call(m, PANEL_SLIDE, [0, 6, 0, 0]);
                m.sw(at(frame, 0x10), 0x1fa);
                m.sw(at(frame, 0x14), 0);
                call(m, PANEL_SET, [0, 7, 0, 0]);
                call(m, SET_LAMP, [0, 0xe1, 0, 0]);
                call(m, SET_CURSOR, [1, 1, 0, 0x28]);
                play = true;
            }
        }
        1 => {
            let context = context(m);
            if m.lw(at(context, 0x96c)) & 0x10 != 0 {
                m.sb(at(w, 0x7f), 1);
                let context = self::context(m);
                let options = m.lw(at(context, 0x96c));
                m.sw(at(context, 0x96c), options | 2);
            } else {
                m.sb(at(w, 0x7c), 2);
            }
        }
        2 => m.sb(at(w, 0x7c), 3),
        3 => {
            m.sb(at(w, 0x7c), 4);
            play = true;
        }
        _ => {}
    }
    if play {
        call(m, PLAY_SOUND, [0x5a, 0, 0, 0]);
    }
    let w = work(m);
    m.sb(at(w, 0x7d), 0);
    w
}

/// 800CF364 (screen 0, the hub menu's start): menu and load phases 0,
/// `W+0x50` = 0, screen 1; cursors 1 and 2 = 1 (0, 0x28); reset the
/// panels; minimap cursor (1, 0); panel 0 entry 2 starts; after a
/// returning selection 4 (`W+0x7E`), panel 0's entry 1 slides and starts,
/// cursor 0xF = 1 (0, 0x19) and the selection becomes 0; lamps 0 = 7 and
/// 2 = 0; hub flag 1 = selection + 1; `W+0xC4` = the page of the mission
/// (800FE6E0 of `context+0x38C/0x38E`); sound 0x5A; the background sounds
/// restart at volume 0. Returns 800F3218's result.
pub fn start_cf364(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let w = work(m);
    m.sh(at(w, 0x1c4), 0);
    m.sw(at(w, 0x50), 0);
    m.sb(at(w, 0x7d), 1);
    call(m, SET_CURSOR, [1, 1, 0, 0x28]);
    call(m, SET_CURSOR, [2, 1, 0, 0x28]);
    // 800DE79C reads no argument register
    call(m, PANELS_RESET, [0; 4]);
    call(m, MINIMAP_CURSOR, [1, 0, 0, 0]);
    call(m, PANEL_START, [0, 2, 0, 0]);
    let w = work(m);
    if m.lbu(at(w, 0x7e)) == 4 {
        call(m, PANEL_SLIDE, [0, 1, 0, 0]);
        call(m, PANEL_START, [0, 0, 0, 0]);
        call(m, SET_CURSOR, [0xf, 1, 0, 0x19]);
        let w = work(m);
        m.sb(at(w, 0x7e), 0);
    }
    call(m, SET_LAMP, [0, 7, 0, 0]);
    call(m, SET_LAMP, [2, 0, 0, 0]);
    let w = work(m);
    let selection = u32::from(m.lbu(at(w, 0x7e)));
    call(m, SET_FLAG, [1, selection.wrapping_add(1), 0, 0]);
    let context = context(m);
    let id = lh(m, at(context, 0x38c));
    let index = lh(m, at(context, 0x38e));
    let page = call(m, PAGE_OF, [id, index, 0, 0]);
    let w = work(m);
    m.sw(at(w, 0xc4), page);
    sound(m, 0x5a);
    let result = call(m, SOUNDS_RESTART, [0; 4]);
    m.pop_frame(0x18);
    result
}

/// 800CF494 (screen 1, the hub menu's tick): the menu phase `W+0x1C4` of
/// [`MENU`] (the argument registers pass through), then the load phase
/// `W+0x1C6` of [`LOADS`] with `a0` = 0; returns the load phase's `v0`.
pub fn tick_cf494(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let w = work(m);
    let phase = lh(m, at(w, 0x1c4));
    let handler = m.lw(slot(MENU, phase, 0));
    call(m, handler, args);
    let result = run_load_phase(m);
    m.pop_frame(0x18);
    result
}

/// The load phase `W+0x1C6` of [`LOADS`] with `a0` = 0 (the loaders read
/// only a0, 800CE554's mode), as 800CF494, 800D12E0, 800DE540 and
/// 800F7A.. run it (`lh; sll 2; lw; jalr` with `a0` = 0 in the delay
/// slot). Returns its `v0`: in phase 3 the idle stub's own address.
pub fn run_load_phase(m: &mut Machine) -> u32 {
    let w = work(m);
    let phase = lh(m, at(w, 0x1c6));
    let loader = m.lw(slot(LOADS, phase, 0));
    if loader == LOAD_IDLE {
        loader
    } else {
        call(m, loader, [0; 4])
    }
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800c_e554 => load_ce554(m, args[0]),
        0x800c_e6b0 => wait_ce6b0(m),
        0x800c_e6ec => relocate_ce6ec(m),
        0x800c_e754 => LOAD_IDLE,
        0x800c_e75c => start_ce75c(m),
        0x800c_e7ec => finish_ce7ec(m),
        0x800c_e894 => start_ce894(m),
        0x800c_e924 => finish_ce924(m),
        0x800c_e9e4 => start_ce9e4(m),
        0x800c_ea6c => finish_cea6c(m),
        0x800c_eafc => start_ceafc(m),
        0x800c_eb94 => finish_ceb94(m),
        0x800c_ec70 => start_cec70(m),
        0x800c_ed08 => finish_ced08(m),
        0x800c_ed38 => wait_ced38(m),
        0x800c_eda8 => input_ceda8(m),
        0x800c_f1dc => open_cf1dc(m),
        0x800c_f364 => start_cf364(m),
        0x800c_f494 => tick_cf494(m, args),
        _ => return None,
    })
}
