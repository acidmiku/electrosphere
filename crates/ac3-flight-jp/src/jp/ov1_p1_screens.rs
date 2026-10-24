//! JP overlay 1, part 1: the menu screens of the new-account path
//! (800CD9A8..800D1738): the link-mode menu, name entry, difficulty,
//! the sound and screen settings, the save prompts and the disc checks.
//!
//! Each screen is a table of step routines ([`SCREENS`](super::ov1_p1::SCREENS),
//! run by [`frame`](super::ov1_p1::frame) with the menu block's screen +0xC
//! and step +0xE); a screen's sub-dialogs keep their own step at +0x4C/+0x4E.
//! The text, icon and cursor layers they drive (800DA49C, 800DA77C,
//! 800DA828, 800DA838, 800DA898, 800DA9A8, 800DBF6C, 800DC028, ...) are
//! overlay 1's other half, called through dispatch; 8002DB4C plays a
//! menu sound.
use super::ov1_p1::{
    MODULE, call, context, ctx_lw, framed, lbu, lh, lhu, lt, menu, menu_lbu, menu_lh, menu_lw,
    mulhi, off, sra,
};
use super::ov1_p1_card::slot_state;
use crate::{arena, common::loaded_module, machine::Machine};

/// The menu sound request (8002DB4C).
pub(crate) fn sound(m: &mut Machine, id: u32) {
    call(m, 0x8002_db4c, [id, 0, 0, 0]);
}
/// A layer call with two arguments.
pub(crate) fn c2(m: &mut Machine, target: u32, a0: u32, a1: u32) -> u32 {
    call(m, target, [a0, a1, 0, 0])
}
/// A layer call with four arguments.
pub(crate) fn c4(m: &mut Machine, target: u32, a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
    call(m, target, [a0, a1, a2, a3])
}
/// `MENU[offset] += 1` (word).
pub(crate) fn count_up(m: &mut Machine, offset: u32) {
    let block = menu(m);
    let n = m.lw(off(block, offset)).wrapping_add(1);
    m.sw(off(block, offset), n);
}
/// The dialog step +0x4E, counted up (halfword).
pub(crate) fn dialog_step_up(m: &mut Machine, block: u32) {
    let step = lhu(m, off(block, 0x4e)).wrapping_add(1);
    m.sh(off(block, 0x4e), step as u16);
}
/// The dialog's next state: +0x4C = `state`, step +0x4E = 0.
pub(crate) fn dialog_to(m: &mut Machine, state: u32) {
    let block = menu(m);
    m.sh(off(block, 0x4c), state as u16);
    m.sh(off(block, 0x4e), 0);
}
/// The pad's held (+0x128) and pressed (+0x12C) words both have `bit`.
pub(crate) fn held_and_pressed(m: &mut Machine, bit: u32) -> bool {
    ctx_lw(m, 0x128) & bit != 0 && ctx_lw(m, 0x12c) & bit != 0
}

// ---- screen 2: the link-mode menu ---------------------------------------------

