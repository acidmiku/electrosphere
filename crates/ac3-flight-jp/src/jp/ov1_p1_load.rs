//! JP overlay 1, part 1: the account screen's frame (800D1798,
//! 800D181C), the Download (load) screen and its dialogs
//! (800D1A5C..800D3B3C).
//!
//! Both screens run a dialog machine: the menu block's +0x4C picks a step
//! list of a dialog table (0x800DD598 for the account screen, 0x800DD66C
//! for Download) and +0x4E the step, called with the menu block. The
//! chosen slot is +0x5A, the file +0x5D, the account entry +0x5F.
use super::ov1_p1::{
    MODULE, call, context, ctx_lw, framed, lbu, lh, lhu, lt, menu, menu_lbu, off, sra,
};
use super::ov1_p1_card::{record_from_slot, slot_state, unpack_record};
use super::ov1_p1_screens::{c2, c4, dialog_step_up, dialog_to, sound};
use crate::{
    arena,
    common::loaded_module,
    machine::{Fault, Machine},
};

/// `0x800DD598`: the account screen's dialog table.
pub const ACCOUNT_DIALOGS: u32 = 0x800d_d598;
/// `0x800DD66C`: the Download screen's dialog table.
pub const LOAD_DIALOGS: u32 = 0x800d_d66c;
/// `0x800CC00C`: the account screen's backdrop jump table (11 entries by
/// dialog state - 1).
const BACKDROPS: u32 = 0x800c_c00c;
/// `[0x800DDA30]`: the 4-byte name buffer of the account screen.
const NAME: u32 = 0x800d_da30;
/// `0x800DD8D4`: the disc each account entry needs (bytes).
const ENTRY_DISC: u32 = 0x800d_d8d4;
/// `0x800DD5F0`: the Download path's difficulty choice (halfword).
const LEVEL: u32 = 0x800d_d5f0;

fn step_by(m: &mut Machine, n: u32) {
    let block = menu(m);
    let step = lhu(m, off(block, 0x4e)).wrapping_add(n);
    m.sh(off(block, 0x4e), step as u16);
}
fn chosen_state(m: &mut Machine) -> u32 {
    let slot = menu_lbu(m, 0x5a);
    slot_state(m, slot)
}
const fn lost(state: u32) -> bool {
    state == 2 || state == 6
}
fn set_menu_byte(m: &mut Machine, offset: u32, value: u32) {
    let block = menu(m);
    m.sb(off(block, offset), value as u8);
}

/// The scrolling backdrop (800D9A08 at (x, y) with the phase of the
/// context's frame counter +0x20 modulo 64, into the packet pointer).
fn backdrop(m: &mut Machine, x: u32, y: u32) {
    let counter = ctx_lw(m, 0x20);
    let rounded = if lt(counter, 0) {
        counter.wrapping_add(0x3f)
    } else {
        counter
    };
    let phase = counter.wrapping_sub(sra(rounded, 6) << 6) << 6;
    let block = menu(m);
    let packet = m.lw(off(block, 8));
    let next = c4(m, 0x800d_9a08, x, y, phase, packet);
    let block = menu(m);
    m.sw(off(block, 8), next);
}

/// The current dialog step of `table` (`jalr` with a0 the menu block).
fn dialog_step(m: &mut Machine, table: u32) {
    let block = menu(m);
    let state = lh(m, off(block, 0x4c)) << 2;
    let step = lh(m, off(block, 0x4e)) << 2;
    let steps = m.lw(state.wrapping_add(table));
    let routine = m.lw(step.wrapping_add(steps));
    call(m, routine, [block, 0, 0, 0]);
}

/// The screens' common tail after the file list (800DC098): icons 0x11
/// (+0xD0), 0x10 (2 with +0xD1 set, else the slot), 3 (the difficulty,
/// context +0x394) and 5 (card flag, context +0x124 bit 4); the menu
/// stream (+0x7C) once the CD queue is idle.
fn status_icons(m: &mut Machine) {
    let v = menu_lbu(m, 0xd0);
    c2(m, 0x800d_a9a8, 0x11, v);
    let block = menu(m);
    let icon = if lbu(m, off(block, 0xd1)) == 1 {
        2
    } else {
        lbu(m, off(block, 0x5a))
    };
    c2(m, 0x800d_a9a8, 0x10, icon);
    let level = ctx_lw(m, 0x394);
    c2(m, 0x800d_a9a8, 3, level);
    let card = u32::from(ctx_lw(m, 0x124) & 0x10 != 0);
    c2(m, 0x800d_a9a8, 5, card);
    if call(m, 0x8002_c580, [1, 0, 0, 0]) == 0 {
        let request = off(menu(m), 0x7c);
        call(m, 0x8002_c8d8, [request, 1, 0, 0]);
    }
}

// ---- the account screen (screen 3) --------------------------------------------

/// 800D1798 (account screen, step 0): step 1, countdown cleared; a 4-byte
/// name buffer from the arena at [`NAME`] (emptied); +0xD2 and the
/// dialog cleared; 800D51FC, 800D5290, 800D5548.
pub fn account_screen_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x48), 0);
        m.sh(off(block, 0x10), 0);
        m.sh(off(block, 0xe), 1);
        let name = arena::alloc(m, 4);
        m.sw(NAME, name);
        m.sb(name, 0);
        let name = m.lw(NAME);
        m.sb(off(name, 1), 0);
        set_menu_byte(m, 0xd2, 0);
        dialog_to(m, 0);
        call(m, 0x800d_51fc, [0; 4]);
        call(m, 0x800d_5290, [0; 4]);
        call(m, 0x800d_5548, [0; 4]);
    });
}

