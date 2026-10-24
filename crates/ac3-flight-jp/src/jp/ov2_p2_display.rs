//! JP overlay 2, part 2: the hub's display configuration pages (the
//! brightness test pattern, the screen position and the game
//! configuration), translated from the JP instructions.
//!
//! The display record `[gp+0x2D4]` holds the screen offset (+8 x, -0xB..
//! 0xC; +0xC y, 0..8) and flag bit 2 of +0 (apply). The game configuration
//! edits a 4-byte record (state +0x13A: +0 vibration off, +1 controller
//! layout 0..2, +2 subtitles, +3 the cursor) and writes it back to context
//! +0x39C, +0x120 bits 0/1 and +0x95A; the sub-page step is state +0x140
//! (handlers at [`CONFIG_STEPS`]).
use super::ov2_p2::{MODULE, framed, lbu, lh, visual, work};
use super::ov2_p2_hub::STATE;
use crate::common::{CONTEXT, loaded_module};
use crate::dispatch;
use crate::machine::{Machine, SmallData};

/// The game configuration sub-page handlers by state +0x140.
pub const CONFIG_STEPS: u32 = 0x8010_acac;

/// 8002DB4C (resident, `script`): start sound effect `a0`.
const SOUND: u32 = 0x8002_db4c;
/// 800F7244 (overlay 2, part 5): draw a bar record.
const BAR: u32 = 0x800f_7244;
/// 800A8EF0 (resident SDK): GetODE.
const GET_ODE: u32 = 0x800a_8ef0;

fn state(m: &mut Machine) -> u32 {
    m.lw(STATE)
}
fn sound(m: &mut Machine, id: u32) -> u32 {
    dispatch::call(m, SOUND, [id, 0, 0, 0])
}
fn pad(m: &mut Machine, offset: u32) -> u32 {
    let context = m.gw(CONTEXT);
    m.lw(context.wrapping_add(offset))
}
fn press(m: &mut Machine, bit: u32, held: u32) -> bool {
    let context = m.gw(CONTEXT);
    m.lw(context.wrapping_add(0x12c)) & bit != 0 && m.lw(context.wrapping_add(0x128)) & held == 0
}
/// The work block's flag byte `index` = `value` (the flags re-read).
fn put_flag(m: &mut Machine, index: u32, value: u32) {
    let block = work(m);
    let flags = m.lw(block.wrapping_add(0x5c));
    m.sb(flags.wrapping_add(index), value as u8);
}
/// Leave a configuration page: fade slot 1 in, step 1, and the words
/// `first` into slot 0 (`None`: kept) and 0x1000 into slot 2.
fn leave(m: &mut Machine, first: Option<u32>) {
    super::ov2_p2::set_fade(m, 1, 1, 0, 0x1e);
    let record = state(m);
    m.sb(record.wrapping_add(1), 1);
    if let Some(value) = first {
        super::ov2_p2::set_word_178(m, 0, value);
    }
    super::ov2_p2::set_word_178(m, 2, 0x1000);
}

/// 800DB3F8: the brightness test pattern: a press of 0x30 leaves the page
/// (sound 0x3B); draws four bars (state record +0x10A, colors at +0x11A..)
/// in white, red, green and blue. Returns the last drawer's value.
pub fn brightness(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, _| {
        if pad(m, 0x12c) & 0x30 != 0 {
            leave(m, None);
            sound(m, 0x3b);
        }
        let record = state(m);
        m.sb(record.wrapping_add(0x11d), 0);
        let record = state(m);
        m.sh(record.wrapping_add(0x10a), 0x3c);
        m.sh(record.wrapping_add(0x112), 0x3c);
        m.sb(record.wrapping_add(0x11a), 0);
        let next = state(m);
        for (offset, value) in [
            (0x10c, 0x98),
            (0x10e, 0x41),
            (0x110, 0x98),
            (0x114, 0x140),
            (0x116, 0x41),
            (0x118, 0x140),
        ] {
            m.sh(record.wrapping_add(offset), value);
        }
        m.sb(next.wrapping_add(0x11b), 0);
        for (offset, value) in [
            (0x11c, 0),
            (0x11e, 0),
            (0x11f, 0),
            (0x120, 0),
            (0x122, 0xff),
            (0x123, 0),
            (0x124, 0),
            (0x126, 0xff),
            (0x127, 0),
            (0x128, 0),
        ] {
            let record = state(m);
            m.sb(record.wrapping_add(offset), value);
        }
        let mut last = draw_bar(m);
        for (x0, x1, colors) in [
            (0x48u32, 0x4du32, [0u8, 0xff, 0, 0, 0xff, 0]),
            (0x53, 0x58, [0, 0, 0xff, 0, 0, 0xff]),
            (0x63, 0x68, [0xff, 0xff, 0xff, 0xff, 0xff, 0xff]),
        ] {
            let record = state(m);
            m.sb(record.wrapping_add(0x122), colors[0]);
            let next = state(m);
            m.sw(record.wrapping_add(8), last);
            for (offset, value) in [
                (0x10a, x0),
                (0x10c, 0x98),
                (0x10e, x1),
                (0x110, 0x98),
                (0x112, x0),
                (0x114, 0x140),
                (0x116, x1),
                (0x118, 0x140),
            ] {
                m.sh(record.wrapping_add(offset), value as u16);
            }
            m.sb(next.wrapping_add(0x123), colors[1]);
            for (offset, value) in [
                (0x124, colors[2]),
                (0x126, colors[3]),
                (0x127, colors[4]),
                (0x128, colors[5]),
            ] {
                let record = state(m);
                m.sb(record.wrapping_add(offset), value);
            }
            last = draw_bar(m);
        }
        let record = state(m);
        m.sw(record.wrapping_add(8), last);
        last
    })
}

