//! JP overlay 1, part 1: the new account's sound, brightness and screen
//! position settings (800CF06C..800CFBD0).
//!
//! The dialogs run in the screen of the account path; their state is the
//! menu block's dialog +0x4C/+0x4E (see
//! [`ov1_p1_screens`](super::ov1_p1_screens)). Sound settings are the
//! context's +0x398 (S.E. level), +0x399 (BGM level) and +0x39A bit 0
//! (mono); the screen position is the display block's +8/+0xC.
use super::ov1_p1::{
    DISPLAY, MODULE, call, context, ctx_lw, display, framed, lbu, lh, lhu, lt, menu, menu_lw,
    mulhi, off, sra,
};
use super::ov1_p1_screens::{c2, c4, count_up, dialog_step_up, dialog_to, sound};
use crate::{common::loaded_module, machine::Machine};

/// `[0x800DD4C0]`: the sound-settings row (0 mono/stereo, 1 S.E. level,
/// 2 BGM level).
pub const ROW: u32 = 0x800d_d4c0;

/// The bar length of a 0..255 level: `level * 221 / 255`, as the
/// original's reciprocal multiply (0x80808081, shift 7).
fn bar_length(level: u32) -> u32 {
    let v = level.wrapping_mul(221);
    sra(mulhi(v, 0x8080_8081).wrapping_add(v), 7).wrapping_sub(sra(v, 31))
}

/// One level bar (800D9400 twice, through the stack rectangle at
/// `sp+0x10`): the black 4-pixel marker at 0x45 + length and the bar of
/// that length from 0x46 (colour (0xC8, 0xC8, 0xC8) on the selected row,
/// else (8, 0xC8, 0xC0)), `y` its row.
fn level_bar(m: &mut Machine, sp: u32, field: u32, y: u32, row: u32, frame: u32) {
    let ctx = context(m);
    let length = bar_length(lbu(m, off(ctx, field)));
    m.sh(off(sp, 0x12), y as u16);
    m.sb(off(sp, 0x1a), 0);
    m.sb(off(sp, 0x19), 0);
    m.sb(off(sp, 0x18), 0);
    m.sh(off(sp, 0x14), 4);
    m.sh(off(sp, 0x16), 0x40);
    m.sh(off(sp, 0x10), length.wrapping_add(0x45) as u16);
    let packet = menu_lw(m, 8);
    let next = call(m, 0x800d_9400, [off(sp, 0x10), packet, off(frame, 4), 0]);
    let block = menu(m);
    m.sw(off(block, 8), next);
    m.sh(off(sp, 0x10), 0x46);
    m.sh(off(sp, 0x12), y as u16);
    let (r, b) = if m.lw(ROW) == row {
        (0xc8, 0xc8)
    } else {
        (8, 0xc0)
    };
    m.sb(off(sp, 0x18), r);
    m.sb(off(sp, 0x19), 0xc8);
    m.sb(off(sp, 0x1a), b);
    let ctx = context(m);
    let length = bar_length(lbu(m, off(ctx, field)));
    m.sh(off(sp, 0x16), 0x40);
    m.sh(off(sp, 0x14), length as u16);
    let packet = menu_lw(m, 8);
    let next = call(m, 0x800d_9400, [off(sp, 0x10), packet, off(frame, 4), 0]);
    let block = menu(m);
    m.sw(off(block, 8), next);
}

/// Toggles bit 0 of the context's +0x39A (mono).
fn toggle_mono(m: &mut Machine, ctx: u32) {
    let v = lbu(m, off(ctx, 0x39a));
    let v = if v & 1 != 0 { v & 0xfe } else { v | 1 };
    m.sb(off(ctx, 0x39a), v as u8);
}