/// 800D181C (account screen, step 1): without a disc number (8002B504 =
/// 0) the pad words are cleared; the backdrop of the dialog state (the
/// jump table [`BACKDROPS`]); the dialog step ([`ACCOUNT_DIALOGS`]);
/// 800DBFE4, the file list (800DC098(1, +0xD2)) and the status icons.
pub fn account_screen(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if call(m, 0x8002_b504, [0; 4]) == 0 {
            let ctx = context(m);
            m.sw(off(ctx, 0x12c), 0);
            m.sw(off(ctx, 0x128), 0);
        }
        let index = sext16_index(menu_lhu(m, 0x4c).wrapping_sub(1));
        if index < 0xb {
            match m.lw(BACKDROPS.wrapping_add(index << 2)) {
                0x800d_1890 => backdrop(m, 0xf9, 0x169),
                0x800d_18c0 => backdrop(m, 0x46, 0x19a),
                0x800d_18f0 => backdrop(m, 0x3a, 0x172),
                0x800d_1944 => {}
                address => m.raise(Fault::Untranslated { address }),
            }
        }
        dialog_step(m, ACCOUNT_DIALOGS);
        call(m, 0x800d_bfe4, [0; 4]);
        let list = menu_lbu(m, 0xd2);
        c2(m, 0x800d_c098, 1, list);
        status_icons(m);
    });
}

fn menu_lhu(m: &mut Machine, offset: u32) -> u32 {
    let block = menu(m);
    lhu(m, off(block, offset))
}
/// `sll 16; sra 16` of a halfword difference (the switch index, compared
/// unsigned).
const fn sext16_index(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}

// ---- Download: the card -------------------------------------------------------

/// 800D1A5C (Download, open): cursors 2, 4, 9; step up, icon 1 (7), icon
/// 4 (5).
pub fn load_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        c4(m, 0x800d_a838, 2, 1, 0, 0x3c);
        c4(m, 0x800d_a838, 4, 1, 0, 0x3c);
        c4(m, 0x800d_a838, 9, 1, 0, 0x3c);
        step_by(m, 1);
        c2(m, 0x800d_a9a8, 1, 7);
        c2(m, 0x800d_a9a8, 4, 5);
    });
}

/// 800D1ADC (Download): once both slots are probed (neither state 0),
/// dialog 1.
pub fn load_wait_probes(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if first != 0 && second != 0 {
            dialog_to(m, 1);
        }
    });
}

/// 800D1B2C (Download): with no card in either slot the "no card" prompt
/// (panel 8, the text 0x13E, cursor 4, step up, icons 1 (2), 6 and 7 off
/// (2), 800DA49C(3), +0xD0 = 1, icon 4 (5)); otherwise dialog 2.
pub fn load_no_card_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if !(lost(first) && lost(second)) {
            dialog_to(m, 2);
            return;
        }
        c2(m, 0x800d_a77c, 8, 0);
        c2(m, 0x800d_a828, 0x13e, 0);
        c4(m, 0x800d_a838, 4, 1, 0, 0x3c);
        step_by(m, 1);
        c2(m, 0x800d_a9a8, 1, 2);
        c2(m, 0x800d_a9a8, 6, 2);
        c2(m, 0x800d_a9a8, 7, 2);
        c2(m, 0x800d_a49c, 3, 2);
        set_menu_byte(m, 0xd0, 1);
        c2(m, 0x800d_a9a8, 4, 5);
    });
}

/// 800D1C18 (no capture reaches it; "no card"): a card back makes it
/// dialog 0 (+0xD0 cleared, icons 1 (7) and 4 (5)); cross returns to the
/// link-mode menu (screen 2, the text 0x9E, sound 0x59, +0xD0 cleared).
pub fn load_no_card(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if !(lost(first) && lost(second)) {
            dialog_to(m, 0);
            set_menu_byte(m, 0xd0, 0);
            c2(m, 0x800d_a9a8, 1, 7);
            c2(m, 0x800d_a9a8, 4, 5);
            return;
        }
        if ctx_lw(m, 0x12c) & 0x20 != 0 {
            let block = menu(m);
            m.sh(off(block, 0xc), 2);
            m.sh(off(block, 0xe), 0);
            c2(m, 0x800d_a828, 0x9e, 0);
            sound(m, 0x59);
            set_menu_byte(m, 0xd0, 0);
        }
    });
}

/// 800D1CF0 (Download slot choice, open): step up; panel 8, the text
/// 0x13E, cursor 4, icons 1 (3) and 4 (2), 800DA49C(4), +0xD1 = 0; the
/// slot preselection for state 5 (slot 0 when only it has one, slot 1
/// when only it has one).
pub fn load_slot_open(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 8, 0);
        c2(m, 0x800d_a828, 0x13e, 0);
        c4(m, 0x800d_a838, 4, 1, 0, 0x3c);
        c2(m, 0x800d_a9a8, 1, 3);
        c2(m, 0x800d_a9a8, 4, 2);
        c2(m, 0x800d_a49c, 4, 2);
        set_menu_byte(m, 0xd1, 0);
        if first == 5 {
            if second != 5 {
                set_menu_byte(m, 0x5a, 0);
            }
        } else if second == 5 {
            set_menu_byte(m, 0x5a, 1);
        }
    });
}

