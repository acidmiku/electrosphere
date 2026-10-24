//! JP overlay 1, part 1: saving the new account (800CFC38..800D12A4):
//! the slot and file choice, the card checks and the save steps.
//!
//! These are dialog routines of the account screen (menu block dialog
//! +0x4C/+0x4E): the chosen slot is the menu block's +0x5A, the file
//! +0x5D. The card operations (800D5A38, 800D5E74, 800D5EC4, 800D64C4,
//! 800D6604, 800D6814, 800D6D78, 800D6F04) are overlay 1's other half.
use super::ov1_p1::{MODULE, call, context, ctx_lw, framed, lbu, lhu, lt, menu, menu_lbu, off};
use super::ov1_p1_card::{frame_counter, name_from_record, slot_state};
use super::ov1_p1_screens::{c2, c4, count_up, dialog_step_up, dialog_to, sound};
use crate::{common::loaded_module, machine::Machine};

/// The dialog step +0x4E advanced by `n`.
fn step_by(m: &mut Machine, n: u32) {
    let block = menu(m);
    let step = lhu(m, off(block, 0x4e)).wrapping_add(n);
    m.sh(off(block, 0x4e), step as u16);
}
/// The chosen slot's state ([`slot_state`] of +0x5A).
fn chosen_state(m: &mut Machine) -> u32 {
    let slot = menu_lbu(m, 0x5a);
    slot_state(m, slot)
}
/// A slot state that lost the card: 2 (no card) or 6.
const fn lost(state: u32) -> bool {
    state == 2 || state == 6
}

/// The save starts: dialog 0x13, the save record reset (800DA1B8), the
/// context's +0x954 = 2, +0x968 = the frame counter, the account name
/// from the record; sound 0x59.
fn begin_save(m: &mut Machine) {
    dialog_to(m, 0x13);
    call(m, 0x800d_a1b8, [0; 4]);
    let ctx = context(m);
    m.sh(off(ctx, 0x954), 2);
    let frame = frame_counter(m);
    let ctx = context(m);
    m.sw(off(ctx, 0x968), frame);
    name_from_record(m);
    sound(m, 0x59);
}

/// Circle starts the save ([`begin_save`]), cross goes back (dialog 7,
/// sound 0x3B).
fn confirm_or_back(m: &mut Machine) {
    let pressed = ctx_lw(m, 0x12c);
    if pressed & 0x10 != 0 {
        begin_save(m);
    } else if pressed & 0x20 != 0 {
        dialog_to(m, 7);
        sound(m, 0x3b);
    }
}

/// 800CFC38 (no capture reaches it): icons 2 (2), 1 (0x17); dialog step
/// up; 800DA49C(0x16); icon 4 (0x16).
pub fn ask_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        c2(m, 0x800d_a9a8, 2, 2);
        c2(m, 0x800d_a9a8, 1, 0x17);
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a49c, 0x16, 0x17);
        c2(m, 0x800d_a9a8, 4, 0x16);
    });
}

/// 800CFC94 (no capture reaches it): circle starts the save, cross
/// returns to dialog 0 (sound 0x3B).
pub fn ask(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            begin_save(m);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 0);
            sound(m, 0x3b);
        }
    });
}

/// 800CFD28: dialog step up and icon 1 (0x1A). Models no result: the
/// original leaves 800DA9A8's `v0` (the condition byte's address), which
/// that routine's translation does not model, and its only caller, step 0
/// of the account screen's dialog list 9 (0x800DD50C, through the dialog
/// step's `jalr`), discards it.
pub fn checking_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a9a8, 1, 0x1a);
    });
}

/// 800CFD60: once both slots have been probed (neither state 0 nor 6),
/// dialog 0xA and icon 4 (0x15).
pub fn wait_probes(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if first == 0 || first == 6 || second == 0 || second == 6 {
            return;
        }
        let block = menu(m);
        m.sh(off(block, 0x4c), 0xa);
        m.sh(off(block, 0x4e), 0);
        c2(m, 0x800d_a9a8, 4, 0x15);
    });
}