/// 800CD9A8 (screen 2, step 0): step 1, the countdown cleared; starts
/// the card layer (800D56E0 and [`probe_reset`](super::ov1_p1_card::probe_reset))
/// unless it runs; the cursors 1, 3 and 0xF; the window frame (800D94C4
/// with the stack quad (0x10A, 0x1C)..(0x137, 0x94) and its colours,
/// into the packet pointer); icons 5 (bit 4 of the context's +0x124),
/// 4, 0x10 and the text 0xBE; the stream request at +0x7C (0x83D) and
/// 800DA49C(2); +0xD1 = 1.
pub fn link_open(m: &mut Machine) {
    framed(m, 0x48, |m, sp| {
        let block = menu(m);
        m.sb(off(block, 0x5e), 0);
        let block2 = menu(m);
        m.sh(off(block, 0xe), 1);
        m.sw(off(block, 0x48), 0);
        if m.lw(off(block2, 0x18)) == 0 {
            call(m, 0x800d_56e0, [1, 0, 0, 0]);
            super::ov1_p1_card::probe_reset(m);
        }
        let block = menu(m);
        m.sw(off(block, 0x18), 1);
        c4(m, 0x800d_a838, 1, 1, 0, 0x3c);
        c4(m, 0x800d_a838, 3, 1, 0, 0x3c);
        c4(m, 0x800d_a838, 0xf, 1, 0, 0x3c);
        let frame = call(m, 0x800d_a6fc, [0xf, 1, 0, 0x3c]);
        for (offset, value) in [
            (0x10, 0x10a),
            (0x18, 0x10a),
            (0x12, 0x1c),
            (0x16, 0x1c),
            (0x14, 0x137),
            (0x1c, 0x137),
            (0x1a, 0x94),
            (0x1e, 0x94),
        ] {
            m.sh(off(sp, offset), value);
        }
        for (offset, value) in [
            (0x23, 0),
            (0x20, 0),
            (0x21, 0x70),
            (0x22, 0x78),
            (0x24, 0),
            (0x25, 0x70),
            (0x26, 0x78),
            (0x28, 0),
            (0x29, 0x70),
            (0x2a, 0x78),
            (0x2c, 0),
            (0x2d, 0x70),
            (0x2e, 0x78),
        ] {
            m.sb(off(sp, offset), value);
        }
        let packet = menu_lw(m, 8);
        let next = call(
            m,
            0x800d_94c4,
            [off(sp, 0x10), packet, off(frame, 0x38), 0x3c],
        );
        let block = menu(m);
        let ctx = context(m);
        m.sw(off(block, 8), next);
        let card = u32::from(m.lw(off(ctx, 0x124)) & 0x10 != 0);
        c2(m, 0x800d_a9a8, 5, card);
        c2(m, 0x800d_a828, 0xbe, card);
        c2(m, 0x800d_a9a8, 4, 1);
        let block = menu(m);
        m.sh(off(block, 0x7c), 0x83d);
        let request = off(block, 0x7c);
        m.sh(off(request, 2), 0);
        m.sh(off(request, 4), 0);
        m.sh(off(request, 6), 0);
        m.sw(off(request, 8), 0);
        c2(m, 0x800d_a49c, 2, 1);
        let block = menu(m);
        m.sb(off(block, 0xd1), 1);
        c2(m, 0x800d_a9a8, 0x10, 2);
    });
}