/// 800D1DCC (Download slot choice): with no card in either slot back to
/// dialog 1 (cursors 2 and 1, +0xD1 = 1); pad +0x130 bits 0/1 switch the
/// slot, a slot without a usable card is skipped (sound 0x3D on a
/// change); icons 6 and 7 (2 for unusable slots); circle goes on (dialog
/// 3, sound 0x59, icon 4 (5)), cross returns to the link-mode menu
/// (screen 2, the text 0xBE, sound 0x3B).
pub fn load_slot_choice(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let first = slot_state(m, 0);
        let second = slot_state(m, 1);
        if lost(first) && lost(second) {
            dialog_to(m, 1);
            c4(m, 0x800d_a838, 2, 1, 0, 0x14);
            c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
            set_menu_byte(m, 0xd1, 1);
            return;
        }
        let ctx = context(m);
        let block = menu(m);
        let held = m.lw(off(ctx, 0x130));
        let old = lbu(m, off(block, 0x5a));
        if held & 3 != 0 {
            m.sb(off(block, 0x5a), (old ^ 1) as u8);
        }
        let unusable = |s: u32| s == 0 || lost(s);
        if unusable(first) {
            set_menu_byte(m, 0x5a, 1);
        }
        if unusable(second) {
            set_menu_byte(m, 0x5a, 0);
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
        if unusable(first) {
            c2(m, 0x800d_a9a8, 6, 2);
        }
        if unusable(second) {
            c2(m, 0x800d_a9a8, 7, 2);
        }
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            dialog_to(m, 3);
            sound(m, 0x59);
            c2(m, 0x800d_a9a8, 4, 5);
        } else if pressed & 0x20 != 0 {
            let block = menu(m);
            m.sh(off(block, 0xc), 2);
            m.sh(off(block, 0xe), 0);
            c2(m, 0x800d_a828, 0xbe, 0);
            sound(m, 0x3b);
        }
    });
}

/// 800D202C (Download): icon 1 (7); a ready card (state 1) opens the next
/// prompt (step up, icon 1 (4), 800DA49C(6), +0xD0 = 1); otherwise
/// dialog 4.
pub fn load_card_ready(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let state = chosen_state(m);
        c2(m, 0x800d_a9a8, 1, 7);
        if state != 1 {
            let block = menu(m);
            m.sh(off(block, 0x4c), 4);
            return;
        }
        step_by(m, 1);
        c2(m, 0x800d_a9a8, 1, 4);
        c2(m, 0x800d_a49c, 6, 4);
        set_menu_byte(m, 0xd0, state);
    });
}

/// 800D20BC and 800D21D0 (two copies; no capture reaches them): a lost
/// card returns to dialog 0; cross too (sound 0x59); +0xD0 cleared.
pub fn load_card_lost(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if lost(chosen_state(m)) {
            dialog_to(m, 0);
            set_menu_byte(m, 0xd0, 0);
        } else if ctx_lw(m, 0x12c) & 0x20 != 0 {
            dialog_to(m, 0);
            sound(m, 0x59);
            set_menu_byte(m, 0xd0, 0);
        }
    });
}

/// 800D22E4 (no capture reaches it): a lost card returns to dialog 0;
/// cross to dialog 2 (sound 0x59); +0xD0 cleared.
pub fn load_card_back(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if lost(chosen_state(m)) {
            dialog_to(m, 0);
            set_menu_byte(m, 0xd0, 0);
        } else if ctx_lw(m, 0x12c) & 0x20 != 0 {
            dialog_to(m, 2);
            sound(m, 0x59);
            set_menu_byte(m, 0xd0, 0);
        }
    });
}

/// 800D255C (no capture reaches it): +0xD0 = 1, then as [`load_card_back`].
pub fn load_card_wait(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let state = chosen_state(m);
        set_menu_byte(m, 0xd0, 1);
        if lost(state) {
            dialog_to(m, 0);
            set_menu_byte(m, 0xd0, 0);
        } else if ctx_lw(m, 0x12c) & 0x20 != 0 {
            dialog_to(m, 2);
            sound(m, 0x59);
            set_menu_byte(m, 0xd0, 0);
        }
    });
}

/// 800D2154 (Download): an unformatted card (state 3) shows its prompt
/// (step up, icon 1 (5), 800DA49C(7), +0xD0 = 1); otherwise dialog 5.
pub fn load_unformatted(m: &mut Machine) {
    state_prompt(m, 3, 5, 7, 5);
}

/// 800D2268 (Download): a card without the game's file (state 4) shows
/// its prompt (step up, icon 1 (6), 800DA49C(8), +0xD0 = 1); otherwise
/// dialog 6.
pub fn load_no_file(m: &mut Machine) {
    state_prompt(m, 4, 6, 8, 6);
}

fn state_prompt(m: &mut Machine, state: u32, icon: u32, text: u32, otherwise: u32) {
    framed(m, 0x18, |m, _| {
        if chosen_state(m) != state {
            let block = menu(m);
            m.sh(off(block, 0x4c), otherwise as u16);
            return;
        }
        step_by(m, 1);
        c2(m, 0x800d_a9a8, 1, icon);
        c2(m, 0x800d_a49c, text, icon);
        set_menu_byte(m, 0xd0, 1);
    });
}

/// 800D237C (Download): step up, icon 1 (7); 800D51FC, 800D5290,
/// 800D5548.
pub fn load_restart(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        step_by(m, 1);
        c2(m, 0x800d_a9a8, 1, 7);
        call(m, 0x800d_51fc, [0; 4]);
        call(m, 0x800d_5290, [0; 4]);
        call(m, 0x800d_5548, [0; 4]);
    });
}

/// 800D23CC (Download): the card step 800D6604 (slot, 1): -1 an error
/// (step +2, icon 1 (0xE)), 2 another (step +2, icon 1 (8)), each with
/// 800DA49C(6); 1 goes on (step +1).
pub fn load_card_step(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_6604, [slot, 1, 0, 0]);
        for (value, icon) in [(u32::MAX, 0xe), (2, 8)] {
            if result == value {
                step_by(m, 2);
                c2(m, 0x800d_a9a8, 1, icon);
                c2(m, 0x800d_a49c, 6, icon);
            }
        }
        if result == 1 {
            step_by(m, 1);
        }
    });
}