/// 800CFDCC: with no card in either slot (states 2 or 6): cursors 1 and
/// 2, dialog step up, icon 1 (0x18), 800DA49C(0x17), +0xD1 = 1 and the
/// text 0x20056 ("insert a card"); otherwise dialog 0xB.
pub fn no_card_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if !(lost(first) && lost(second)) {
            dialog_to(m, 0xb);
            return;
        }
        c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
        c4(m, 0x800d_a838, 2, 1, 0, 0x14);
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a9a8, 1, 0x18);
        c2(m, 0x800d_a49c, 0x17, 0x18);
        let block = menu(m);
        m.sb(off(block, 0xd1), 1);
        c2(m, 0x800d_a828, 0x0002_0056, 0x18);
    });
}

/// 800CFEA4 (no capture reaches it): while both slots have no card,
/// circle starts the save and cross goes back (dialog 7); a card makes
/// it dialog 9.
pub fn no_card(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if !(lost(first) && lost(second)) {
            dialog_to(m, 9);
            return;
        }
        confirm_or_back(m);
    });
}

/// 800CFF90 (slot choice, open): dialog step up; panel 0x12, the text
/// 0x40036, cursor 4, icons 1 (0x19) and 4 (2), the choice box
/// (800DBF6C at (0x40, 0x32)), 800DA49C(0x18); slot 1 is preselected
/// when slot 0 is not in state 5 but slot 1 is; +0xD1 = 0.
pub fn slot_open(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0x12, 0);
        c2(m, 0x800d_a828, 0x0004_0036, 0);
        c4(m, 0x800d_a838, 4, 1, 0, 0x3c);
        c2(m, 0x800d_a9a8, 1, 0x19);
        c2(m, 0x800d_a9a8, 4, 2);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0x18, 0x32);
        if first != 5 && second == 5 {
            let block = menu(m);
            m.sb(off(block, 0x5a), 1);
        }
        let block = menu(m);
        m.sb(off(block, 0xd1), 0);
    });
}

/// 800D0074 (slot choice): pad +0x130 bits 0/1 switch the slot (+0x5A);
/// with no card in either slot back to the no-card dialog (0xA: panel
/// 0x11, the text 0x20056, cursors 2 and 1); a slot without a usable
/// card (states 0, 2, 6) is not selectable (sound 0x3D on a change);
/// icons 6 and 7; circle goes on (dialog 0xC, sound 0x59,
/// 800DA49C(0x19)), cross goes back (dialog 7, sound 0x3B).
pub fn slot_choice(m: &mut Machine) {
    framed(m, 0x28, |m, _| {
        let ctx = context(m);
        let block = menu(m);
        let held = m.lw(off(ctx, 0x130));
        let old = lbu(m, off(block, 0x5a));
        if held & 3 != 0 {
            m.sb(off(block, 0x5a), (old ^ 1) as u8);
        }
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if lost(first) && lost(second) {
            let block = menu(m);
            m.sb(off(block, 0xd1), 1);
            dialog_to(m, 0xa);
            c2(m, 0x800d_a77c, 0x11, 0);
            c2(m, 0x800d_a828, 0x0002_0056, 0);
            c4(m, 0x800d_a838, 2, 1, 0, 0x14);
            c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
            return;
        }
        if first == 0 || lost(first) {
            let block = menu(m);
            m.sb(off(block, 0x5a), 1);
        }
        if second == 0 || lost(second) {
            let block = menu(m);
            m.sb(off(block, 0x5a), 0);
        }
        if menu_lbu(m, 0x5a) != old {
            sound(m, 0x3d);
        }
        if menu_lbu(m, 0x5a) == 0 {
            c2(m, 0x800d_a9a8, 6, 0);
            c2(m, 0x800d_a9a8, 7, 1);
        }
        if menu_lbu(m, 0x5a) == 1 {
            c2(m, 0x800d_a9a8, 6, 1);
            c2(m, 0x800d_a9a8, 7, 0);
        }
        if first == 0 || first == 2 {
            c2(m, 0x800d_a9a8, 6, 2);
        }
        if second == 0 || second == 2 {
            c2(m, 0x800d_a9a8, 7, 2);
        }
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            dialog_to(m, 0xc);
            sound(m, 0x59);
            c2(m, 0x800d_a49c, 0x19, 0);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 7);
            sound(m, 0x3b);
        }
    });
}