/// Draw the state record's bar (+0x10A) into ordering table [visual + 8]
/// + 4 (800F7244). Returns the new packet pointer (not stored).
fn draw_bar(m: &mut Machine) -> u32 {
    let record = state(m);
    let vis = visual(m);
    let bar = record.wrapping_add(0x10a);
    let ot = m.lw(vis.wrapping_add(8)).wrapping_add(4);
    let packet = m.lw(record.wrapping_add(8));
    dispatch::call(m, BAR, [bar, packet, ot, 0])
}

/// 800DB714: the screen position page: the repeated pad bits 8 / 4 / 2 /
/// 1 move the display offset ([gp+0x2D4] +8 up to 0xC / down to -0xB,
/// +0xC up to 8 / down to 0; flags 7, 9, 8, 6 show the moving direction;
/// sound 0x2E once), GetODE requests applying (flag bit 2); confirm keeps
/// and cancel restores the offset (state +0x106/+0x108) and leave the
/// page; otherwise the option pad (context +0x124 bit 0x10 with stick
/// +0x68 above 0x80, or pad +0x60 bit 0x1000) resets the offset to (-2, 0)
/// (sound 0x30 for the latter). Returns the original's last `v0`: the
/// sound call's value after a confirm or a pad +0x60 reset, the context
/// after a cancel, else the last pad test's value.
pub fn screen_position(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let mut moved = false;
        for (bit, offset, flag, up) in [
            (8, 8, 7, true),
            (4, 8, 9, false),
            (2, 0xc, 8, true),
            (1, 0xc, 6, false),
        ] {
            let mut value = 0;
            if pad(m, 0x130) & bit != 0 {
                let display = m.gw(0x2d4);
                let current = m.lw(display.wrapping_add(offset));
                let can = if up {
                    super::ov2_p2::less(current, if offset == 8 { 0xc } else { 8 })
                } else if offset == 8 {
                    !super::ov2_p2::less(current, (-0xb_i32) as u32)
                } else {
                    (current as i32) > 0
                };
                if can {
                    let next = if up {
                        current.wrapping_add(1)
                    } else {
                        current.wrapping_sub(1)
                    };
                    m.sw(display.wrapping_add(offset), next);
                    value = 1;
                }
            }
            put_flag(m, flag, value);
            if value == 1 {
                if !moved {
                    sound(m, 0x2e);
                }
                moved = true;
            }
        }
        if dispatch::call(m, GET_ODE, [0; 4]) != 0 {
            let display = m.gw(0x2d4);
            let flags = m.lw(display) | 2;
            m.sw(display, flags);
        }
        if press(m, 0x10, 0x2f) {
            leave(m, Some(3));
            let display = m.gw(0x2d4);
            let flags = m.lw(display) | 2;
            m.sw(display, flags);
            return sound(m, 0x3c);
        }
        if press(m, 0x20, 0x5f) {
            let record = state(m);
            let display = m.gw(0x2d4);
            let x = lh(m, record.wrapping_add(0x106));
            m.sw(display.wrapping_add(8), x);
            let y = lh(m, record.wrapping_add(0x108));
            m.sw(display.wrapping_add(0xc), y);
            leave(m, Some(3));
            let display = m.gw(0x2d4);
            let flags = m.lw(display) | 2;
            m.sw(display, flags);
            sound(m, 0x3b);
            let context = m.gw(CONTEXT);
            m.sw(context.wrapping_add(0x12c), 0);
            return context;
        }
        let context = m.gw(CONTEXT);
        if m.lw(context.wrapping_add(0x124)) & 0x10 != 0 {
            if !super::ov2_p2::less(lh(m, context.wrapping_add(0x68)), 0x81) {
                let display = m.gw(0x2d4);
                m.sw(display.wrapping_add(8), (-2_i32) as u32);
                m.sw(display.wrapping_add(0xc), 0);
            }
            let option = pad(m, 0x124) & 0x10;
            if option != 0 {
                return option;
            }
        }
        let context = m.gw(CONTEXT);
        if u32::from(m.lhu(context.wrapping_add(0x60))) & 0x1000 == 0 {
            return 0;
        }
        let display = m.gw(0x2d4);
        m.sw(display.wrapping_add(8), (-2_i32) as u32);
        m.sw(display.wrapping_add(0xc), 0);
        sound(m, 0x30)
    })
}