/// 800D248C (Download): the card step 800D6814 (slot, 1): -1 an error
/// (step +1, icon 1 (0xE)), 2 another (step +1, icon 1 (8)), each with
/// 800DA49C(6) and +0xD0 = 1; 1 goes on (dialog 7).
pub fn load_card_finish(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let result = call(m, 0x800d_6814, [slot, 1, 0, 0]);
        for (value, icon) in [(u32::MAX, 0xe), (2, 8)] {
            if result == value {
                step_by(m, 1);
                c2(m, 0x800d_a9a8, 1, icon);
                c2(m, 0x800d_a49c, 6, icon);
                set_menu_byte(m, 0xd0, 1);
            }
        }
        if result == 1 {
            dialog_to(m, 7);
        }
    });
}

// ---- Download: the file -------------------------------------------------------

/// 800D2600 (Download file choice, open): step up, countdown and +0xD3
/// cleared; the text 0xBE; cursors 1, 2, 4, 9 off and 1, 5, 0xA on; the
/// file list (800DC028(1)); icons 1 (9) and 4 (3); 800DA49C(9).
pub fn load_file_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x48), 0);
        m.sb(off(block, 0xd3), 0);
        dialog_step_up(m, block);
        c2(m, 0x800d_a828, 0xbe, 0);
        c4(m, 0x800d_a838, 1, 1, 0, 0x14);
        for cursor in [2, 4, 9] {
            c4(m, 0x800d_a838, cursor, 3, u32::MAX, u32::MAX);
        }
        for cursor in [1, 5, 0xa] {
            c4(m, 0x800d_a838, cursor, 1, 0, 0x3c);
        }
        c2(m, 0x800d_c028, 1, 0);
        c2(m, 0x800d_a9a8, 1, 9);
        c2(m, 0x800d_a9a8, 4, 3);
        c2(m, 0x800d_a49c, 9, 3);
    });
}

/// 800D26F0 (Download file choice): a lost card returns to dialog 0 (the
/// list hidden); pad +0x130 bits 1/0 move the file (+0x5D, modulo 3;
/// sound 0x3D); icons 6..8; circle takes a file that holds a save (bit
/// file of 800D5C80's mask: dialog 8, sound 0x59, icon 4 (5); else sound
/// 0x30); cross goes back (dialog 2, cursors 2 and 1, the list hidden,
/// sound 0x3B).
pub fn load_file_choice(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let state = chosen_state(m);
        let saves = call(m, 0x800d_5c80, [0; 4]);
        if lost(state) {
            dialog_to(m, 0);
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
        for k in 0..3u32 {
            let file = menu_lbu(m, 0x5d);
            c2(m, 0x800d_a9a8, 6 + k, u32::from(file != k));
        }
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            let file = menu_lbu(m, 0x5d);
            if sra(saves, file & 0x1f) & 1 != 0 {
                dialog_to(m, 8);
                sound(m, 0x59);
                c2(m, 0x800d_a9a8, 4, 5);
            } else {
                sound(m, 0x30);
            }
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 2);
            c4(m, 0x800d_a838, 2, 1, 0, 0x14);
            c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
            c2(m, 0x800d_c028, 0, 0);
            sound(m, 0x3b);
        }
    });
}

/// 800D2954 (Download): step up, icon 1 (0xC). Returns 800DA49C(0xC)'s
/// result.
pub fn load_reading_open(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        step_by(m, 1);
        c2(m, 0x800d_a9a8, 1, 0xc);
        c2(m, 0x800d_a49c, 0xc, 0xc)
    })
}

/// 800D2994 (Download): the chosen file's save buffer into the record
/// ([`record_from_slot`]) and the record into the context
/// ([`unpack_record`], the account entry to +0x5F); the context's +0x964
/// bit 0 cleared; step up.
pub fn load_record(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let file = menu_lbu(m, 0x5d);
        record_from_slot(m, file);
        let entry = off(menu(m), 0x5f);
        unpack_record(m, entry);
        let ctx = context(m);
        let flags = m.lw(off(ctx, 0x964));
        let block = menu(m);
        m.sw(off(ctx, 0x964), flags & !1);
        let step = lhu(m, off(block, 0x4e));
        m.sh(off(block, 0x4e), step.wrapping_add(1) as u16);
    });
}

/// 800D29FC (Download): reads the file (800D6AB4: slot, file, 1): -1 an
/// error (step +1, icon 1 (0xE), 800DA49C(0xD), +0xD0 = 1, the list
/// hidden), 2 another (step +1, icon 1 (8), 800DA49C(6), +0xD0 = 1); 1
/// goes on (dialog 9, 800DA49C(0xA), the list hidden).
pub fn load_read(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let slot = menu_lbu(m, 0x5a);
        let file = menu_lbu(m, 0x5d);
        let result = call(m, 0x800d_6ab4, [slot, file, 1, 0]);
        if result == u32::MAX {
            step_by(m, 1);
            c2(m, 0x800d_a9a8, 1, 0xe);
            c2(m, 0x800d_a49c, 0xd, 0xe);
            set_menu_byte(m, 0xd0, 1);
            c2(m, 0x800d_c028, 0, 0);
        }
        if result == 2 {
            step_by(m, 1);
            c2(m, 0x800d_a9a8, 1, 8);
            c2(m, 0x800d_a49c, 6, 8);
            set_menu_byte(m, 0xd0, 1);
        }
        if result == 1 {
            dialog_to(m, 9);
            c2(m, 0x800d_a49c, 0xa, 0);
            c2(m, 0x800d_c028, 0, 0);
        }
    });
}