/// 800CDB64 (screen 2, step 1): the link-mode menu (0 New Account, 1
/// Download, 2 the third choice, which needs bit 0 of the context's
/// +0x964). Draws the 5x5 opening triangles (800D9624) for the first 30
/// frames and the scrolling backdrop (800D9A08); the decide and cancel
/// handling (800DA898, sounds 0x59/0x3B; cancel returns to the title
/// and stops the card layer), the cursor (pad +0x12C bits 0/1, skipping
/// Download while a slot reports state 5), the icons 6..8, 1 and 5,
/// and the menu stream (8002CB58, 8002C580, 8002C8D8 of +0x7C).
pub fn link_menu(m: &mut Machine) {
    framed(m, 0x80, |m, sp| {
        let first = slot_state(m, 0);
        m.sw(off(sp, 0x40), first);
        let second = slot_state(m, 1);
        m.sw(off(sp, 0x44), second);
        let fp = call(m, 0x800d_a6fc, [1, 0, 0, 0]);
        let ctx = context(m);
        let flags = m.lw(off(ctx, 0x964));
        for offset in [0x21, 0x25, 0x29] {
            m.sb(off(sp, offset), 0x70);
        }
        for offset in [0x22, 0x26, 0x2a] {
            m.sb(off(sp, offset), 0x78);
        }
        let block = menu(m);
        for offset in [0x23, 0x20, 0x24, 0x28] {
            m.sb(off(sp, offset), 0);
        }
        let third = flags & 1;
        m.sw(off(sp, 0x48), third);
        if lbu(m, off(block, 0x5e)) < 0x1e {
            for row in 0..5u32 {
                let (s5, s6) = (row * 0x12, 0x64u32.wrapping_sub(row * 0x12));
                for col in 0..5u32 {
                    let (s0, s2) = (0x113 + 9 * col, 9 * col);
                    let corner = ((col + row) << 1).wrapping_add(0x1e);
                    let block = menu(m);
                    let t = corner.wrapping_sub(lbu(m, off(block, 0x5e)));
                    if lt(t, 0) {
                        continue;
                    }
                    m.sh(off(sp, 0x10), s2.wrapping_sub(t.wrapping_sub(0x113)) as u16);
                    let y = (t << 1).wrapping_add(0x64).wrapping_sub(s5);
                    m.sh(off(sp, 0x12), s6 as u16);
                    m.sh(off(sp, 0x14), s0 as u16);
                    m.sh(off(sp, 0x16), s6 as u16);
                    m.sh(off(sp, 0x18), s0 as u16);
                    m.sh(off(sp, 0x1a), y as u16);
                    let packet = m.lw(off(block, 8));
                    m.sw(off(sp, 0x50), 0x800e_0000);
                    let next = call(
                        m,
                        0x800d_9624,
                        [off(sp, 0x10), packet, off(fp, 0x38), 0x800e_0000],
                    );
                    let block = menu(m);
                    m.sw(off(block, 8), next);
                }
            }
            let block = menu(m);
            let n = lbu(m, off(block, 0x5e)).wrapping_add(1);
            m.sb(off(block, 0x5e), n as u8);
        }
        let counter = ctx_lw(m, 0x20);
        let rounded = if lt(counter, 0) {
            counter.wrapping_add(0x3f)
        } else {
            counter
        };
        let phase = counter.wrapping_sub(sra(rounded, 6) << 6) << 6;
        let packet = menu_lw(m, 8);
        let next = c4(m, 0x800d_9a08, 0x46, 0x19a, phase, packet);
        let block = menu(m);
        m.sw(off(block, 8), next);
        let r = c2(m, 0x800d_a898, 1, 0x19a);
        let pad = ctx_lw(m, 0x12c);
        if pad & 0x10 != 0 || r == 0 {
            if r.wrapping_sub(1) < 2 {
                c4(m, 0x800d_a838, 1, 3, u32::MAX, u32::MAX);
                c4(m, 0x800d_a838, 3, 3, u32::MAX, u32::MAX);
                c4(m, 0x800d_a838, 8, 3, u32::MAX, u32::MAX);
                sound(m, 0x59);
            } else {
                c4(m, 0x800d_a838, 1, 0, 0, 0);
                c4(m, 0x800d_a838, 3, 0, 0, 0);
                c4(m, 0x800d_a838, 8, 0, 0, 0);
                let block = menu(m);
                if lbu(m, off(block, 0x59)) == 2 {
                    let step = lhu(m, off(block, 0xe)).wrapping_add(1);
                    m.sh(off(block, 0xe), step as u16);
                } else {
                    let choice = lbu(m, off(block, 0x59));
                    m.sh(off(block, 0xe), 0);
                    m.sh(off(block, 0x10), 0);
                    m.sh(off(block, 0xc), choice.wrapping_add(3) as u16);
                }
            }
        } else if pad & 0x20 != 0 {
            let block = menu(m);
            m.sh(off(block, 0xc), 1);
            m.sh(off(block, 0xe), 0);
            m.sw(off(block, 0x78), 0);
            sound(m, 0x3b);
            call(m, 0x8002_c6a8, [0, 0, 0, 0]);
            let block = menu(m);
            m.sw(off(block, 0x18), 0);
            call(m, 0x800d_58b0, [0, 0, 0, 0]);
            super::ov1_p1_card::probe_reset(m);
            return;
        }
        let old = menu_lbu(m, 0x59);
        let r = c2(m, 0x800d_a898, 1, 0);
        let download_blocked = first == 5 || second == 5;
        if ctx_lw(m, 0x12c) & 2 != 0 && r == 2 {
            let block = menu(m);
            let choice = lbu(m, off(block, 0x59));
            let next = if choice < 2 { choice + 1 } else { 0 };
            m.sb(off(block, 0x59), next as u8);
            let block = menu(m);
            if lbu(m, off(block, 0x59)) == 1 && !download_blocked {
                m.sb(off(block, 0x59), r as u8);
            }
            let block = menu(m);
            if lbu(m, off(block, 0x59)) == 2 && third == 0 {
                m.sb(off(block, 0x59), 0);
            }
        }
        if ctx_lw(m, 0x12c) & 1 != 0 && r == 2 {
            let block = menu(m);
            let choice = lbu(m, off(block, 0x59));
            let next = if choice == 0 { r } else { choice - 1 };
            m.sb(off(block, 0x59), next as u8);
            let block = menu(m);
            let choice = lbu(m, off(block, 0x59));
            if choice == 2 && third == 0 {
                m.sb(off(block, 0x59), (choice - 1) as u8);
            }
            let block = menu(m);
            if lbu(m, off(block, 0x59)) == 1 && !download_blocked {
                m.sb(off(block, 0x59), 0);
            }
        }
        if menu_lbu(m, 0x59) != old {
            sound(m, 0x3d);
        }
        if menu_lbu(m, 0x59) == 1 && !download_blocked && c2(m, 0x800d_a898, 1, 0) == 2 {
            let block = menu(m);
            m.sb(off(block, 0x59), 0);
        }
        let choice = menu_lbu(m, 0x59);
        c2(m, 0x800d_a9a8, 6, u32::from(choice != 0));
        let icon = if download_blocked {
            u32::from(menu_lbu(m, 0x59) != 1)
        } else {
            2
        };
        c2(m, 0x800d_a9a8, 7, icon);
        let icon = if third == 0 {
            2
        } else {
            u32::from(menu_lbu(m, 0x59) != 2)
        };
        c2(m, 0x800d_a9a8, 8, icon);
        c2(m, 0x800d_a9a8, 1, 1);
        count_up(m, 0x48);
        let card = u32::from(ctx_lw(m, 0x124) & 0x10 != 0);
        c2(m, 0x800d_a9a8, 5, card);
        let stream = menu_lh(m, 0x7c);
        if call(m, 0x8002_cb58, [stream, 0, 0, 0]) == 1
            && menu_lh(m, 0xc) == 2
            && call(m, 0x8002_c580, [1, 0, 0, 0]) == 0
        {
            let request = off(menu(m), 0x7c);
            call(m, 0x8002_c8d8, [request, 1, 0, 0]);
        }
    });
}