/// The configuration record's flags: +0x32 the cursor's value (+0, +1
/// plus 2, +2 plus 5), +0x34..+0x37 the record.
fn config_flags(m: &mut Machine, config: u32) {
    match lbu(m, config.wrapping_add(3)) {
        0 => {
            let value = lbu(m, config);
            put_flag(m, 0x32, value);
        }
        1 => {
            let value = lbu(m, config.wrapping_add(1)).wrapping_add(2);
            put_flag(m, 0x32, value);
        }
        2 => {
            let value = lbu(m, config.wrapping_add(2)).wrapping_add(5);
            put_flag(m, 0x32, value);
        }
        _ => {}
    }
}
/// Flags +0x34..+0x37 = the configuration record.
fn config_bytes(m: &mut Machine, config: u32) {
    for n in 0..4u32 {
        let value = lbu(m, config.wrapping_add(n));
        put_flag(m, 0x34 + n, value);
    }
}

/// 800DBAEC (sub-page 0, also hub update mode 16): load the configuration
/// record `config` from the context (+0x39C, +0x120 bits, +0x95A), cursor
/// 0, reset cell 0x10 of list 2, word 0x10000 into slot 2, and the flags.
/// Returns 1.
pub fn config_load(m: &mut Machine, config: u32) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = m.gw(CONTEXT);
        let off = if lbu(m, context.wrapping_add(0x39c)) == 1 {
            0
        } else {
            1
        };
        m.sb(config, off);
        let context = m.gw(CONTEXT);
        let bits = m.lw(context.wrapping_add(0x120));
        let layout = if bits & 1 == 0 {
            2
        } else if bits & 2 == 0 {
            1
        } else {
            0
        };
        m.sb(config.wrapping_add(1), layout);
        let context = m.gw(CONTEXT);
        let subtitles = lbu(m, context.wrapping_add(0x95a));
        if subtitles == 0 {
            m.sb(config.wrapping_add(2), 0);
        } else if subtitles == 1 {
            m.sb(config.wrapping_add(2), 1);
        }
        m.sb(config.wrapping_add(3), 0);
        super::ov2_p2::reset_cell(m, 2, 0x10);
        super::ov2_p2::set_word_178(m, 2, 0x1_0000);
        config_flags(m, config);
        put_flag(m, 0x33, 0);
        config_bytes(m, config);
        1
    })
}

/// `n % 3` as compiled (magic 0x55555556).
fn modulo3(n: u32) -> u32 {
    let high = ((i64::from(n as i32) * 0x5555_5556) >> 32) as u32;
    let q = high.wrapping_sub((n as i32 >> 31) as u32);
    n.wrapping_sub(q.wrapping_mul(3))
}