/// 800D2AE8 (no capture reaches it): +0xD0 = 1; cross goes back (dialog
/// 2, sound 0x59, +0xD0 cleared, the list hidden); a lost card returns to
/// the link-mode menu (screen 2, +0xD0 cleared, the list hidden).
pub fn load_read_failed(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let state = chosen_state(m);
        set_menu_byte(m, 0xd0, 1);
        if ctx_lw(m, 0x12c) & 0x20 != 0 {
            dialog_to(m, 2);
            sound(m, 0x59);
            set_menu_byte(m, 0xd0, 0);
            c2(m, 0x800d_c028, 0, 0);
        }
        if lost(state) {
            let block = menu(m);
            m.sh(off(block, 0xc), 2);
            m.sh(off(block, 0xe), 0);
            m.sb(off(block, 0xd0), 0);
            c2(m, 0x800d_c028, 0, 0);
        }
    });
}

/// 800D2BA0: whether the loaded account has progress: each of five groups
/// of four bytes of the progress block (context +0x95C: +0x2E2/+0x31E/
/// +0x35A/+0x396, +0x2E7.., +0x2D2.., +0x2F5.., +0x2F0..) has a non-zero
/// byte; 0 at the first group without one.
pub fn has_progress(m: &mut Machine) -> u32 {
    let progress = off(context(m), 0x95c);
    let any = |m: &mut Machine, first: u32| {
        (0..4u32).fold(0, |v, k| v | lbu(m, off(progress, first + 0x3c * k)))
    };
    for first in [0x2e2, 0x2e7, 0x2d2, 0x2f5] {
        if any(m, first) == 0 {
            return 0;
        }
    }
    u32::from(any(m, 0x2f0) != 0)
}

/// 800D2C84 (file, entry): the file information box: the account name
/// (800D5970(0)) at (0x40, 0x32), then "new file" (entry 6),
/// "simulation" (7), or, for a file that holds the entry (800D59C0), its
/// title (800D5BF4) and its "level", "time", "unit" and "control" lines
/// (800D5A70, 800D5AC8, 800D5C2C, 800D5CC0 of the file), else "no data".
pub fn file_info(m: &mut Machine, file: u32, entry: u32) {
    framed(m, 0x20, |m, _| {
        c2(m, 0x800d_7d08, 4, 0);
        c2(m, 0x800d_7c88, 2, 0);
        c4(m, 0x800d_7ce4, 1, 0xa, 0x14, 0);
        let v = c2(m, 0x800d_a8bc, 0xf, 0);
        c2(m, 0x800d_7cd4, v, 0);
        c4(m, 0x800d_7c60, 0x80, 0x80, 0x80, 0);
        c2(m, 0x800d_7c40, 0x40, 0x32);
        let name = c2(m, 0x800d_5970, 0, 0);
        c2(m, 0x800d_8aa0, name, 0);
        c2(m, 0x800d_7d08, 3, 0);
        let entry = entry & 0xff;
        let text = if entry == 6 {
            c2(m, 0x800d_7c40, 0x8c, 0x147);
            0x800c_c038
        } else if entry == 7 {
            c2(m, 0x800d_7c40, 0x8c, 0x147);
            0x800c_c044
        } else {
            let file = file & 0xff;
            if c2(m, 0x800d_59c0, file, entry) == 0 {
                c2(m, 0x800d_7c40, 0x8c, 0x147);
                0x800c_c070
            } else {
                c2(m, 0x800d_7c40, 0x6a, 0x118);
                let title = c2(m, 0x800d_5bf4, entry, 0);
                c2(m, 0x800d_8aa0, title, 0);
                for (y, label, value, argument) in [
                    (0x140, 0x800c_c050, 0x800d_5a70, entry),
                    (0x154, 0x800c_c058, 0x800d_5ac8, entry),
                    (0x168, 0x800c_c060, 0x800d_5c2c, entry),
                ] {
                    c2(m, 0x800d_7c40, 0x6a, y);
                    c2(m, 0x800d_8aa0, label, 0);
                    c2(m, 0x800d_7c40, 0x96, y);
                    let v = c2(m, value, argument, 0);
                    c2(m, 0x800d_8aa0, v, 0);
                }
                c2(m, 0x800d_7c40, 0x6a, 0x17c);
                c2(m, 0x800d_8aa0, 0x800c_c068, 0);
                c2(m, 0x800d_7c40, 0x96, 0x17c);
                c2(m, 0x800d_5cc0, file, 0)
            }
        };
        c2(m, 0x800d_8aa0, text, 0);
    });
}

/// 800D2E74 (account entry choice, open): the text 0x21E, panel 9,
/// cursors 1 and 0xF, icon 1 (0xA); +0xD3 = 1, step up; icons 6..0xD
/// (the entry +0x5F on); with progress ([`has_progress`]) the context's
/// +0x96C bit 4 is set and icon 0xC (0xD for entry 6) shows, else it is
/// cleared and icon 0xD shows (2).
pub fn entry_open(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        c2(m, 0x800d_a828, 0x21e, 0);
        c2(m, 0x800d_a77c, 9, 0);
        c4(m, 0x800d_a838, 1, 1, 0, 0x14);
        c4(m, 0x800d_a838, 0xf, 1, 0, 0x14);
        c2(m, 0x800d_a9a8, 1, 0xa);
        let block = menu(m);
        let step = lhu(m, off(block, 0x4e));
        m.sb(off(block, 0xd3), 1);
        m.sh(off(block, 0x4e), step.wrapping_add(1) as u16);
        for k in 0..8u32 {
            let entry = menu_lbu(m, 0x5f);
            c2(m, 0x800d_a9a8, 6 + k, u32::from(entry == k));
        }
        let icon = if has_progress(m) != 0 {
            let ctx = context(m);
            let flags = m.lw(off(ctx, 0x96c));
            m.sw(off(ctx, 0x96c), flags | 0x10);
            if menu_lbu(m, 0x5f) == 6 { 0xd } else { 0xc }
        } else {
            let ctx = context(m);
            let flags = m.lw(off(ctx, 0x96c));
            m.sw(off(ctx, 0x96c), flags & !0x10);
            0xd
        };
        c2(m, 0x800d_a9a8, icon, 2);
    });
}