/// A level (context `field`) moved by pad bits 3 (+0x10) and 2 (-0x10),
/// clamped to 0..0xFF; a change plays sound 0x2E, then stores.
fn move_level(m: &mut Machine, ctx: u32, held: u32, field: u32) {
    let mut level = lbu(m, off(ctx, field));
    if held & 8 != 0 {
        level = level.wrapping_add(0x10);
    }
    if held & 4 != 0 {
        level = level.wrapping_sub(0x10);
    }
    if !lt(level, 0x100) {
        level = 0xff;
    } else if lt(level, 0) {
        level = 0;
    }
    let now = context(m);
    if lbu(m, off(now, field)) == level {
        return;
    }
    sound(m, 0x2e);
    let ctx = context(m);
    m.sb(off(ctx, field), level as u8);
}

/// 800CF06C (sound settings): the frame gauge (800DA340's value scaled
/// to 7 * (15v / 16383)) and the two level bars; icons 6 and 7 for
/// stereo/mono (2 for the inactive one); the CD audio mode (8002BD28,
/// 8002BD5C); the row moves with pad +0x130 bits 1 and 0 (sound 0x3D);
/// bits 3/2 toggle mono on row 0 (sound 0x3D) or move the row's level;
/// circle continues (dialog 6, sound 0x59), cross goes back (dialog 3,
/// sound 0x3B); icons 8..0xA, the row's icon on. Counts +0x48.
pub fn sound_settings(m: &mut Machine) {
    framed(m, 0x60, |m, sp| {
        let frame = call(m, 0x800d_a6fc, [0; 4]);
        let gauge = call(m, 0x800d_a340, [0; 4]);
        let v = (gauge << 4).wrapping_sub(gauge);
        m.sh(off(sp, 0x28), 0x44);
        m.sh(off(sp, 0x2a), 0xe0);
        m.sh(off(sp, 0x2e), 0xc);
        m.sb(off(sp, 0x30), 0xc8);
        m.sb(off(sp, 0x31), 0xc8);
        m.sb(off(sp, 0x32), 0xc8);
        let q = sra(mulhi(v, 0x8001_0003).wrapping_add(v), 14).wrapping_sub(sra(v, 31));
        let width = (q << 3).wrapping_sub(q);
        m.sh(off(sp, 0x2c), width as u16);
        let frame2 = call(m, 0x800d_a6fc, [0; 4]);
        let packet = menu_lw(m, 8);
        let next = call(m, 0x800d_9400, [off(sp, 0x28), packet, frame2, 0]);
        let block = menu(m);
        let ctx = context(m);
        m.sw(off(block, 8), next);
        let icon = if lbu(m, off(ctx, 0x39a)) & 1 != 0 {
            2
        } else {
            u32::from(m.lw(ROW) != 0)
        };
        c2(m, 0x800d_a9a8, 6, icon);
        let ctx = context(m);
        let icon = if lbu(m, off(ctx, 0x39a)) & 1 == 0 {
            2
        } else {
            u32::from(m.lw(ROW) != 0)
        };
        c2(m, 0x800d_a9a8, 7, icon);
        let ctx = context(m);
        let mono = u32::from(lbu(m, off(ctx, 0x39a)) & 1 != 0);
        call(m, 0x8002_bd28, [mono, 0, 0, 0]);
        call(m, 0x8002_bd5c, [0, 0, 0, 0]);
        level_bar(m, sp, 0x398, 0xe5, 1, frame);
        level_bar(m, sp, 0x399, 0x133, 2, frame);
        if ctx_lw(m, 0x130) & 2 != 0 {
            let row = m.lw(ROW);
            m.sw(ROW, if lt(row, 2) { row.wrapping_add(1) } else { 0 });
            sound(m, 0x3d);
        }
        if ctx_lw(m, 0x130) & 1 != 0 {
            let row = m.lw(ROW);
            m.sw(ROW, if lt(0, row) { row.wrapping_sub(1) } else { 2 });
            sound(m, 0x3d);
        }
        let ctx = context(m);
        let held = m.lw(off(ctx, 0x130));
        if held & 0xc != 0 {
            match m.lw(ROW) {
                0 => {
                    if held & 8 != 0 {
                        toggle_mono(m, ctx);
                    }
                    if held & 4 != 0 {
                        let ctx = context(m);
                        toggle_mono(m, ctx);
                    }
                    sound(m, 0x3d);
                }
                1 => move_level(m, ctx, held, 0x398),
                2 => move_level(m, ctx, held, 0x399),
                _ => {}
            }
        }
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            dialog_to(m, 6);
            sound(m, 0x59);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 3);
            sound(m, 0x3b);
        }
        for icon in [8, 9, 0xa] {
            c2(m, 0x800d_a9a8, icon, 0);
        }
        let row = m.lw(ROW);
        c2(m, 0x800d_a9a8, row.wrapping_add(8), 1);
        count_up(m, 0x48);
    });
}