// ---- the account dialogs ----------------------------------------------------

/// 800CE24C (no capture reaches it): counts the step +0xE up, clears the
/// dialog state +0xE0 and starts 800DA4F4 with the context's +0x38C.
/// Models no result: the original leaves 800DA4F4's `v0`, which that
/// routine's translation does not model, and its only caller, step 2 of
/// screen 2 (the list 0x800DD4B0 of the screen table, through the screen
/// step's `jalr`), discards it.
pub fn account_open(m: &mut Machine) {
    let block = menu(m);
    let ctx = context(m);
    framed(m, 0x18, |m, _| {
        let step = lhu(m, off(block, 0xe));
        m.sw(off(block, 0xe0), 0);
        m.sh(off(block, 0xe), step.wrapping_add(1) as u16);
        let value = lh(m, off(ctx, 0x38c));
        call(m, 0x800d_a4f4, [value, 0, 0, 0]);
    });
}

/// 800CE290 (no capture reaches it): runs the dialog routine +0xE0 of
/// the table 0x800DD914 (with the caller's arguments) and acts on its
/// result bits (1 next routine, 2 the exit flag +0x58, 4 routine 1); then
/// 800DA4DC's state: from routine 2 on the text 0x400036 and icon 4,
/// and icon 1 for states 1..6 (0x49, 0x46, 0x47, 0x65, 0x66, 0).
pub fn account_dialog(m: &mut Machine, args: [u32; 4]) {
    framed(m, 0x18, |m, _| {
        let index = menu_lw(m, 0xe0) << 2;
        let routine = m.lw(index.wrapping_add(0x800d_d914));
        let bits = call(m, routine, args);
        if bits & 1 != 0 {
            count_up(m, 0xe0);
        }
        if bits & 2 != 0 {
            let block = menu(m);
            m.sb(off(block, 0x58), 1);
            return;
        }
        if bits & 4 != 0 {
            let block = menu(m);
            m.sw(off(block, 0xe0), 1);
        }
        let state = call(m, 0x800d_a4dc, [bits, 0, 0, 0]);
        if !lt(menu_lw(m, 0xe0), 2) {
            c2(m, 0x800d_a828, 0x0040_0036, 0);
            c2(m, 0x800d_a9a8, 4, 4);
        }
        for (value, icon) in [
            (1, 0x49),
            (2, 0x46),
            (3, 0x47),
            (4, 0x65),
            (5, 0x66),
            (6, 0),
        ] {
            if state == value {
                c2(m, 0x800d_a9a8, 1, icon);
            }
        }
    });
}