/// The entry moved right: with progress 0..4 step, 5 goes to 7 (the
/// simulation), 7 to 6 (new file), 6 and up to 0; without, 0..5 step and
/// the rest to 0.
fn entry_right(e: u32, progress: bool) -> u32 {
    if !progress {
        return if e < 6 { e + 1 } else { 0 };
    }
    match e {
        5 => 7,
        7 => 6,
        0..=4 => e + 1,
        _ => 0,
    }
}
/// The entry moved left: with progress 0 and 6 go to 7, 7 to 5, 1..5
/// step down, 8 and up to 0; without, 0 goes to 6.
fn entry_left(e: u32, progress: bool) -> u32 {
    if !progress {
        return if e != 0 { e.wrapping_sub(1) } else { 6 };
    }
    match e {
        0 | 6 => 7,
        7 => 5,
        1..6 => e - 1,
        _ => 0,
    }
}

/// 800D2FB8 (account entry choice): a lost card returns to dialog 0 (the
/// text 0x13E, +0xD3 cleared); with the cursor free (800DA898(1) = 2)
/// pad +0x130 bits 3/2 move the entry (sound 0x3D); icons 6..0xD and
/// 0xC/0xD; the file information box ([`file_info`]); decide on a saved
/// entry (bit of 800D5CA0's mask) or the new file/simulation entries:
/// first the cursor (800DA838(1, 3, -1, -1), +0x60 = 1, sound 0x59),
/// then the account (800D4DF4 of the entry, dialog 0xB, the context's
/// +0x954 = 2, +0x96C bit 4 cleared; entry 6 gives dialog 0xA, entry 7
/// screen 6; the context's +0x95D takes the entry); otherwise sound
/// 0x30; cross (or the cursor running out) goes back (+0x60 = 2, sound
/// 0x3B; then dialog 7 with the YES/NO box (800DBF6C at (0x40, 0x1E))).
pub fn entry_choice(m: &mut Machine) {
    framed(m, 0x28, |m, _| {
        let cursor = c2(m, 0x800d_a898, 1, 0);
        let saves = call(m, 0x800d_5ca0, [0; 4]);
        if lost(chosen_state(m)) {
            let block = menu(m);
            m.sh(off(block, 0x4c), 0);
            m.sh(off(block, 0x4e), 0);
            m.sb(off(block, 0xd3), 0);
            c2(m, 0x800d_a828, 0x13e, 0);
            return;
        }
        for (bit, right) in [(8u32, true), (4, false)] {
            let ctx = context(m);
            if m.lw(off(ctx, 0x130)) & bit == 0 || cursor != 2 {
                continue;
            }
            let progress = m.lw(off(ctx, 0x96c)) & 0x10 != 0;
            let block = menu(m);
            let entry = lbu(m, off(block, 0x5f));
            let next = if right {
                entry_right(entry, progress)
            } else {
                entry_left(entry, progress)
            };
            m.sb(off(block, 0x5f), next as u8);
            sound(m, 0x3d);
        }
        for k in 0..8u32 {
            let entry = menu_lbu(m, 0x5f);
            c2(m, 0x800d_a9a8, 6 + k, u32::from(entry == k));
        }
        let icon = if ctx_lw(m, 0x96c) & 0x10 != 0 && menu_lbu(m, 0x5f) != 6 {
            0xc
        } else {
            0xd
        };
        c2(m, 0x800d_a9a8, icon, 2);
        let file = menu_lbu(m, 0x5d);
        let entry = menu_lbu(m, 0x5f);
        file_info(m, file, entry);
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x30 == 0x10 || cursor == 0 {
            entry_decide(m, cursor, saves, pressed);
        }
        if ctx_lw(m, 0x12c) & 0x20 == 0 && cursor != 0 {
            return;
        }
        if cursor != 0 && cursor != 3 {
            c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
            set_menu_byte(m, 0x60, 2);
            sound(m, 0x3b);
            return;
        }
        if menu_lbu(m, 0x60) != 2 {
            return;
        }
        c4(m, 0x800d_a838, 1, 0, 0, 0);
        dialog_to(m, 7);
        c4(m, 0x800d_bf6c, 0x40, 0x1e, 0, 0);
    });
}

/// The decide part of [`entry_choice`].
fn entry_decide(m: &mut Machine, cursor: u32, saves: u32, pressed: u32) {
    let entry = menu_lbu(m, 0x5f);
    let special = entry.wrapping_sub(6) < 2;
    if !special && sra(saves, menu_lbu(m, 0x5f) & 0x1f) & 1 == 0 {
        if pressed & 0x10 != 0 {
            sound(m, 0x30);
        }
        return;
    }
    if cursor != 0 && cursor != 3 {
        c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
        set_menu_byte(m, 0x60, 1);
        sound(m, 0x59);
        return;
    }
    if menu_lbu(m, 0x60) != 1 {
        return;
    }
    if menu_lbu(m, 0x5f) < 6 {
        c4(m, 0x800d_a838, 1, 0, 0, 0);
        let entry = menu_lbu(m, 0x5f);
        call(m, 0x800d_4df4, [entry, 0, 0, 0]);
        let block = menu(m);
        m.sh(off(block, 0x4c), 0xb);
        let ctx = context(m);
        m.sh(off(block, 0x4e), 0);
        m.sh(off(ctx, 0x954), 2);
        let flags = m.lw(off(ctx, 0x96c));
        m.sw(off(ctx, 0x96c), flags & !0x10);
    }
    let block = menu(m);
    if lbu(m, off(block, 0x5f)) == 6 {
        let ctx = context(m);
        m.sh(off(block, 0x4c), 0xa);
        m.sh(off(block, 0x4e), 0);
        let flags = m.lw(off(ctx, 0x96c));
        m.sw(off(ctx, 0x96c), flags & !0x10);
    }
    let block = menu(m);
    if lbu(m, off(block, 0x5f)) == 7 {
        m.sh(off(block, 0xc), 6);
        m.sh(off(block, 0xe), 0);
    }
    let block = menu(m);
    let ctx = context(m);
    let entry = lbu(m, off(block, 0x5f));
    m.sb(off(ctx, 0x95d), entry as u8);
}