/// 800D02DC: icon 1 (0x1A); a card ready in the chosen slot (state 1)
/// opens the next prompt (dialog step up, panel 0x11, the text 0x20056,
/// icons 1 (0x1C) and 4 (0x15), 800DA49C(0x1A)); otherwise dialog 0xD.
pub fn card_ready(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        c2(m, 0x800d_a9a8, 1, 0x1a);
        if chosen_state(m) != 1 {
            let block = menu(m);
            m.sh(off(block, 0x4c), 0xd);
            return;
        }
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0x11, 0);
        c2(m, 0x800d_a828, 0x0002_0056, 0);
        c2(m, 0x800d_a9a8, 1, 0x1c);
        c2(m, 0x800d_a9a8, 4, 0x15);
        c2(m, 0x800d_a49c, 0x1a, 0x15);
    });
}

/// 800D0384, 800D076C, 800D0A10, 800D0CEC (four copies of one routine;
/// none but 800D0A10's neighbours reached in the captures): the chosen
/// slot lost its card (states 2, 6): dialog 9; else circle starts the
/// save and cross goes back (dialog 7).
pub fn slot_confirm(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        if lost(chosen_state(m)) {
            dialog_to(m, 9);
            return;
        }
        confirm_or_back(m);
    });
}

/// 800D0460: an unformatted card in the chosen slot (state 3): the
/// format prompt (dialog step up, panel 0x13, the text 0x80056, cursors
/// 1..3 off and 4, icons 1 (0x1E) and 4 (0x15), 800DA49C(0x1B));
/// otherwise dialog 0xE.
pub fn unformatted_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if chosen_state(m) != 3 {
            let block = menu(m);
            m.sh(off(block, 0x4c), 0xe);
            return;
        }
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0x13, 0);
        c2(m, 0x800d_a828, 0x0008_0056, 0);
        c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
        c4(m, 0x800d_a838, 2, 3, u32::MAX, u32::MAX);
        c4(m, 0x800d_a838, 3, 3, u32::MAX, u32::MAX);
        c4(m, 0x800d_a838, 4, 1, 0, 0x14);
        c2(m, 0x800d_a9a8, 1, 0x1e);
        c2(m, 0x800d_a9a8, 4, 0x15);
        c2(m, 0x800d_a49c, 0x1b, 0x15);
    });
}

/// 800D0548 (no capture reaches it): the format prompt: a lost card
/// gives dialog 9, state 0 waits; START formats (icon 1 (0x20), dialog
/// step up, sound 0x59, 800DA49C(0x1C)), cross goes back (dialog 7).
pub fn unformatted(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let state = chosen_state(m);
        if lost(state) {
            dialog_to(m, 9);
            return;
        }
        if state == 0 {
            return;
        }
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x40 != 0 {
            c2(m, 0x800d_a9a8, 1, 0x20);
            step_by(m, 1);
            sound(m, 0x59);
            c2(m, 0x800d_a49c, 0x1c, 0);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 7);
            sound(m, 0x3b);
        }
    });
}

/// 800D061C (no capture reaches it): formats the chosen slot (800D5E74);
/// on a result, dialog 0xE step 3. Returns 0, or the menu block after a
/// result (the register the original leaves).
pub fn format(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_5e74, [slot, 0, 0, 0]);
        if result == 0 {
            return 0;
        }
        let block = menu(m);
        m.sh(off(block, 0x4c), 0xe);
        m.sh(off(block, 0x4e), 3);
        block
    })
}

/// 800D0668: a card with save room in the chosen slot (state 4) goes on
/// (dialog step up); otherwise dialog 0xF and icon 1 (0x1A).
pub fn room_check(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if chosen_state(m) == 4 {
            step_by(m, 1);
            return;
        }
        let block = menu(m);
        m.sh(off(block, 0x4c), 0xf);
        c2(m, 0x800d_a9a8, 1, 0x1a);
    });
}

/// 800D06D4: the save file check (800D64C4 of the chosen slot): -1 waits;
/// 0 or 1 opens the next prompt (step +1, icon 1 (0x22),
/// 800DA49C(0x1E), the text 0x20016); 2 and up skip it (step +2).
pub fn file_check(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_64c4, [slot, 0, 0, 0]);
        if result == u32::MAX {
            return;
        }
        if lt(result, 2) {
            step_by(m, 1);
            c2(m, 0x800d_a9a8, 1, 0x22);
            c2(m, 0x800d_a49c, 0x1e, 0x22);
            c2(m, 0x800d_a828, 0x0002_0016, 0x22);
        } else {
            step_by(m, 2);
        }
    });
}