/// 800CE3F4: resets the context's progress block (+0x95C): account
/// byte 6, the five 60-halfword tables to 0x8CA0, and every other table,
/// bit set and record cleared; clears the context flag +0x96C bit 0x10.
pub fn progress_reset(m: &mut Machine) {
    let ctx = context(m);
    let progress = off(ctx, 0x95c);
    m.sb(off(ctx, 0x95c), 0);
    m.sb(off(progress, 1), 6);
    m.sb(off(progress, 2), 0);
    for k in 0..0x3cu32 {
        let at = off(progress, 2 * k);
        for offset in [0x68, 0xe0, 0x158, 0x1d0, 0x248] {
            m.sh(off(at, offset), 0x8ca0);
        }
    }
    for k in 0..0x3cu32 {
        let at = off(progress, k);
        for offset in [0x2c0, 0x2fc, 0x338, 0x374, 0x3b0] {
            m.sb(off(at, offset), 0);
        }
    }
    for k in (0..=0x3bu32).rev() {
        m.sb(off(progress, k + 0x3ec), 0);
    }
    for k in (0..=0xb3u32).rev() {
        m.sb(off(progress, k + 0x43c), 0);
    }
    for k in 0..0x3cu32 {
        let at = off(progress, 4 * k);
        m.sw(off(at, 0x4f0), 0);
        m.sw(off(at, 0x5e0), 0);
    }
    m.sw(off(progress, 0x6d0), 0);
    m.sw(off(progress, 0x6d4), 0);
    m.sw(off(progress, 0x6d8), 0);
    for k in (0..=0x3fu32).rev() {
        m.sb(off(progress, k + 0x18), 0);
    }
    for k in (0..=0xfu32).rev() {
        m.sb(off(progress, k + 0x58), 0);
    }
    let ctx = context(m);
    let flags = m.lw(off(ctx, 0x96c));
    m.sw(off(ctx, 0x96c), flags & !0x10);
}

// ---- screen 3: name entry ---------------------------------------------------

/// 800CE518 (name entry, open): dialog step up; the panels 0xB and 6, the
/// text 0x85E, cursor 0xF, icons 1 (0xF) and 4 (0x16), the keyboard
/// (800DBF6C at (0x6F, 0x96)) and 800DA49C(0xE).
pub fn name_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sh(off(block, 0x10), 0);
        m.sw(off(block, 0x48), 0);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0xb, 0);
        c2(m, 0x800d_a77c, 6, 0);
        c2(m, 0x800d_a828, 0x85e, 0);
        c4(m, 0x800d_a838, 0xf, 1, 0, 0x3c);
        c2(m, 0x800d_a9a8, 1, 0xf);
        c2(m, 0x800d_a9a8, 4, 0x16);
        c4(m, 0x800d_bf6c, 0x6f, 0x96, 1, 1);
        c2(m, 0x800d_a49c, 0xe, 0x96);
    });
}

/// The keyboard cursor (+0x5B) moved right: rows 0..0xD, 0xE..0x1A,
/// 0x1B..0x26, 0x27..0x31 and 0x32..0x33 wrap (0x26 to 0x1A, as the
/// original's table has it).
fn key_right(c: u32) -> Option<u32> {
    match c {
        0xd => Some(0),
        0..0xe => Some(c + 1),
        0x1a => Some(0xe),
        0xe..0x1b => Some(c + 1),
        0x26 => Some(0x1a),
        0x1b..0x27 => Some(c + 1),
        0x31 => Some(0x27),
        0x27..0x32 => Some(c + 1),
        0x33 => Some(0x32),
        0x32..0x34 => Some(c + 1),
        _ => None,
    }
}
fn key_left(c: u32) -> Option<u32> {
    match c {
        0 => Some(0xd),
        1..0xe => Some(c - 1),
        0xe => Some(0x1a),
        0xf..0x1b => Some(c - 1),
        0x1b => Some(0x1a),
        0x1c..0x27 => Some(c - 1),
        0x27 => Some(0x31),
        0x28..0x32 => Some(c - 1),
        0x32 => Some(0x33),
        0x33 => Some(c - 1),
        _ => None,
    }
}
fn key_down(c: u32) -> Option<u32> {
    match c {
        0xd => Some(c + 0xd),
        0..0xe => Some(c + 0xe),
        0x1a => Some(c + 0x17),
        0xe..0x1b => Some(c + 0xd),
        0x26 => Some(c + 0xb),
        0x1b..0x27 => Some(c + 0xc),
        0x27..0x30 => Some(0x32),
        0x30..0x32 => Some(0x33),
        0x32 => Some(2),
        0x33 => Some(9),
        _ => None,
    }
}
fn key_up(c: u32) -> Option<u32> {
    match c {
        0..9 => Some(0x32),
        9..0xe => Some(0x33),
        0xe..0x1b => Some(c - 0xe),
        0x1b..0x27 => Some(c - 0xd),
        0x27..0x32 => Some(c - 0xc),
        0x32 => Some(0x29),
        _ => None,
    }
}