/// 800D34FC (no capture reaches it; difficulty, open): step up; the text
/// 0x43E, cursor 0xF, icons 1 (0xB) and 4 (6), the choice box
/// (800DBF6C at (0x40, 0x32)), 800DA49C(0xB), icon 4 (6).
pub fn level_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a828, 0x43e, 0);
        c4(m, 0x800d_a838, 0xf, 1, 0, 0x3c);
        c2(m, 0x800d_a9a8, 1, 0xb);
        c2(m, 0x800d_a9a8, 4, 6);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0xb, 0x32);
        c2(m, 0x800d_a9a8, 4, 6);
    });
}

/// 800D3584 (no capture reaches it; difficulty for a new file): "easy",
/// "normal", "hard"; [`LEVEL`] moves with pad +0x130 bits 1/0 modulo 3
/// (sound 0x3D); circle takes it (the context's +0x394, the 60 mission
/// records reset: +0xD0C.. bytes, +0xBA4.. halfwords 0xD2F0, +0xE4C..
/// and +0xF3C.. words; dialog 0xB, sound 0x59, +0x954 = 2, +0x38C = 1),
/// cross goes back (dialog 9, sound 0x3B); icons 6..8; a lost card gives
/// dialog 2 with the box (800DBF6C at (0x6F, 0x94)).
pub fn level(m: &mut Machine) {
    framed(m, 0x28, |m, _| {
        let state = chosen_state(m);
        c2(m, 0x800d_7d08, 4, 0);
        c4(m, 0x800d_7c60, 0x80, 0x80, 0x80, 0);
        c2(m, 0x800d_7c88, 2, 0);
        for (y, text) in [
            (0xa6, 0x800c_c078),
            (0xda, 0x800c_c080),
            (0x10e, 0x800c_c088),
        ] {
            c2(m, 0x800d_7c40, 0x6a, y);
            c2(m, 0x800d_8aa0, text, y);
        }
        if ctx_lw(m, 0x130) & 2 != 0 {
            let (v, u) = (lh(m, LEVEL), lhu(m, LEVEL));
            m.sh(LEVEL, if lt(v, 2) { u.wrapping_add(1) } else { 0 } as u16);
            sound(m, 0x3d);
        }
        if ctx_lw(m, 0x130) & 1 != 0 {
            let (v, u) = (lh(m, LEVEL), lhu(m, LEVEL));
            m.sh(LEVEL, if lt(0, v) { u.wrapping_sub(1) } else { 2 } as u16);
            sound(m, 0x3d);
        }
        let ctx = context(m);
        let pressed = m.lw(off(ctx, 0x12c));
        if pressed & 0x10 != 0 {
            let v = lh(m, LEVEL);
            m.sw(off(ctx, 0x394), v);
            for k in 0..0x3cu32 {
                let ctx = context(m);
                m.sb(off(ctx.wrapping_add(k), 0xd0c), 0);
                let ctx = context(m);
                m.sh(off(ctx.wrapping_add(2 * k), 0xba4), 0xd2f0);
                m.sw(off(ctx.wrapping_add(4 * k), 0xe4c), 0);
                m.sw(off(ctx.wrapping_add(4 * k), 0xf3c), 0);
            }
            dialog_to(m, 0xb);
            sound(m, 0x59);
            let ctx = context(m);
            m.sh(off(ctx, 0x954), 2);
            m.sh(off(ctx, 0x38c), 1);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 9);
            sound(m, 0x3b);
        }
        for k in 0..3u32 {
            let level = lh(m, LEVEL);
            c2(m, 0x800d_a9a8, 6 + k, u32::from(k != level));
        }
        if lost(state) {
            dialog_to(m, 2);
            c4(m, 0x800d_bf6c, 0x6f, 0x94, 0, 0);
        }
    });
}

/// 800D37FC (Download, the disc prompt open): step up, +0xD4 cleared;
/// the box (800DBF6C at (0x6F, 0x96), 0, 0).
pub fn disc_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        let step = lhu(m, off(block, 0x4e));
        m.sw(off(block, 0xd4), 0);
        m.sh(off(block, 0x4e), step.wrapping_add(1) as u16);
        c4(m, 0x800d_bf6c, 0x6f, 0x96, 0, 0);
    });
}

/// 800D3840 (Download, the disc check): without a disc number (8002B504
/// = 0) icon 1 (0x49); otherwise the disc the account entry needs
/// ([`ENTRY_DISC`] by the context's +0x38C): either disc (3) or disc 3
/// goes on to screen 6; disc 1 or 2 inserted against the other needed,
/// or no disc (4, -1), asks for the needed disc (icon 1 (+0xD4 + 0x46 or
/// 0x47)), stops the CD (8002B428), 800DA49C(0x25) and steps up.
pub fn disc_check(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        let disc = call(m, 0x8002_b504, [0; 4]);
        if disc == 0 {
            c2(m, 0x800d_a9a8, 1, 0x49);
            return;
        }
        let needed = |m: &mut Machine| {
            let ctx = context(m);
            let entry = lh(m, off(ctx, 0x38c));
            lbu(m, entry.wrapping_add(ENTRY_DISC))
        };
        let prompt = |m: &mut Machine, k: u32| {
            let icon = menu_lw(m, 0xd4).wrapping_add(k);
            c2(m, 0x800d_a9a8, 1, icon);
        };
        let ask = |m: &mut Machine| {
            call(m, 0x8002_b428, [0; 4]);
            c2(m, 0x800d_a49c, 0x25, 0);
            step_by(m, 1);
        };
        let need = needed(m);
        if need == 3 || disc == 3 {
            to_screen_6(m);
            return;
        }
        if disc == 4 || disc == u32::MAX {
            if need == 1 {
                prompt(m, 0x46);
            }
            if needed(m) == 2 {
                prompt(m, 0x47);
            }
            ask(m);
            return;
        }
        if disc == 1 {
            if need == 1 {
                to_screen_6(m);
                return;
            }
            prompt(m, 0x47);
            ask(m);
            return;
        }
        if disc == 2 {
            if needed(m) == 2 {
                to_screen_6(m);
                return;
            }
            prompt(m, 0x46);
            ask(m);
        }
    });
}