/// The card error prompt: icon 1 (`icon`), the text `text` and
/// 800DA49C(0x20), in the original order for `order` (true: text first).
fn error_prompt(m: &mut Machine, icon: u32, text: u32, text_first: bool) {
    c2(m, 0x800d_a9a8, 1, icon);
    if text_first {
        c2(m, 0x800d_a828, text, icon);
        c2(m, 0x800d_a49c, 0x20, icon);
    } else {
        c2(m, 0x800d_a49c, 0x20, icon);
        c2(m, 0x800d_a828, text, icon);
    }
}

/// 800D0848: the card operation 800D5EC4 of the chosen slot: 1 goes on
/// (step +1); -1 is an error (step +3, icon 1 (0x23), 800DA49C(0x20),
/// the text 0x20016).
pub fn card_operation(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_5ec4, [slot, 0, 0, 0]);
        if result == 1 {
            step_by(m, 1);
        } else if result == u32::MAX {
            step_by(m, 3);
            error_prompt(m, 0x23, 0x0002_0016, false);
        }
    });
}

/// 800D08DC: the write step 800D6D78 (chosen slot, 1): -1 is an error
/// (step +2, the error prompt 0x23/0x20016); 1 goes on (step +1).
pub fn write_step(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_6d78, [slot, 1, 0, 0]);
        if result == u32::MAX {
            step_by(m, 2);
            error_prompt(m, 0x23, 0x0002_0016, false);
        }
        if result == 1 {
            step_by(m, 1);
        }
    });
}

/// 800D0978: the write step 800D6F04 (chosen slot, -1, 1): -1 is an error
/// (step +1, the error prompt 0x23/0x20016); 1 finishes (dialog 0x10).
pub fn write_finish(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_6f04, [slot, u32::MAX, 1, 0]);
        if result == u32::MAX {
            step_by(m, 1);
            error_prompt(m, 0x23, 0x0002_0016, false);
        }
        if result == 1 {
            dialog_to(m, 0x10);
        }
    });
}

/// 800D0AEC (no capture reaches it): dialog step up.
pub fn next_step(m: &mut Machine) {
    step_by(m, 1);
}

/// 800D0B0C (no capture reaches it): the card step 800D6604 (chosen
/// slot, 1): -1 an error (step +2, icon 1 (0x23)), 2 another (step +2,
/// icon 1 (0x25)), each with the text 0x20056, 800DA49C(0x20) and icon
/// 4 (0x15); 1 goes on (step +1).
pub fn create_step(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_6604, [slot, 1, 0, 0]);
        for (value, icon) in [(u32::MAX, 0x23), (2, 0x25)] {
            if result == value {
                step_by(m, 2);
                error_prompt(m, icon, 0x0002_0056, true);
                c2(m, 0x800d_a9a8, 4, 0x15);
            }
        }
        if result == 1 {
            step_by(m, 1);
        }
    });
}

/// 800D0BFC (no capture reaches it): the card step 800D6814 (chosen
/// slot, 1): -1 an error (step +1, icon 1 (0x23)), 2 another (step +1,
/// icon 1 (0x25), panel 6), each with the text 0x20056, 800DA49C(0x20)
/// and icon 4 (0x15); 1 finishes (dialog 0x10).
pub fn create_finish(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_6814, [slot, 1, 0, 0]);
        if result == u32::MAX {
            step_by(m, 1);
            error_prompt(m, 0x23, 0x0002_0056, true);
            c2(m, 0x800d_a9a8, 4, 0x15);
        }
        if result == 2 {
            step_by(m, 1);
            c2(m, 0x800d_a9a8, 1, 0x25);
            c2(m, 0x800d_a77c, 6, 0x25);
            c2(m, 0x800d_a828, 0x0002_0056, 0x25);
            c2(m, 0x800d_a49c, 0x20, 0x25);
            c2(m, 0x800d_a9a8, 4, 0x15);
        }
        if result == 1 {
            dialog_to(m, 0x10);
        }
    });
}

/// 800D0DC8 (file choice, open): dialog step up, countdown cleared;
/// panel 0x14, the text 0x100056, icons 1 (0x24) and 4 (0x16), the file
/// list (800DC028(1)), +0xD2 = 0, the choice box (800DBF6C at
/// (0x40, 0x32)), 800DA49C(0x1F).
pub fn file_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x48), 0);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0x14, 0);
        c2(m, 0x800d_a828, 0x0010_0056, 0);
        c2(m, 0x800d_a9a8, 1, 0x24);
        c2(m, 0x800d_a9a8, 4, 0x16);
        c2(m, 0x800d_c028, 1, 0x16);
        let block = menu(m);
        m.sb(off(block, 0xd2), 0);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0x1f, 0x32);
    });
}