/// 800CE5AC (name entry): icon 2 at the keyboard cursor; the name buffer
/// (800D5970(0)) and its length; the cursor moves (pad +0x130 bits 3, 2,
/// 1, 0; sound 0x3D); START ends the entry (dialog 1, or sound 0x30 for
/// an empty name); L2/R2 (0x300) and key 0x31 switch the character page
/// (+0x5C; sound 0x2F); circle types the key under the cursor (up to 8
/// characters, no leading space), 0xD and cross delete (sound 0x2E), 0x1A
/// ends, 0x33 returns to the link-mode menu (panels 7, 9, the text 0xBE,
/// sound 0x3B, the arena back to `[0x800DDA30]`). Counts +0x48.
pub fn name_entry(m: &mut Machine) {
    framed(m, 0x28, |m, _| {
        let cursor = menu_lbu(m, 0x5b);
        c2(m, 0x800d_a9a8, 2, cursor);
        let name = c2(m, 0x800d_5970, 0, cursor);
        let mut length = call(m, 0x800a_def8, [name, 0, 0, 0]);
        let ctx = context(m);
        let held = m.lw(off(ctx, 0x130));
        // Some(sound) at the end, or None.
        let tail: Option<u32> = 'act: {
            for (bit, mv) in [
                (8u32, key_right as fn(u32) -> Option<u32>),
                (4, key_left),
                (2, key_down),
                (1, key_up),
            ] {
                if held & bit == 0 {
                    continue;
                }
                let block = menu(m);
                let c = lbu(m, off(block, 0x5b));
                if let Some(next) = mv(c) {
                    m.sb(off(block, 0x5b), next as u8);
                } else if bit == 1 && menu_lbu(m, 0x5b) < 0x34 {
                    let block = menu(m);
                    m.sb(off(block, 0x5b), 0x30);
                }
                break 'act Some(0x3d);
            }
            let pressed = m.lw(off(ctx, 0x12c));
            if pressed & 0x40 != 0 {
                if lt(0, length) {
                    dialog_to(m, 1);
                    break 'act Some(0x59);
                }
                break 'act Some(0x30);
            }
            if pressed & 0x300 != 0 {
                let block = menu(m);
                let page = lbu(m, off(block, 0x5c)) ^ 1;
                m.sb(off(block, 0x5c), page as u8);
                break 'act Some(0x2f);
            }
            if pressed & 0x10 != 0 {
                let block = menu(m);
                let c = lbu(m, off(block, 0x5b));
                match c {
                    0x1a => {
                        if lt(0, length) {
                            dialog_to(m, 1);
                            break 'act Some(0x59);
                        }
                        break 'act Some(0x30);
                    }
                    0xd => {
                        if !lt(0, length) {
                            m.sb(name.wrapping_add(length), 0);
                            break 'act None;
                        }
                        length = length.wrapping_sub(1);
                        sound(m, 0x2e);
                        m.sb(name.wrapping_add(length), 0);
                        break 'act None;
                    }
                    0x31 => {
                        let block = menu(m);
                        let page = lbu(m, off(block, 0x5c)) ^ 1;
                        m.sb(off(block, 0x5c), page as u8);
                        break 'act Some(0x2f);
                    }
                    0x33 => {
                        let block = menu(m);
                        m.sb(off(block, 0x5b), 0);
                        let block = menu(m);
                        m.sh(off(block, 0xc), 2);
                        m.sh(off(block, 0xe), 0);
                        m.sh(off(block, 0x10), 0);
                        c2(m, 0x800d_a77c, 7, 0);
                        c2(m, 0x800d_a77c, 9, 0);
                        c2(m, 0x800d_a828, 0xbe, 0);
                        sound(m, 0x3b);
                        let mark = m.lw(0x800d_da30);
                        arena::reset(m, mark);
                        break 'act None;
                    }
                    _ => {}
                }
                if !lt(length, 8) {
                    break 'act None;
                }
                let key = keyboard_char(m);
                if key == 0x20 && length == 0 {
                    break 'act Some(0x30);
                }
                let key = keyboard_char(m);
                m.sb(name.wrapping_add(length), key as u8);
                m.sb(length.wrapping_add(name).wrapping_add(1), 0);
                break 'act Some(0x2f);
            }
            if pressed & 0x20 != 0 {
                if lt(0, length) {
                    length = length.wrapping_sub(1);
                    sound(m, 0x2e);
                    m.sb(name.wrapping_add(length), 0);
                    break 'act None;
                }
                let block = menu(m);
                if lbu(m, off(block, 0x5b)) == 0x33 {
                    break 'act None;
                }
                m.sb(off(block, 0x5b), 0x33);
                break 'act Some(0x3d);
            }
            None
        };
        if let Some(id) = tail {
            sound(m, id);
        }
        count_up(m, 0x48);
    });
}