fn menu_lw(m: &mut Machine, offset: u32) -> u32 {
    let block = menu(m);
    m.lw(off(block, offset))
}
fn to_screen_6(m: &mut Machine) {
    let block = menu(m);
    m.sh(off(block, 0xc), 6);
    m.sh(off(block, 0xe), 0);
}

/// 800D3A24 (no capture reaches it): the text 0x400036, icon 4 (4),
/// +0xD3 cleared; once the CD reports 3 (8002B528), step up.
pub fn disc_wait(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        c2(m, 0x800d_a828, 0x0040_0036, 0);
        c2(m, 0x800d_a9a8, 4, 4);
        set_menu_byte(m, 0xd3, 0);
        if call(m, 0x8002_b528, [0; 4]) == 3 {
            step_by(m, 1);
        }
    });
}

/// 800D3A8C (no capture reaches it): when the CD reports 1 (8002B528):
/// icon 1 (0x49), 800DA49C(0x26), +0xD4 = 0x1F, step -2. Returns the CD
/// report, or the new step (as the word the original leaves).
pub fn disc_retry(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let report = call(m, 0x8002_b528, [0; 4]);
        if report != 1 {
            return report;
        }
        c2(m, 0x800d_a9a8, 1, 0x49);
        c2(m, 0x800d_a49c, 0x26, 0x49);
        let block = menu(m);
        let step = lhu(m, off(block, 0x4e));
        m.sw(off(block, 0xd4), 0x1f);
        let step = step.wrapping_sub(2);
        m.sh(off(block, 0x4e), step as u16);
        step
    })
}

/// 800D3AE8 (Download screen, step 0): step 1, countdown and dialog
/// cleared; the box (800DBF6C at (0x6F, 0x94), 0, 0) with +0xD3 cleared;
/// 800DA49C(3).
pub fn load_screen_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x48), 0);
        m.sh(off(block, 0xe), 1);
        m.sh(off(block, 0x4c), 0);
        m.sh(off(block, 0x4e), 0);
        m.sb(off(block, 0xd3), 0);
        c4(m, 0x800d_bf6c, 0x6f, 0x94, 0, 0);
        c2(m, 0x800d_a49c, 3, 0x94);
    });
}

/// 800D3B3C (Download screen, step 1): the backdrop by +0xD3 (1: at
/// (0x33, 0x18D), 0: at (0x46, 0x19A)); the dialog step
/// ([`LOAD_DIALOGS`]); 800DBFE4, the file list (800DC098(0, 0)) and the
/// status icons.
pub fn load_screen(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        match menu_lbu(m, 0xd3) {
            1 => backdrop(m, 0x33, 0x18d),
            0 => backdrop(m, 0x46, 0x19a),
            _ => {}
        }
        dialog_step(m, LOAD_DIALOGS);
        call(m, 0x800d_bfe4, [0; 4]);
        c2(m, 0x800d_c098, 0, 0);
        status_icons(m);
    });
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, ..] = args;
    let void = |m: &mut Machine, f: fn(&mut Machine)| {
        f(m);
        0
    };
    Some(match target {
        0x800d_1798 => void(m, account_screen_open),
        0x800d_181c => void(m, account_screen),
        0x800d_1a5c => void(m, load_open),
        0x800d_1adc => void(m, load_wait_probes),
        0x800d_1b2c => void(m, load_no_card_open),
        0x800d_1c18 => void(m, load_no_card),
        0x800d_1cf0 => void(m, load_slot_open),
        0x800d_1dcc => void(m, load_slot_choice),
        0x800d_202c => void(m, load_card_ready),
        0x800d_20bc | 0x800d_21d0 => void(m, load_card_lost),
        0x800d_2154 => void(m, load_unformatted),
        0x800d_2268 => void(m, load_no_file),
        0x800d_22e4 => void(m, load_card_back),
        0x800d_237c => void(m, load_restart),
        0x800d_23cc => void(m, load_card_step),
        0x800d_248c => void(m, load_card_finish),
        0x800d_255c => void(m, load_card_wait),
        0x800d_2600 => void(m, load_file_open),
        0x800d_26f0 => void(m, load_file_choice),
        0x800d_2954 => load_reading_open(m),
        0x800d_2994 => void(m, load_record),
        0x800d_29fc => void(m, load_read),
        0x800d_2ae8 => void(m, load_read_failed),
        0x800d_2ba0 => has_progress(m),
        0x800d_2c84 => {
            file_info(m, a0, a1);
            0
        }
        0x800d_2e74 => void(m, entry_open),
        0x800d_2fb8 => void(m, entry_choice),
        0x800d_34fc => void(m, level_open),
        0x800d_3584 => void(m, level),
        0x800d_37fc => void(m, disc_open),
        0x800d_3840 => void(m, disc_check),
        0x800d_3a24 => void(m, disc_wait),
        0x800d_3a8c => disc_retry(m),
        0x800d_3ae8 => void(m, load_screen_open),
        0x800d_3b3c => void(m, load_screen),
        _ => return None,
    })
}