/// 800CF750 (brightness, open): dialog step up; panel 0xF, the text
/// 0x8010, icon 1 (0x14), the box (800DBF6C at (0x40, 0x32), 0, 0) and
/// 800DA49C(0x13).
pub fn brightness_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0xf, 0);
        c2(m, 0x800d_a828, 0x8010, 0);
        c2(m, 0x800d_a9a8, 1, 0x14);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 0, 0);
        c2(m, 0x800d_a49c, 0x13, 0x32);
    });
}

/// 800CF7B4 (screen position): pad +0x130 bits 3/2 move the display
/// offset +8 right/left within -11..12, bits 1/0 the offset +0xC down/up
/// within 0..8 (sound 0x2E); icons 6..9 show the direction; a reset
/// (bit 4 of the context's +0x124 with +0x68 > 0x80, or bit 0x1000 of
/// +0x60 with sound 0x30) puts the offset to (-2, 0); display flag 2 on
/// an ODE frame (800A8EF0 = 1) and on circle/cross; circle continues
/// (dialog 7, sound 0x59), cross goes back (dialog 6, sound 0x3B).
pub fn screen_position(m: &mut Machine) {
    framed(m, 0x28, |m, _| {
        let ctx = context(m);
        let held = m.lw(off(ctx, 0x130));
        let request = display(m);
        let x = m.lw(off(request, 8));
        let y = m.lw(off(request, 0xc));
        let (mut dx, mut dy) = (0u32, 0u32);
        if held & 0xf != 0 {
            if held & 8 != 0 {
                dx = u32::from(lt(x, 0xc));
            }
            if held & 4 != 0 && !lt(x, 0xffff_fff5) {
                dx = u32::MAX;
            }
            if held & 2 != 0 && lt(y, 8) {
                dy = 1;
            }
            if held & 1 != 0 && lt(0, y) {
                dy = u32::MAX;
            }
            if dx != 0 || dy != 0 {
                sound(m, 0x2e);
                let request = display(m);
                m.sw(off(request, 8), x.wrapping_add(dx));
                m.sw(off(request, 0xc), y.wrapping_add(dy));
            }
        }
        c2(m, 0x800d_a9a8, 6, u32::from(dy == u32::MAX));
        c2(m, 0x800d_a9a8, 7, u32::from(dx == 1));
        c2(m, 0x800d_a9a8, 8, u32::from(dy == 1));
        c2(m, 0x800d_a9a8, 9, u32::from(dx == u32::MAX));
        let ctx = context(m);
        if m.lw(off(ctx, 0x124)) & 0x10 != 0 {
            if !lt(lh(m, off(ctx, 0x68)), 0x81) {
                let request = display(m);
                m.sw(off(request, 8), 0xffff_fffe);
                m.sw(off(request, 0xc), 0);
            }
        } else if lhu(m, off(ctx, 0x60)) & 0x1000 != 0 {
            let request = display(m);
            m.sw(off(request, 8), 0xffff_fffe);
            m.sw(off(request, 0xc), 0);
            sound(m, 0x30);
        }
        if call(m, 0x800a_8ef0, [0; 4]) == 1 {
            let request = display(m);
            let flags = m.lw(request);
            m.sw(request, flags | 2);
        }
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x30 != 0 {
            let request = display(m);
            let flags = m.lw(request);
            m.sw(request, flags | 2);
            if pressed & 0x10 != 0 {
                dialog_to(m, 7);
                sound(m, 0x59);
            } else if pressed & 0x20 != 0 {
                dialog_to(m, 6);
                sound(m, 0x3b);
            }
        }
    });
}