/// The character under the keyboard cursor: byte +4 of entry +0x5B of
/// the page table `MENU[+0x64 + 4 * page]` (8-byte entries).
fn keyboard_char(m: &mut Machine) -> u32 {
    let block = menu(m);
    let page = lbu(m, off(block, 0x5c)) << 2;
    let cursor = lbu(m, off(block, 0x5b));
    let table = m.lw(off(block.wrapping_add(page), 0x64));
    lbu(m, off((cursor << 3).wrapping_add(table), 4))
}

/// 800CEB20 (name confirmation, open): dialog step up; panel 0xC, the
/// text 0x105E, icons 1 (0x10) and 4 (0x16), the YES/NO choice
/// (800DBF6C at (0x6F, 0x96)) and 800DA49C(0xE).
pub fn name_confirm_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0xc, 0);
        c2(m, 0x800d_a828, 0x105e, 0);
        c2(m, 0x800d_a9a8, 1, 0x10);
        c2(m, 0x800d_a9a8, 4, 0x16);
        c4(m, 0x800d_bf6c, 0x6f, 0x96, 1, 0);
        c2(m, 0x800d_a49c, 0xe, 0x96);
    });
}

/// 800CEB90 (name confirmation): circle accepts (dialog 2; the name into
/// the context's +0x948, [`progress_reset`], context +0x964 bit 0
/// cleared, sound 0x59); cross goes back (dialog 0, sound 0x3B).
pub fn name_confirm(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if ctx_lw(m, 0x128) & 0x10 != 0 && ctx_lw(m, 0x12c) & 0x10 != 0 {
            let block = menu(m);
            m.sh(off(block, 0x4c), 2);
            m.sh(off(block, 0x4e), 0);
            let name = c2(m, 0x800d_5970, 0, 0);
            let ctx = context(m);
            call(m, 0x800a_dea8, [off(ctx, 0x948), name, 0, 0]);
            progress_reset(m);
            let ctx = context(m);
            let flags = m.lw(off(ctx, 0x964));
            m.sw(off(ctx, 0x964), flags & !1);
            sound(m, 0x59);
        }
        if held_and_pressed(m, 0x20) {
            dialog_to(m, 0);
            sound(m, 0x3b);
        }
    });
}

// ---- screen 3: difficulty and the configuration prompt ------------------------

/// 800CEC70 (difficulty, open): dialog step up; panel 0xA, the text
/// 0x43E, cursor 0xF, icons 1 (0xB) and 4 (6), the choice box
/// (800DBF6C at (0x40, 0x32)) and 800DA49C(0x10).
pub fn difficulty_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x48), 0);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0xa, 0);
        c2(m, 0x800d_a828, 0x43e, 0);
        c4(m, 0x800d_a838, 0xf, 1, 0, 0x3c);
        c2(m, 0x800d_a9a8, 1, 0xb);
        c2(m, 0x800d_a9a8, 4, 6);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0x10, 0x32);
    });
}