/// The file choice's icons 6..8 (the file +0x5D on).
fn file_icons(m: &mut Machine) {
    for k in 0..3u32 {
        let file = menu_lbu(m, 0x5d);
        c2(m, 0x800d_a9a8, 6 + k, u32::from(file != k));
    }
}

/// 800D0E54 (file choice): a lost card gives dialog 9 (the list hidden);
/// pad +0x130 bits 1/0 move the file (+0x5D, modulo 3; sound 0x3D);
/// icons 6..8; START takes the file (800D5A38: an existing save goes to
/// dialog 0x11, an empty file to 0x12 with +0xD2 = 1; sound 0x59),
/// cross goes back (dialog 0xB, the list hidden, sound 0x3B).
pub fn file_choice(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if lost(chosen_state(m)) {
            dialog_to(m, 9);
            c2(m, 0x800d_c028, 0, 0);
            return;
        }
        if ctx_lw(m, 0x130) & 2 != 0 {
            let block = menu(m);
            let file = lbu(m, off(block, 0x5d));
            m.sb(off(block, 0x5d), if file < 2 { file + 1 } else { 0 } as u8);
            sound(m, 0x3d);
        }
        if ctx_lw(m, 0x130) & 1 != 0 {
            let block = menu(m);
            let file = lbu(m, off(block, 0x5d));
            m.sb(off(block, 0x5d), if file != 0 { file - 1 } else { 2 } as u8);
            sound(m, 0x3d);
        }
        file_icons(m);
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x40 != 0 {
            let file = menu_lbu(m, 0x5d);
            if call(m, 0x800d_5a38, [file, 0, 0, 0]) != 0 {
                let block = menu(m);
                m.sh(off(block, 0x4c), 0x11);
            } else {
                let block = menu(m);
                m.sh(off(block, 0x4c), 0x12);
                m.sb(off(block, 0xd2), 1);
            }
            let block = menu(m);
            m.sh(off(block, 0x4e), 0);
            sound(m, 0x59);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 0xb);
            c2(m, 0x800d_c028, 0, 0);
            sound(m, 0x3b);
        }
    });
}

/// 800D1080 (overwrite prompt, open): dialog step up; panel 0x15, the
/// text 0x200056, icons 1 (0x26) and 4 (0x18), the file's cursor
/// (6 + file, 1, 0, 0xA), the list (800DC028(2)), 800DA49C(0x21).
pub fn overwrite_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0x15, 0);
        c2(m, 0x800d_a828, 0x0020_0056, 0);
        c2(m, 0x800d_a9a8, 1, 0x26);
        c2(m, 0x800d_a9a8, 4, 0x18);
        let file = menu_lbu(m, 0x5d);
        c4(m, 0x800d_a838, file.wrapping_add(6), 1, 0, 0xa);
        c2(m, 0x800d_c028, 2, 1);
        c2(m, 0x800d_a49c, 0x21, 1);
    });
}

/// 800D1108 (no capture reaches it; overwrite prompt): a lost card gives
/// dialog 9 (the list hidden); icons 6..8; START overwrites (dialog
/// 0x12, the file's cursor off, sound 0x59), cross goes back (dialog
/// 0x10, cursor off, sound 0x3B). Counts +0x48.
pub fn overwrite(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if lost(chosen_state(m)) {
            dialog_to(m, 9);
            c2(m, 0x800d_c028, 0, 0);
            return;
        }
        file_icons(m);
        let pressed = ctx_lw(m, 0x12c);
        for (bit, dialog, id) in [(0x40u32, 0x12u32, 0x59u32), (0x20, 0x10, 0x3b)] {
            if pressed & bit != 0 {
                let file = menu_lbu(m, 0x5d);
                dialog_to(m, dialog);
                c4(m, 0x800d_a838, file.wrapping_add(6), 0, 0, 0);
                sound(m, id);
                break;
            }
        }
        count_up(m, 0x48);
    });
}

/// 800D12A4 (saving, open): dialog step up; icons 1 (0x27) and 4 (0x18);
/// the context's +0x968 takes the frame counter and the account name
/// comes from the record.
pub fn saving_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        step_by(m, 1);
        c2(m, 0x800d_a9a8, 1, 0x27);
        c2(m, 0x800d_a9a8, 4, 0x18);
        let frame = frame_counter(m);
        let ctx = context(m);
        m.sw(off(ctx, 0x968), frame);
        name_from_record(m);
    });
}

