//! JP overlay 2: the disc-check screen (800EB944..800EC33F), translated from
//! the JP instructions. 800EB944 is class `changed` (PAL overlay-1
//! 800CB5E4, a different routine now); the rest are `new`.
//!
//! Block `[0x80112DF8]` (0x1C bytes, 800EB9EC): +0 phase and +1 substate
//! (rows of the callback table at 0x8010B30C), +2 leave request (1 back to
//! the hub, 2 leave to the next state), +4 a text-style flag, +7 the fade
//! timer, +8 and +9 flags, +0xC the current picture buffer, +0x10/+0x14
//! the two 0x2000-byte picture buffers, +0x18/+0x1A halfwords.
use super::ov2_p4::{At, call, lt, magic_div, sra};
use super::ov2_p4_text as text;
use crate::common::{CONTEXT, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112DF8]`: the disc-check block.
pub const BLOCK: u32 = 0x8011_2df8;
/// `[0x80112DC4]`: the hub block; `[0x80112DC0]` the hub's style tables
/// (+0x5C style bytes); `[0x80112DDC]` the map scene (+8 picture);
/// `[0x80112DA8]` the story state (+0x14 a disc-2 flag); `[0x80112DFC]`
/// the broadcast picture block.
const HUB: u32 = 0x8011_2dc4;
const STYLES: u32 = 0x8011_2dc0;
const MAP: u32 = 0x8011_2ddc;
const STORY: u32 = 0x8011_2da8;
const PICTURES: u32 = 0x8011_2dfc;
/// gp+0x2D4: the screen tint block (+0 flags, +0x10..+0x12 colour).
const TINT: u32 = 0x2d4;
/// 0x8010B30C: per phase, the row of substate callbacks.
const CALLBACKS: u32 = 0x8010_b30c;
/// Resident: the disc state (8002B528: 1 right disc, 3 lid open...), arena
/// allocation and reset.
const DISC_STATE: u32 = 0x8002_b528;
const ALLOC: u32 = 0x8002_7410;
const RESET: u32 = 0x8002_7538;
/// Overlay 2 (other parts): 80100FAC, 800DE908 (text slot), 800DEDF0
/// (layer switch), 800E3E3C, 800E3FCC, 800E25D4, 800DEE08 (style entry),
/// 800FF19C (background fade), 800E4858, 800DEF94 (style byte), 800DEEA8
/// (style halfword), 800E6DFC (picture blend), 800E419C.
const BACKDROP_ON: u32 = 0x8010_0fac;
const TEXT_SLOT: u32 = 0x800d_e908;
const LAYER: u32 = 0x800d_edf0;
const HUB_E3E3C: u32 = 0x800e_3e3c;
const HUB_E3FCC: u32 = 0x800e_3fcc;
const HUB_E25D4: u32 = 0x800e_25d4;
const STYLE_ENTRY: u32 = 0x800d_ee08;
const BACKGROUND_FADE: u32 = 0x800f_f19c;
const HUB_E4858: u32 = 0x800e_4858;
const STYLE_BYTE: u32 = 0x800d_ef94;
const STYLE_HALF: u32 = 0x800d_eea8;
const BLEND_PICTURE: u32 = 0x800e_6dfc;
const HUB_E419C: u32 = 0x800e_419c;
/// 800ECDA8: upload a TIM directory (this part).
const UPLOAD_TIMS: u32 = 0x800e_cda8;
/// Debug dump: "sim:\usr\makepack\pta\tim\" (27 bytes) and ".tim".
const DUMP_PATH: u32 = 0x800c_ce9c;
const DUMP_SUFFIX: u32 = 0x800c_ceb8;
/// libc strlen, strcat; BIOS open, write, close (resident).
const STRLEN: u32 = 0x800a_def8;
const STRCAT: u32 = 0x800a_dd88;
const OPEN: u32 = 0x800a_f3d8;
const WRITE: u32 = 0x800a_f408;
const CLOSE: u32 = 0x800a_f418;

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != 2 {
        return None;
    }
    Some(match target {
        0x800e_b944 => lid_open_eb944(m, args),
        0x800e_b98c => right_disc_eb98c(m, args),
        0x800e_b9ec => init_eb9ec(m),
        0x800e_bbd0 => fade_start_ebbd0(m),
        0x800e_bc3c => fade_ebc3c(m),
        0x800e_bd48 => tick_ebd48(m),
        0x800e_c11c => dump_tim_ec11c(m, args[0]),
        _ => return None,
    })
}

fn block(m: &mut Machine) -> u32 {
    m.lw(BLOCK)
}

/// 800EB944 (JP; PAL overlay-1 800CB5E4, changed): when the disc state
/// (8002B528) is 3, advance the substate. Returns the substate + 1 or the
/// disc state.
pub fn lid_open_eb944(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let state = call(m, DISC_STATE, args);
    let v = if state == 3 {
        let b = block(m);
        let next = u32::from(m.lbu(b.at(1))).wrapping_add(1);
        m.sb(b.at(1), next as u8);
        next
    } else {
        state
    };
    m.pop_frame(0x18);
    v
}

/// 800EB98C: when the disc state is 1 (the right disc), style byte 1 =
/// 0x49 and back two substates. Returns the substate - 2 or the state.
pub fn right_disc_eb98c(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let state = call(m, DISC_STATE, args);
    let v = if state == 1 {
        let styles = m.lw(STYLES);
        let bytes = m.lw(styles.at(0x5c));
        m.sb(bytes.at(1), 0x49);
        let b = block(m);
        let back = u32::from(m.lbu(b.at(1))).wrapping_sub(2);
        m.sb(b.at(1), back as u8);
        back
    } else {
        state
    };
    m.pop_frame(0x18);
    v
}

/// 800EB9EC: count the hub's step (+0x86), allocate the block and its two
/// picture buffers, the frame's buffer as the current picture, clear the
/// state; map backdrop on, text slot 0xC, layers 0..2 off; upload the hub's
/// pictures (+0xAC, `[+0x18] + 0x18`); the hub's 800E3E3C, 800E3FCC,
/// 800E25D4; style entry 15; background fade 3 (disc-2 story) or 0.
/// Returns the fade's result.
pub fn init_eb9ec(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let hub = m.lw(HUB);
    let steps = m.lhu(hub.at(0x86)).wrapping_add(1);
    m.sh(hub.at(0x86), steps);
    let b = call(m, ALLOC, [0x1c, 0, 0, 0]);
    m.sw(BLOCK, b);
    for i in 0..2u32 {
        let buffer = call(m, ALLOC, [0x2000, 0, 0, 0]);
        let b = block(m);
        m.sw(b.wrapping_add(i << 2).at(0x10), buffer);
    }
    let context = m.gw(CONTEXT);
    let index = m.lw(context.at(4));
    let b = block(m);
    let current = m.lw(b.wrapping_add(index << 2).at(0x10));
    m.sb(b, 0);
    let again = block(m);
    m.sw(b.at(0xc), current);
    m.sb(again.at(1), 0);
    for offset in 2..8 {
        let b = block(m);
        m.sb(b.at(offset), 0);
    }
    let b = block(m);
    m.sb(b.at(8), 1);
    let b = block(m);
    m.sb(b.at(9), 0);
    let b = block(m);
    m.sb(b.at(0xa), 0);
    let b = block(m);
    m.sh(b.at(0x18), 0);
    m.sh(b.at(0x1a), 0);
    call(m, BACKDROP_ON, [0; 4]);
    let hub = m.lw(HUB);
    let entry = m.lw(hub.at(0x158));
    call(m, TEXT_SLOT, [entry, 0xc, 1, 0]);
    call(m, LAYER, [0, 0, 0, 0]);
    call(m, LAYER, [1, 0, 0, 0]);
    call(m, LAYER, [2, 0, 0, 0]);
    let hub = m.lw(HUB);
    let pictures = m.lw(hub.at(0xac));
    call(m, UPLOAD_TIMS, [pictures, 0, 0, 0]);
    let hub = m.lw(HUB);
    let table = m.lw(hub.at(0x18));
    let pictures = m.lw(table.at(0x18));
    call(m, UPLOAD_TIMS, [pictures, 0, 0, 0]);
    call(m, HUB_E3E3C, [0; 4]);
    call(m, HUB_E3FCC, [0; 4]);
    call(m, HUB_E25D4, [0; 4]);
    call(m, STYLE_ENTRY, [0xf, 1, 0, 0x14]);
    let story = m.lw(STORY);
    let fade = if m.lw(story.at(0x14)) != 0 { 3 } else { 0 };
    let v = call(m, BACKGROUND_FADE, [fade, 0, 0, 0]);
    m.pop_frame(0x20);
    v
}

/// 800EBBD0: unless the story's disc-2 flag is set: count the hub's step,
/// and start the fade timer (block +7) at 0x20 when the tint red is 0x8F,
/// else at 1. Returns the timer, or the flag.
pub fn fade_start_ebbd0(m: &mut Machine) -> u32 {
    let story = m.lw(STORY);
    let flag = m.lw(story.at(0x14));
    if flag != 0 {
        return flag;
    }
    let hub = m.lw(HUB);
    let steps = m.lhu(hub.at(0x86));
    let tint = m.gw(TINT);
    m.sh(hub.at(0x86), steps.wrapping_add(1));
    let red = m.lbu(tint.at(0x10));
    let timer = if red == 0x8f { 0x20 } else { 1 };
    let b = block(m);
    m.sb(b.at(7), timer);
    u32::from(timer)
}

/// Signed `x / 32` as compiled.
fn div32(x: u32) -> u32 {
    sra(if lt(x, 0) { x.wrapping_add(0x1f) } else { x }, 5)
}

/// 800EBC3C: one fade step: the timer counts down (at 0 the hub's step is
/// counted); the tint colour is (143, 135, 83) * timer / 32 and its flag 8
/// set. Returns the tint flags.
pub fn fade_ebc3c(m: &mut Machine) -> u32 {
    let b = block(m);
    let timer = u32::from(m.lbu(b.at(7))).wrapping_sub(1);
    m.sb(b.at(7), timer as u8);
    if timer & 0xff == 0 {
        let hub = m.lw(HUB);
        let steps = m.lhu(hub.at(0x86));
        m.sh(hub.at(0x86), steps.wrapping_add(1));
    }
    for (offset, scale) in [(0x10, 143u32), (0x11, 135), (0x12, 83)] {
        let b = block(m);
        let timer = u32::from(m.lbu(b.at(7)));
        let tint = m.gw(TINT);
        m.sb(tint.at(offset), div32(timer.wrapping_mul(scale)) as u8);
    }
    let tint = m.gw(TINT);
    let flags = m.lw(tint) | 8;
    m.sw(tint, flags);
    flags
}

/// The current picture `[block + 0xC]` from the hub mode's picture: modes
/// 0, 2, 3 the hub's +0x274, modes 1, 4 the map's +8 (800EBD90 / 800EBDAC).
fn load_picture(m: &mut Machine) {
    let hub = m.lw(HUB);
    let mode = m.lw(hub.at(0xe0));
    match mode {
        0 | 2 | 3 => {
            let hub = m.lw(HUB);
            let b = block(m);
            let picture = m.lw(hub.at(0x274));
            m.sw(b.at(0xc), picture);
        }
        1 | 4 => {
            let map = m.lw(MAP);
            let b = block(m);
            let picture = m.lw(map.at(8));
            m.sw(b.at(0xc), picture);
        }
        _ => {}
    }
}

/// The picture back (800EBF94 / 800EBFB0).
fn store_picture(m: &mut Machine) {
    let hub = m.lw(HUB);
    let mode = m.lw(hub.at(0xe0));
    match mode {
        0 | 2 | 3 => {
            let b = block(m);
            let hub = m.lw(HUB);
            let picture = m.lw(b.at(0xc));
            m.sw(hub.at(0x274), picture);
        }
        1 | 4 => {
            let b = block(m);
            let map = m.lw(MAP);
            let picture = m.lw(b.at(0xc));
            m.sw(map.at(8), picture);
        }
        _ => {}
    }
}

/// 800EBD48: the per-frame tick. The hub's picture in, the hub's 800E4858;
/// style bytes 0x28 (pad bit 0x10 held), 0x10 (block +4, while +8 is 0;
/// else style 0x10 = 2), 0x11 = 0; the context's message (+0x948) in font
/// 4 (blend 2, squeezed in 10..20 at line style 15, grey) at (50, 73);
/// the phase/substate callback (with the block); unless block +9 is 1, the
/// picture blended (800E6DFC at (0xE3, 0x157) with the hub's +8 and the
/// frame counter's phase); the picture out. Leave request 1: the hub's
/// 800E419C, text slot 0x1B, hub step 7.0, the block released; in hub mode
/// 0, pad flag 1 of `[context + 0x96C]` (and context +0x954 = 2, hub
/// result 0x60000003) for missions 0x14/0x23/0x28/0x2D, else cleared; flag
/// 8 cleared. Leave request 2: 800E419C and hub result 0x60000004.
pub fn tick_ebd48(m: &mut Machine) -> u32 {
    let frame = m.push_frame(0x28);
    load_picture(m);
    call(m, HUB_E4858, [0; 4]);
    let context = m.gw(CONTEXT);
    let held = m.lw(context.at(0x124)) & 0x10 != 0;
    call(m, STYLE_BYTE, [0x28, u32::from(held), 0, 0]);
    let b = block(m);
    if m.lbu(b.at(8)) == 0 {
        let flag = u32::from(m.lbu(b.at(4)));
        call(m, STYLE_BYTE, [0x10, flag, 0, 0]);
    } else {
        let styles = m.lw(STYLES);
        let bytes = m.lw(styles.at(0x5c));
        m.sb(bytes.at(0x10), 2);
    }
    call(m, STYLE_BYTE, [0x11, 0, 0, 0]);
    text::select_font_f35b8(m, 4);
    text::set_blend_f33f8(m, 2);
    text::set_squeeze_f34c8(m, 1, 0xa, 0x14);
    let line = call(m, STYLE_HALF, [0xf, 0, 0, 0]);
    text::set_line_f34b0(m, line);
    text::set_color_f33d0(m, 0x80, 0x80, 0x80);
    text::set_pen_f33a8(m, 0x32, 0x49);
    // printf(message) with the leftover registers of 800F33A8 / 800F33D0 /
    // 800F34C8 (y << 16, blue << 16, 0x80110000): unused by a message
    // without conversions, stored in the caller's frame.
    let context = m.gw(CONTEXT);
    text::printf_f4ae0(m, [context.at(0x948), 0x49 << 16, 0x80 << 16, 0x8011_0000]);
    let b = block(m);
    let phase = u32::from(m.lbu(b));
    let row = m.lw(CALLBACKS.wrapping_add(phase << 2));
    let substate = u32::from(m.lbu(b.at(1)));
    let callback = m.lw(row.wrapping_add(substate << 2));
    call(m, callback, [b, 0, 0, 0]);
    let b = block(m);
    if m.lbu(b.at(9)) != 1 {
        let context = m.gw(CONTEXT);
        let counter = m.lw(context.at(0x20));
        let hub = m.lw(HUB);
        let picture = m.lw(b.at(0xc));
        m.sw(frame.at(0x10), picture);
        let source = m.lw(hub.at(8));
        let phase = (counter as i32 % 64) as u32;
        let blended = call(m, BLEND_PICTURE, [0xe3, 0x157, source, phase << 6]);
        let b = block(m);
        m.sw(b.at(0xc), blended);
    }
    store_picture(m);
    let b = block(m);
    let leave = m.lbu(b.at(2));
    let v = match leave {
        1 => {
            call(m, HUB_E419C, [0; 4]);
            let hub = m.lw(HUB);
            let entry = m.lw(hub.at(0x15c));
            call(m, TEXT_SLOT, [entry, 0x1b, 0, 0]);
            let b = block(m);
            let hub = m.lw(HUB);
            m.sh(hub.at(0x84), 7);
            m.sh(hub.at(0x86), 0);
            call(m, RESET, [b, 0, 0, 0]);
            let hub = m.lw(HUB);
            if m.lw(hub.at(0xe0)) == 0 {
                let context = m.gw(CONTEXT);
                let mission = m.lh(context.at(0x38c));
                if matches!(mission, 0x14 | 0x23 | 0x28 | 0x2d) {
                    let context = m.gw(CONTEXT);
                    m.sh(context.at(0x954), 2);
                    let flags = m.lw(context.at(0x96c));
                    let hub = m.lw(HUB);
                    m.sw(context.at(0x96c), flags | 1);
                    m.sw(hub.at(0x94), 0x6000_0003);
                } else {
                    let context = m.gw(CONTEXT);
                    let flags = m.lw(context.at(0x96c));
                    m.sw(context.at(0x96c), flags & !1);
                }
            }
            let context = m.gw(CONTEXT);
            let flags = m.lw(context.at(0x96c)) & !8;
            m.sw(context.at(0x96c), flags);
            flags
        }
        2 => {
            call(m, HUB_E419C, [0; 4]);
            let hub = m.lw(HUB);
            m.sw(hub.at(0x94), 0x6000_0004);
            0x6000_0004
        }
        _ => 2,
    };
    m.pop_frame(0x28);
    v
}

/// 800EC11C (debug, unreached): write broadcast picture `index` as
/// "sim:\usr\makepack\pta\tim\NNNN.tim" (4-digit index) on the debug host
/// file system: the TIM header (id 0x10, flags 9), the CLUT block (0x20C
/// bytes at (0x300, 5), 256x1, from `[PICTURES] + 0x1AC` + 4) and the pixel
/// block (0x4C bytes at (0x380, 0x100), 4x8, from the index's pointer at
/// `[PICTURES] + 0x1AC` + 0x204). Returns close's result.
pub fn dump_tim_ec11c(m: &mut Machine, index: u32) -> u32 {
    let sp = m.push_frame(0x98);
    let path = sp.at(0x38);
    m.sw(sp.at(0x10), 0x10);
    m.sw(sp.at(0x14), 9);
    m.sw(sp.at(0x28), 0x20c);
    m.sh(sp.at(0x2c), 0x300);
    m.sh(sp.at(0x2e), 5);
    m.sh(sp.at(0x32), 1);
    m.sw(sp.at(0x18), 0x4c);
    m.sh(sp.at(0x1c), 0x380);
    m.sh(sp.at(0x20), 4);
    m.sh(sp.at(0x22), 8);
    m.sh(sp.at(0x30), 0x100);
    m.sh(sp.at(0x1e), 0x100);
    for i in 0..6u32 {
        let w = m.lw(DUMP_PATH + 4 * i);
        m.sw(path.at(4 * i), w);
    }
    let h = m.lhu(DUMP_PATH + 0x18);
    let c = m.lbu(DUMP_PATH + 0x1a);
    m.sh(path.at(0x18), h);
    m.sb(path.at(0x1a), c);
    let length = call(m, STRLEN, [path, 0, 0, 0]);
    let end = path.wrapping_add(length);
    let thousands = magic_div(index, 0x1062_4dd3, false, 6);
    m.sb(end, thousands.wrapping_add(0x30) as u8);
    let hundreds = magic_div(index, 0x51eb_851f, false, 5);
    m.sb(
        end.at(1),
        hundreds
            .wrapping_sub(thousands.wrapping_mul(10))
            .wrapping_add(0x30) as u8,
    );
    let tens = magic_div(index, 0x6666_6667, false, 2);
    m.sb(
        end.at(2),
        tens.wrapping_sub(hundreds.wrapping_mul(10))
            .wrapping_add(0x30) as u8,
    );
    m.sb(
        end.at(3),
        index.wrapping_sub(tens.wrapping_mul(10)).wrapping_add(0x30) as u8,
    );
    m.sb(end.at(4), 0);
    call(m, STRCAT, [path, DUMP_SUFFIX, 0, 0]);
    let fd = call(m, OPEN, [path, 0x202, 0, 0]);
    call(m, WRITE, [fd, sp.at(0x10), 8, 0]);
    call(m, WRITE, [fd, sp.at(0x28), 0xc, 0]);
    let pictures = m.lw(PICTURES);
    let clut = m.lw(pictures.at(0x1ac)).at(4);
    call(m, WRITE, [fd, clut, 0x200, 0]);
    call(m, WRITE, [fd, sp.at(0x18), 0xc, 0]);
    let pictures = m.lw(PICTURES);
    let table = m.lw(pictures.at(0x1ac));
    let pixels = m.lw(table.wrapping_add(index << 2).at(0x204));
    call(m, WRITE, [fd, pixels, 0x40, 0]);
    let v = call(m, CLOSE, [fd, 0, 0, 0]);
    m.pop_frame(0x98);
    v
}