/// 800DBC94 (sub-page 1, also hub update mode 17): edit the
/// configuration record: the repeated pad bits 2 / 1 move the cursor (mod
/// 3), 8 / 4 change the setting (toggle, layout mod 3); sound 0x3D for
/// any. Returns 1 on confirm (sound 0x3C), 2 on cancel (sound 0x3B, press
/// cleared), else 0 after the flags (+0x33 shows held pad 1 / 2).
pub fn config_edit(m: &mut Machine, config: u32) -> u32 {
    framed(m, 0x20, |m, _| {
        let pressed = pad(m, 0x130);
        if pressed & 0xf != 0 {
            sound(m, 0x3d);
            if pressed & 2 != 0 {
                let cursor = modulo3(lbu(m, config.wrapping_add(3)).wrapping_add(1));
                m.sb(config.wrapping_add(3), cursor as u8);
            }
            if pressed & 1 != 0 {
                let cursor = modulo3(lbu(m, config.wrapping_add(3)).wrapping_add(2));
                m.sb(config.wrapping_add(3), cursor as u8);
            }
            if pressed & 0xc != 0 {
                match lbu(m, config.wrapping_add(3)) {
                    0 => {
                        let value = lbu(m, config) ^ 1;
                        m.sb(config, value as u8);
                    }
                    1 => {
                        let layout = lbu(m, config.wrapping_add(1));
                        let step = if pressed & 8 != 0 { 1 } else { 2 };
                        m.sb(
                            config.wrapping_add(1),
                            modulo3(layout.wrapping_add(step)) as u8,
                        );
                    }
                    2 => {
                        let value = u32::from(lbu(m, config.wrapping_add(2)) == 0);
                        m.sb(config.wrapping_add(2), value as u8);
                    }
                    _ => {}
                }
            }
        }
        if press(m, 0x10, 0x2f) {
            sound(m, 0x3c);
            return 1;
        }
        if press(m, 0x20, 0x5f) {
            sound(m, 0x3b);
            let context = m.gw(CONTEXT);
            m.sw(context.wrapping_add(0x12c), 0);
            return 2;
        }
        config_flags(m, config);
        let held = pad(m, 0x128);
        let value = if held & 1 != 0 {
            1
        } else if held & 2 != 0 {
            2
        } else {
            0
        };
        put_flag(m, 0x33, value);
        config_bytes(m, config);
        0
    })
}

/// 800DBFC4 (sub-page 2, also hub update mode 18): store the
/// configuration record `config` into the context. Returns 2.
pub fn config_store(m: &mut Machine, config: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let off = u8::from(lbu(m, config) == 0);
    m.sb(context.wrapping_add(0x39c), off);
    match lbu(m, config.wrapping_add(1)) {
        2 => {
            let context = m.gw(CONTEXT);
            let bits = m.lw(context.wrapping_add(0x120)) & !3;
            m.sw(context.wrapping_add(0x120), bits);
        }
        1 => {
            let context = m.gw(CONTEXT);
            let bits = (m.lw(context.wrapping_add(0x120)) | 1) & !2;
            m.sw(context.wrapping_add(0x120), bits);
        }
        0 => {
            let context = m.gw(CONTEXT);
            let bits = m.lw(context.wrapping_add(0x120)) | 3;
            m.sw(context.wrapping_add(0x120), bits);
        }
        _ => {}
    }
    match lbu(m, config.wrapping_add(2)) {
        0 => {
            let context = m.gw(CONTEXT);
            m.sb(context.wrapping_add(0x95a), 0);
        }
        1 => {
            let context = m.gw(CONTEXT);
            m.sb(context.wrapping_add(0x95a), 1);
        }
        _ => {}
    }
    2
}

/// 800DC0A0: run the configuration sub-page of state +0x140 on the record
/// at state +0x13A; 1 steps to the next sub-page, 2 leaves the page (step
/// 1, sub-page 0, fade slot 1 in, word 0x1000 into slot 2). Returns 2 or
/// the last call's value.
pub fn config_page(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let record = state(m);
        let step = m.lw(record.wrapping_add(0x140));
        let handler = m.lw((step << 2).wrapping_add(CONFIG_STEPS));
        let config = record.wrapping_add(0x13a);
        let result = match handler {
            0x800d_baec => config_load(m, config),
            0x800d_bc94 => config_edit(m, config),
            0x800d_bfc4 => config_store(m, config),
            _ => dispatch::call(m, handler, [config, 0, 0, 0]),
        };
        if result == 1 {
            let record = state(m);
            let step = m.lw(record.wrapping_add(0x140)).wrapping_add(1);
            m.sw(record.wrapping_add(0x140), step);
        }
        if result != 2 {
            return 2;
        }
        let record = state(m);
        m.sb(record.wrapping_add(1), 1);
        m.sw(record.wrapping_add(0x140), 0);
        super::ov2_p2::set_fade(m, 1, 1, 0, 0x1e);
        super::ov2_p2::set_word_178(m, 2, 0x1000)
    })
}

// ---- dispatch -------------------------------------------------------------------------------------

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let a0 = args[0];
    Some(match target {
        0x800d_b3f8 => brightness(m),
        0x800d_b714 => screen_position(m),
        0x800d_baec => config_load(m, a0),
        0x800d_bc94 => config_edit(m, a0),
        0x800d_bfc4 => config_store(m, a0),
        0x800d_c0a0 => config_page(m),
        _ => return None,
    })
}