/// 800D1300: writes the file header (800D6F04: chosen slot and file, 1):
/// -1 is an error (step +2, icon 1 (0x28), the text 0x20016,
/// 800DA49C(0x23), the list hidden); 1 packs the record (the account
/// name from the record, [`pack_record`](super::ov1_p1_card::pack_record))
/// and goes on (step +1).
pub fn write_header(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let file = menu_lbu(m, 0x5d);
        let result = call(m, 0x800d_6f04, [slot, file, 1, 0]);
        if result == u32::MAX {
            step_by(m, 2);
            c2(m, 0x800d_a9a8, 1, 0x28);
            c2(m, 0x800d_a828, 0x0002_0016, 0x28);
            c2(m, 0x800d_a49c, 0x23, 0x28);
            c2(m, 0x800d_c028, 0, 0x28);
        }
        if result == 1 {
            name_from_record(m);
            super::ov1_p1_card::pack_record(m);
            step_by(m, 1);
        }
    });
}

/// 800D13B8: writes the record (800D713C: chosen slot and file, 6, 1):
/// -1 is an error (step +1, icon 1 (0x28), the text 0x20056,
/// 800DA49C(0x23), the list hidden); 1 finishes (+0x88 = 0x78 frames,
/// step +2, the save record reset (800DA1B8), the context's +0x954 = 2,
/// +0x10 cleared, 800DA49C(0x24)).
pub fn write_record(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let file = menu_lbu(m, 0x5d);
        let result = call(m, 0x800d_713c, [slot, file, 6, 1]);
        if result == u32::MAX {
            step_by(m, 1);
            c2(m, 0x800d_a9a8, 1, 0x28);
            c2(m, 0x800d_a828, 0x0002_0056, 0x28);
            c2(m, 0x800d_a49c, 0x23, 0x28);
            c2(m, 0x800d_c028, 0, 0x28);
        }
        if result == 1 {
            let block = menu(m);
            let step = lhu(m, off(block, 0x4e));
            m.sw(off(block, 0x88), 0x78);
            m.sh(off(block, 0x4e), step.wrapping_add(2) as u16);
            call(m, 0x800d_a1b8, [0; 4]);
            let ctx = context(m);
            let block = menu(m);
            m.sh(off(ctx, 0x954), 2);
            m.sh(off(block, 0x10), 0);
            c2(m, 0x800d_a49c, 0x24, 0);
        }
    });
}

/// 800D1488 (no capture reaches it): a lost card gives dialog 9; else
/// icon 1 (0x28); circle finishes (dialog 0x13, the save record reset,
/// the context's +0x954 = 2, +0x10 cleared), cross gives dialog 0xB.
pub fn write_failed(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        if lost(chosen_state(m)) {
            dialog_to(m, 9);
            return;
        }
        c2(m, 0x800d_a9a8, 1, 0x28);
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            dialog_to(m, 0x13);
            call(m, 0x800d_a1b8, [0; 4]);
            let ctx = context(m);
            let block = menu(m);
            m.sh(off(ctx, 0x954), 2);
            m.sh(off(block, 0x10), 0);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 0xb);
        }
    });
}

/// 800D1554 ("save complete"): icon 1 (0x2A); counts +0x88 down; when it
/// has run out (negative) or circle/cross is pressed, dialog 0x13.
pub fn saved(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        c2(m, 0x800d_a9a8, 1, 0x2a);
        let block = menu(m);
        let count = m.lw(off(block, 0x88));
        if lt(count, 0) || ctx_lw(m, 0x12c) & 0x30 != 0 {
            m.sh(off(block, 0x4c), 0x13);
            m.sh(off(block, 0x4e), 0);
        } else {
            m.sw(off(block, 0x88), count.wrapping_sub(1));
        }
    });
}

/// 800D15C4: dialog step up, +0xD4 cleared, the context's +0x38C = 1;
/// the YES/NO box (800DBF6C at (0x6F, 0x96), 0, 1).
pub fn continue_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        let step = lhu(m, off(block, 0x4e));
        m.sw(off(block, 0xd4), 0);
        m.sh(off(block, 0x4e), step.wrapping_add(1) as u16);
        let ctx = context(m);
        m.sh(off(ctx, 0x38c), 1);
        c4(m, 0x800d_bf6c, 0x6f, 0x96, 0, 1);
    });
}