/// 800CECF8 (difficulty): "easy", "normal", "hard" (800D8AA0); the
/// difficulty is the context's +0x394 (clamped to 0..2), moved by pad
/// +0x130 bits 1 and 0 modulo 3 (sound 0x3D); circle continues (dialog
/// 3, sound 0x59), cross goes back (dialog 0, sound 0x3B); icons 6..8.
pub fn difficulty(m: &mut Machine) {
    framed(m, 0x20, |m, _| {
        const THIRD: u32 = 0x5555_5556;
        let ctx = context(m);
        let mut level = m.lw(off(ctx, 0x394));
        let held = m.lw(off(ctx, 0x130));
        let pressed = m.lw(off(ctx, 0x12c));
        if lt(level, 0) {
            level = 0;
        } else if !lt(level, 3) {
            level = 2;
        }
        c2(m, 0x800d_7d08, 4, 0);
        c4(m, 0x800d_7c60, 0x80, 0x80, 0x80, 0);
        c2(m, 0x800d_7c88, 2, 0);
        for (y, text) in [
            (0xa6, 0x800c_bff4),
            (0xda, 0x800c_bffc),
            (0x10e, 0x800c_c004),
        ] {
            c2(m, 0x800d_7c40, 0x6a, y);
            c2(m, 0x800d_8aa0, text, y);
        }
        let mod3 = |v: u32| {
            let q = mulhi(v, THIRD).wrapping_sub(sra(v, 31));
            v.wrapping_sub(q.wrapping_mul(3))
        };
        if pressed & 0x20 != 0 {
            dialog_to(m, 0);
            sound(m, 0x3b);
        } else if pressed & 0x10 != 0 {
            dialog_to(m, 3);
            sound(m, 0x59);
        } else if held & 3 != 0 {
            if held & 2 != 0 {
                level = mod3(level.wrapping_add(1));
            }
            if held & 1 != 0 {
                level = mod3(level.wrapping_add(2));
            }
            let ctx = context(m);
            m.sw(off(ctx, 0x394), level);
            sound(m, 0x3d);
        }
        for k in 0..3u32 {
            c2(m, 0x800d_a9a8, 6 + k, u32::from(k != level));
        }
        count_up(m, 0x48);
    });
}

/// 800CEECC (configuration prompt, open): dialog step up, +0x50 and the
/// countdown cleared; panel 0xD, the text 0x2056, icons 1 (0x11) and 4
/// (0x17), the YES/NO choice (800DBF6C at (0x40, 0x32)) and
/// 800DA49C(0x11).
pub fn configure_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x50), 0);
        m.sw(off(block, 0x48), 0);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0xd, 0);
        c2(m, 0x800d_a828, 0x2056, 0);
        c2(m, 0x800d_a9a8, 1, 0x11);
        c2(m, 0x800d_a9a8, 4, 0x17);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0x11, 0x32);
    });
}

/// 800CEF44 (configuration prompt): circle continues (dialog 4, sound
/// 0x59), else cross skips (dialog 7, sound 0x3B). Counts +0x48.
pub fn configure(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        if held_and_pressed(m, 0x10) {
            dialog_to(m, 4);
            sound(m, 0x59);
        } else if held_and_pressed(m, 0x20) {
            dialog_to(m, 7);
            sound(m, 0x3b);
        }
        count_up(m, 0x48);
    });
}

/// 800CF004 (sound settings, open): dialog step up, countdown cleared;
/// panel 0xE, the text 0x4016, icon 1 (0x12), the settings box
/// (800DBF6C at (0x40, 0x32)) and 800DA49C(0x12).
pub fn sound_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        m.sw(off(block, 0x48), 0);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0xe, 0);
        c2(m, 0x800d_a828, 0x4016, 0);
        c2(m, 0x800d_a9a8, 1, 0x12);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0x12, 0x32);
    });
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800c_d9a8 => {
            link_open(m);
            0
        }
        0x800c_db64 => {
            link_menu(m);
            0
        }
        0x800c_e24c => {
            account_open(m);
            0
        }
        0x800c_e290 => {
            account_dialog(m, args);
            0
        }
        0x800c_e3f4 => {
            progress_reset(m);
            0
        }
        0x800c_e518 => {
            name_open(m);
            0
        }
        0x800c_e5ac => {
            name_entry(m);
            0
        }
        0x800c_eb20 => {
            name_confirm_open(m);
            0
        }
        0x800c_eb90 => {
            name_confirm(m);
            0
        }
        0x800c_ec70 => {
            difficulty_open(m);
            0
        }
        0x800c_ecf8 => {
            difficulty(m);
            0
        }
        0x800c_eecc => {
            configure_open(m);
            0
        }
        0x800c_ef44 => {
            configure(m);
            0
        }
        0x800c_f004 => {
            sound_open(m);
            0
        }
        _ => return None,
    })
}