/// 800CFA34 (screen-position prompt, open): dialog step up; the text
/// 0x10016, icon 1 (0x13), the box (800DBF6C at (0x40, 0x32)) and
/// 800DA49C(0x14).
pub fn position_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a828, 0x0001_0016, 0);
        c2(m, 0x800d_a9a8, 1, 0x13);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0x14, 0x32);
    });
}

/// 800CFA94 (brightness): circle continues (dialog 5, the display offset
/// kept at menu +0xDC/+0xDE, sound 0x59), cross goes back (dialog 4,
/// sound 0x3B); then 800DBB30.
pub fn brightness(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            let block = menu(m);
            let request = m.lw(DISPLAY);
            m.sh(off(block, 0x4c), 5);
            m.sh(off(block, 0x4e), 0);
            let v = lhu(m, off(request, 8));
            m.sh(off(block, 0xdc), v as u16);
            let v = lhu(m, off(request, 0xc));
            m.sh(off(block, 0xde), v as u16);
            sound(m, 0x59);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 4);
            sound(m, 0x3b);
        }
        call(m, 0x800d_bb30, [0; 4]);
    });
}

/// 800CFB2C (save prompt, open): dialog step up; panel 0x11, the text
/// 0x20056, cursor 2, icons 2 (1), 1 and 4 (0x16), the YES/NO choice
/// (800DBF6C at (0x40, 0x32)), 800DA49C(0x15); +0xD1 = 1.
pub fn save_prompt_open(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let block = menu(m);
        dialog_step_up(m, block);
        c2(m, 0x800d_a77c, 0x11, 0);
        c2(m, 0x800d_a828, 0x0002_0056, 0);
        c4(m, 0x800d_a838, 2, 3, u32::MAX, u32::MAX);
        c2(m, 0x800d_a9a8, 2, 1);
        c2(m, 0x800d_a9a8, 1, 0x16);
        c2(m, 0x800d_a9a8, 4, 0x16);
        c4(m, 0x800d_bf6c, 0x40, 0x32, 1, 0);
        c2(m, 0x800d_a49c, 0x15, 0x32);
        let block = menu(m);
        m.sb(off(block, 0xd1), 1);
    });
}

/// 800CFBD0 (save prompt): circle saves (dialog 9, sound 0x59), cross
/// skips (dialog 8, sound 0x3B).
pub fn save_prompt(m: &mut Machine) {
    framed(m, 0x18, |m, _| {
        let pressed = ctx_lw(m, 0x12c);
        if pressed & 0x10 != 0 {
            dialog_to(m, 9);
            sound(m, 0x59);
        } else if pressed & 0x20 != 0 {
            dialog_to(m, 8);
            sound(m, 0x3b);
        }
    });
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let _ = args;
    Some(match target {
        0x800c_f06c => {
            sound_settings(m);
            0
        }
        0x800c_f750 => {
            brightness_open(m);
            0
        }
        0x800c_f7b4 => {
            screen_position(m);
            0
        }
        0x800c_fa34 => {
            position_open(m);
            0
        }
        0x800c_fa94 => {
            brightness(m);
            0
        }
        0x800c_fb2c => {
            save_prompt_open(m);
            0
        }
        0x800c_fbd0 => {
            save_prompt(m);
            0
        }
        _ => return None,
    })
}