/// 800D1618 (the disc check): the disc number (8002B504): 0 shows icon 1
/// (0x49); disc 1 or 3 continues to screen 6; 2, 4 and -1 (no disc)
/// show the prompt (icon 1 (+0xD4 + 0x46)), stop the CD (8002B428),
/// step up and 800DA49C(0x25).
pub fn disc_check(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let disc = call(m, 0x8002_b504, [0; 4]);
        if disc == 0 {
            c2(m, 0x800d_a9a8, 1, 0x49);
        }
        if disc == 3 || disc == 1 {
            let block = menu(m);
            m.sh(off(block, 0xc), 6);
            m.sh(off(block, 0xe), 0);
            return;
        }
        if disc == 4 || disc == u32::MAX || disc == 2 {
            let icon = menu_lw_d4(m).wrapping_add(0x46);
            c2(m, 0x800d_a9a8, 1, icon);
            call(m, 0x8002_b428, [0; 4]);
            step_by(m, 1);
            c2(m, 0x800d_a49c, 0x25, icon);
        }
    });
}

fn menu_lw_d4(m: &mut Machine) -> u32 {
    let block = menu(m);
    m.lw(off(block, 0xd4))
}

/// 800D16D4 (no capture reaches it): the text 0x400036, icon 4 (4), the
/// list hidden; once the CD reports 3 (8002B528), step up.
pub fn disc_wait(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        c2(m, 0x800d_a828, 0x0040_0036, 0);
        c2(m, 0x800d_a9a8, 4, 4);
        c2(m, 0x800d_c028, 0, 4);
        if call(m, 0x8002_b528, [0; 4]) == 3 {
            step_by(m, 1);
        }
    });
}

/// 800D1738 (no capture reaches it): when the CD reports 1 (8002B528):
/// icon 1 (0x49), step -2, 800DA49C(0x26), +0xD4 = 0x1F.
pub fn disc_retry(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if call(m, 0x8002_b528, [0; 4]) != 1 {
            return;
        }
        c2(m, 0x800d_a9a8, 1, 0x49);
        step_by(m, 0xffff_fffe);
        c2(m, 0x800d_a49c, 0x26, 0x49);
        let block = menu(m);
        m.sw(off(block, 0xd4), 0x1f);
    });
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    let void = |m: &mut Machine, f: fn(&mut Machine)| {
        f(m);
        0
    };
    Some(match target {
        0x800c_fc38 => void(m, ask_open),
        0x800c_fc94 => void(m, ask),
        0x800c_fd28 => void(m, checking_open),
        0x800c_fd60 => void(m, wait_probes),
        0x800c_fdcc => void(m, no_card_open),
        0x800c_fea4 => void(m, no_card),
        0x800c_ff90 => void(m, slot_open),
        0x800d_0074 => void(m, slot_choice),
        0x800d_02dc => void(m, card_ready),
        0x800d_0384 | 0x800d_076c | 0x800d_0a10 | 0x800d_0cec => void(m, slot_confirm),
        0x800d_0460 => void(m, unformatted_open),
        0x800d_0548 => void(m, unformatted),
        0x800d_061c => format(m),
        0x800d_0668 => void(m, room_check),
        0x800d_06d4 => void(m, file_check),
        0x800d_0848 => void(m, card_operation),
        0x800d_08dc => void(m, write_step),
        0x800d_0978 => void(m, write_finish),
        0x800d_0aec => void(m, next_step),
        0x800d_0b0c => void(m, create_step),
        0x800d_0bfc => void(m, create_finish),
        0x800d_0dc8 => void(m, file_open),
        0x800d_0e54 => void(m, file_choice),
        0x800d_1080 => void(m, overwrite_open),
        0x800d_1108 => void(m, overwrite),
        0x800d_12a4 => void(m, saving_open),
        0x800d_1300 => void(m, write_header),
        0x800d_13b8 => void(m, write_record),
        0x800d_1488 => void(m, write_failed),
        0x800d_1554 => void(m, saved),
        0x800d_15c4 => void(m, continue_open),
        0x800d_1618 => void(m, disc_check),
        0x800d_16d4 => void(m, disc_wait),
        0x800d_1738 => void(m, disc_retry),
        _ => return None,
    })
}
