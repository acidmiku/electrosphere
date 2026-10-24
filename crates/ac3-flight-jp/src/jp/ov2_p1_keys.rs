//! JP overlay 2, part 1: the hub options' KEY CONFIG settings and the
//! controller-type choice of the first visit (none of these ran in the
//! captures; they are validated against the original with the oracle).
//!
//! - 800D1B6C / 800D1F54: load the five controller records of the context
//!   (+0x15C + 0x5C*n: halfwords +0x16..+0x3E the buttons of each action,
//!   +0x1AC the analog flags, +0x1AE.. the axes, +0x1B4 options) into the
//!   options block `K` and store them back.
//! - The first visit's phases (options menu phases 4..7): 800D2D74 wait,
//!   800D2DF0 choose the controller type (`K+5`), 800D2FA8 open it; and
//!   800D3088, KEY CONFIG's start.
//! - 800D3240 the type's text setup (800D31A8, 800D31B0, 800D31E0,
//!   800D3210), 800D32DC, 800D32FC an action's label, 800D1588 a textured
//!   quad.
use super::ov2::MODULE;
use super::ov2_p1::{FLAGS, SET_FLAG, TEXT_COLOR, TEXT_FONT, at, call, context, lh, lt, work};
use super::ov2_p1_options::{OPTIONS, type_index_d1b04};
use super::ov2_p1_pages::{
    CURSOR, INSTALL_BANK, PANEL_RESTART, PANEL_START, PANEL_STATE, SET_CURSOR, SET_LAMP,
};
use crate::common::loaded_module;
use crate::machine::Machine;

/// 8010A520: the button-assignment slot of each of the five controller
/// records (bytes).
pub const RECORD_SLOTS: u32 = 0x8010_a520;
/// 8010A508: the action of each context button halfword 0xB..0x1E (bytes).
pub const SLOT_ACTIONS: u32 = 0x8010_a508;
/// 8010A528: the context halfword of each action (0x1F: none).
pub const ACTION_SLOTS: u32 = 0x8010_a528;
/// 8010A540: the fifteen button masks (halfwords).
pub const BUTTON_MASKS: u32 = 0x8010_a540;
/// 8010A560: the codes of the four analog axes (halfwords).
pub const AXES: u32 = 0x8010_a560;
/// 800F34F4: text-layer setting (`value`).
pub const TEXT_SETTING: u32 = 0x800f_34f4;

fn options(m: &mut Machine) -> u32 {
    m.lw(OPTIONS)
}

/// High word of the unsigned 64-bit product (`multu` + `mfhi`).
fn mulhu(a: u32, b: u32) -> u32 {
    ((u64::from(a) * u64::from(b)) >> 32) as u32
}

/// 800D1588 (`x`, `y`, `rect`, `mode`, stack +0x10 `prim`): a textured
/// quad (POLY_FT4, code 0x2E, grey 0x80, clut 0x55A) over the RECT's size
/// at (`x`, `y`), texture coordinates from the RECT's position within its
/// 64 x 256 texture page, texture page from its position and `mode`'s
/// semi-transparency, linked into slot 1 of the frame's table `[W+8]`.
/// Returns the next primitive (`prim` + 0x28).
pub fn quad_d1588(m: &mut Machine, x: u32, y: u32, rect: u32, mode: u32) -> u32 {
    let prim = m.lw(at(m.sp, 0x10));
    m.sb(at(prim, 3), 9);
    m.sb(at(prim, 7), 0x2e);
    m.sh(at(prim, 8), x as u16);
    m.sh(at(prim, 0xa), y as u16);
    let width = u32::from(m.lhu(at(rect, 4)));
    m.sh(at(prim, 0x18), x as u16);
    let left = x.wrapping_sub(1);
    m.sh(at(prim, 0x12), y as u16);
    m.sh(at(prim, 0x10), width.wrapping_add(left) as u16);
    let height = u32::from(m.lhu(at(rect, 6)));
    let top = y.wrapping_sub(1);
    m.sh(at(prim, 0x1a), height.wrapping_add(top) as u16);
    let width = u32::from(m.lhu(at(rect, 4)));
    m.sh(at(prim, 0x20), width.wrapping_add(left) as u16);
    let height = u32::from(m.lhu(at(rect, 6)));
    m.sh(at(prim, 0x22), height.wrapping_add(top) as u16);
    // u: (x mod 64) * 4 (as a signed halfword); v: y mod 256
    let u = |m: &mut Machine| {
        let px = m.lh(rect) as i32;
        ((((px % 64) as u32) << 16) as i32 >> 14) as u32
    };
    let v = |m: &mut Machine| {
        let py = m.lh(at(rect, 2)) as i32;
        (py % 256) as u32
    };
    let u0 = u(m);
    m.sb(at(prim, 0xc), u0 as u8);
    let v0 = v(m);
    m.sb(at(prim, 0xd), v0 as u8);
    let u1 = u(m);
    let w = u32::from(m.lbu(at(rect, 4))).wrapping_sub(1);
    m.sb(at(prim, 0x14), u1.wrapping_add(w) as u8);
    let v1 = v(m);
    m.sb(at(prim, 0x15), v1 as u8);
    let u2 = u(m);
    m.sb(at(prim, 0x1c), u2 as u8);
    let v2 = v(m);
    let h = u32::from(m.lbu(at(rect, 6))).wrapping_sub(1);
    m.sb(at(prim, 0x1d), v2.wrapping_add(h) as u8);
    let u3 = u(m);
    let w = u32::from(m.lbu(at(rect, 4))).wrapping_sub(1);
    m.sb(at(prim, 0x24), u3.wrapping_add(w) as u8);
    let py = m.lh(at(rect, 2)) as i32;
    let h = u32::from(m.lbu(at(rect, 6)));
    m.sb(at(prim, 4), 0x80);
    m.sb(at(prim, 5), 0x80);
    m.sb(at(prim, 6), 0x80);
    let v3 = (py % 256) as u32;
    m.sb(at(prim, 0x25), v3.wrapping_add(h.wrapping_sub(1)) as u8);
    let page_y = u32::from(m.lhu(at(rect, 2)));
    let page_x = u32::from(m.lhu(rect));
    m.sh(at(prim, 0xe), 0x55a);
    let tpage = ((mode & 3) << 5)
        | (((page_y & 0x100) << 16) as i32 >> 20) as u32
        | ((page_x & 0x3ff) >> 6)
        | ((page_y & 0x200) << 2);
    let w = work(m);
    m.sh(at(prim, 0x16), tpage as u16);
    let table = m.lw(at(w, 8));
    let head = m.lw(prim);
    let link = m.lw(at(table, 4));
    m.sw(prim, (head & 0xff00_0000) | (link & 0x00ff_ffff));
    let table = m.lw(at(w, 8));
    let link = m.lw(at(table, 4));
    m.sw(at(table, 4), (link & 0xff00_0000) | (prim & 0x00ff_ffff));
    prim.wrapping_add(0x28)
}

/// The analog flag bit and its sense for axis code `code` (0, 6 and 8
/// carry an inversion flag: for 0 and 6 a set bit means not inverted).
fn axis_flag(code: u32) -> Option<(u32, bool)> {
    match code {
        0 => Some((4, false)),
        6 => Some((8, false)),
        8 => Some((0x10, true)),
        _ => None,
    }
}

/// 800D1B6C: load the controller records into `K`: for each of the five
/// records ([`RECORD_SLOTS`]) the button bytes (`K+0x24+16*slot`, 0x14 =
/// none) from the context's action halfwords and the button masks, the
/// four analog axes (`K+0x84+4*slot`, 0xC = none) and their inversions
/// (`K+0x9C+4*slot`), the analog mode `K+0x15+slot` (0, 2 or 1 by the
/// record's flags); `K+0x14` from context +0x120 bit 2; for the current
/// controller type (800D1B04 of context +0x124) the response `K+0x1B` and
/// the two analog options `K+0xB4/+0xB5`. Returns the original's last
/// value (1 or the block pointer).
pub fn load_settings_d1b6c(m: &mut Machine) -> u32 {
    m.push_frame(0x30);
    let context = context(m);
    let bits = m.lw(at(context, 0x124));
    let port = type_index_d1b04(bits);
    for i in 0..5u32 {
        let entry = RECORD_SLOTS + i;
        let base = i * 0x5c;
        let slot = u32::from(m.lbu(entry));
        let k = options(m);
        let buttons = at(k, (slot << 4).wrapping_add(0x24));
        for b in (0..=0xeu32).rev() {
            m.sb(at(buttons, b), 0x14);
        }
        for halfword in 0xbu32..0x1f {
            let index = (halfword << 1) + base;
            let action = SLOT_ACTIONS + (halfword - 0xb);
            for button in 0..0xfu32 {
                let context = self::context(m);
                let mask = lh(m, BUTTON_MASKS + button * 2);
                let value = u32::from(m.lhu(at(context, index).wrapping_add(0x15c)));
                if mask & value != 0 {
                    let action = m.lbu(action);
                    m.sb(at(buttons, button), action);
                }
            }
        }
        let context = self::context(m);
        let slot = u32::from(m.lbu(entry));
        let k = options(m);
        let record = at(context, base + 0x15c).wrapping_add(0x40);
        let axes = at(k, (slot << 2).wrapping_add(0x84));
        let inverts = at(k, (slot << 2).wrapping_add(0x9c));
        for b in (0..=3u32).rev() {
            m.sb(at(axes, b), 0xc);
        }
        for axis in 0..6u32 {
            let code = axis << 1;
            for n in 0..4u32 {
                let flag = at(inverts, n);
                let want = lh(m, AXES + n * 2);
                let have = u32::from(m.lbu(at(record, axis).wrapping_add(0x12)));
                if want == have {
                    m.sb(at(axes, n), code as u8);
                    if let Some((bit, set)) = axis_flag(code) {
                        let flags = u32::from(m.lhu(at(record, 0x10)));
                        let on = flags & bit != 0;
                        m.sb(flag, u8::from(on == set));
                    }
                }
            }
        }
        let flags = u32::from(m.lhu(at(record, 0x10)));
        let mode = if flags & 2 != 0 {
            0
        } else if flags & 1 != 0 {
            2
        } else {
            1
        };
        let k = options(m);
        let slot = u32::from(m.lbu(entry));
        m.sb(at(k, slot).wrapping_add(0x15), mode);
    }
    let context = self::context(m);
    let mut result = if m.lw(at(context, 0x120)) & 4 != 0 {
        let k = options(m);
        m.sb(at(k, 0x14), 0);
        k
    } else {
        let k = options(m);
        m.sb(at(k, 0x14), 1);
        1
    };
    if !lt(port, 0) {
        let context = self::context(m);
        let record = at(context, port.wrapping_mul(0x5c)).wrapping_add(0x19c);
        let types = m.lw(at(context, 0x124));
        let speed = u32::from(m.lbu(at(record, 3)));
        let response = if types == 4 {
            mulhu(speed >> 1, 0x6bca_1af3) >> 3
        } else {
            speed >> 4
        };
        let k = options(m);
        m.sb(at(k, 0x1b), response.wrapping_sub(1) as u8);
        let context = self::context(m);
        let word = m.lw(at(context, port.wrapping_mul(0x5c)).wrapping_add(0x1b4));
        let k = options(m);
        m.sb(at(k, 0xb4), u8::from(word & 8 != 0));
        let k = options(m);
        let second = word & 0x10 != 0;
        m.sb(at(k, 0xb5), u8::from(second));
        result = if second { 1 } else { k };
    }
    m.pop_frame(0x30);
    result
}

/// 800D1F54: store `K` back into the controller records (the inverse of
/// 800D1B6C): the action halfwords from the button bytes and masks, the
/// axes (0xFF none) and inversion bits, the analog mode bits; context
/// +0x120 bit 2 from `K+0x14`; for the current controller type the two
/// analog options (+0x1B4 bits 3 and 4) and the response bytes (+0x19F
/// .. +0x1AB, every fourth: 0x10/0x20/0x30, or 0x26/0x4C/0x72 for type
/// 4). Returns the original's last value.
pub fn save_settings_d1f54(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = save_settings(m);
    m.pop_frame(0x18);
    result
}

fn save_settings(m: &mut Machine) -> u32 {
    let context = context(m);
    let bits = m.lw(at(context, 0x124));
    let port = type_index_d1b04(bits);
    for i in 0..5u32 {
        let entry = RECORD_SLOTS + i;
        let base = i * 0x5c;
        let slot = u32::from(m.lbu(entry));
        let context = self::context(m);
        let k = options(m);
        let buttons = at(k, (slot << 4).wrapping_add(0x24));
        for n in 0..0x15u32 {
            let offset = base + 0x16 + n * 2;
            m.sh(at(context, offset).wrapping_add(0x15c), 0);
        }
        let context = self::context(m);
        for button in 0..0xfu32 {
            let action = u32::from(m.lbu(at(buttons, button)));
            let halfword = u32::from(m.lbu(ACTION_SLOTS.wrapping_add(action)));
            if halfword != 0x1f {
                let mask = m.lhu(BUTTON_MASKS + button * 2);
                let offset = (halfword << 1).wrapping_add(base);
                m.sh(at(context, offset).wrapping_add(0x15c), mask);
            }
        }
        let context = self::context(m);
        let slot = u32::from(m.lbu(entry));
        let k = options(m);
        let record = at(context, base + 0x15c).wrapping_add(0x40);
        let axes = at(k, (slot << 2).wrapping_add(0x84));
        let inverts = at(k, (slot << 2).wrapping_add(0x9c));
        for b in (0..=5u32).rev() {
            m.sb(at(record, 0x12 + b), 0xff);
        }
        for n in 0..4u32 {
            let flag = at(inverts, n);
            let code = u32::from(m.lbu(at(axes, n)));
            let axis = code >> 1;
            if !lt(axis, 6) {
                continue;
            }
            let value = m.lbu(AXES + n * 2);
            m.sb(at(record, axis).wrapping_add(0x12), value);
            let code = u32::from(m.lbu(at(axes, n)));
            if let Some((bit, set)) = axis_flag(code) {
                let inverted = m.lbu(flag);
                let flags = u32::from(m.lhu(at(record, 0x10)));
                // 0 and 6: flag 0 sets the bit; 8: flag 1 sets it
                let on = if set { inverted == 1 } else { inverted == 0 };
                let flags = if on {
                    flags | bit
                } else {
                    flags & !bit & 0xffff
                };
                m.sh(at(record, 0x10), flags as u16);
            }
        }
        let flags = u32::from(m.lhu(at(record, 0x10))) & 0xfffc;
        let k = options(m);
        m.sh(at(record, 0x10), flags as u16);
        let slot = u32::from(m.lbu(entry));
        match m.lbu(at(k, slot).wrapping_add(0x15)) {
            0 => m.sh(at(record, 0x10), (flags | 2) as u16),
            2 => m.sh(at(record, 0x10), (flags | 1) as u16),
            _ => {}
        }
    }
    let context = self::context(m);
    let k = options(m);
    let word = m.lw(at(context, 0x120)) & !4;
    m.sw(at(context, 0x120), word);
    let result = word | 4;
    if m.lbu(at(k, 0x14)) == 0 {
        m.sw(at(context, 0x120), result);
    }
    if lt(port, 0) {
        return result;
    }
    let context = self::context(m);
    let record = at(context, port.wrapping_mul(0x5c)).wrapping_add(0x15c);
    let k = options(m);
    let first = m.lbu(at(k, 0xb4)) == 1;
    let word = m.lw(at(record, 0x58));
    m.sw(at(record, 0x58), if first { word | 8 } else { word & !8 });
    let k = options(m);
    let second = m.lbu(at(k, 0xb5)) == 1;
    let word = m.lw(at(record, 0x58));
    m.sw(
        at(record, 0x58),
        if second { word | 0x10 } else { word & !0x10 },
    );
    let context = self::context(m);
    let k = options(m);
    let record = at(context, port.wrapping_mul(0x5c)).wrapping_add(0x15c);
    let speed = m.lbu(at(k, 0x1b));
    if speed > 2 {
        return 4;
    }
    let four = m.lw(at(context, 0x124)) == 4;
    let value = match (speed, four) {
        (0, true) => 0x26,
        (0, false) => 0x10,
        (1, true) => 0x4c,
        (1, false) => 0x20,
        (_, true) => 0x72,
        (_, false) => 0x30,
    };
    for n in (0..4u32).rev() {
        m.sb(at(record, 0x43 + n * 4), value);
    }
    record.wrapping_add(0x3c)
}

/// 800D2D74 (options menu phase 4): count `W+0x1A4` while panel 0's first
/// entry is not at rest; once hub cursor 1 is 2 the menu phase becomes 5.
/// Hub flag 5 = the controller type `K+5`. Returns 800DEF94's result.
pub fn wait_d2d74(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    if call(m, PANEL_STATE, [0, 1, 0, 0]) == 0 {
        let w = work(m);
        let count = m.lw(at(w, 0x1a4)).wrapping_add(1);
        m.sw(at(w, 0x1a4), count);
    }
    if call(m, CURSOR, [1, 0, 0, 0]) == 2 {
        let k = options(m);
        m.sb(at(k, 1), 5);
    }
    let k = options(m);
    let kind = u32::from(m.lbu(at(k, 5)));
    let result = call(m, SET_FLAG, [5, kind, 0, 0]);
    m.pop_frame(0x18);
    result
}

/// 800D2DF0 (options menu phase 5): the controller type `K+5` (1..3,
/// right 0x8 and left 0x4 wrap), hub flag 5; confirm (pressed 0x10, none
/// of 0x2F held) sets menu phase 6 (type 3 also load phase 3), cursors 1
/// and 2 = 3 and restarts panel 0; cancel (0x20, none of 0x5F) clears the
/// type, phase 6, cursor 1 = 3, and clears the pressed bits. Returns the
/// context pointer after a cancel, else the last button test's value.
pub fn choose_type_d2df0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let context = context(m);
    if m.lw(at(context, 0x130)) & 8 != 0 {
        let k = options(m);
        let kind = m.lbu(at(k, 5));
        m.sb(at(k, 5), if kind < 3 { kind + 1 } else { 1 });
    }
    let context = self::context(m);
    if m.lw(at(context, 0x130)) & 4 != 0 {
        let k = options(m);
        let kind = m.lbu(at(k, 5));
        m.sb(at(k, 5), if kind < 2 { 3 } else { kind - 1 });
    }
    let k = options(m);
    let kind = u32::from(m.lbu(at(k, 5)));
    call(m, SET_FLAG, [5, kind, 0, 0]);
    let context = self::context(m);
    if m.lw(at(context, 0x12c)) & 0x10 != 0 && m.lw(at(context, 0x128)) & 0x2f == 0 {
        let k = options(m);
        let kind = m.lbu(at(k, 5));
        if kind == 3 {
            let w = work(m);
            m.sh(at(w, 0x1c6), u16::from(kind));
        }
        let k = options(m);
        m.sb(at(k, 1), 6);
        call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
        call(m, SET_CURSOR, [2, 3, u32::MAX, u32::MAX]);
        call(m, PANEL_RESTART, [0, 1, 0, 0]);
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
            let k = options(m);
            m.sb(at(k, 5), 0);
            let k = options(m);
            m.sb(at(k, 1), 6);
            call(m, SET_CURSOR, [1, 3, u32::MAX, u32::MAX]);
            let context = self::context(m);
            m.sw(at(context, 0x12c), 0);
            context
        }
    };
    m.pop_frame(0x18);
    result
}

/// 800D2FA8 (options menu phase 6): once hub cursor 1 is 0 and the loader
/// idle (load phase 0): without a type, back to menu phase 1 (cursor 1 =
/// 1 (0, 0x28), lamp 2 = 1; returns 800DEDF0's result); otherwise the
/// chosen screen `K+0` (7, 8, 9 for types 1..3) and menu phase 0
/// (returns the block pointer). While waiting, the tested value.
pub fn open_type_d2fa8(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = open_type(m);
    m.pop_frame(0x18);
    result
}

fn open_type(m: &mut Machine) -> u32 {
    let cursor = call(m, CURSOR, [1, 0, 0, 0]);
    if cursor != 0 {
        return cursor;
    }
    let w = work(m);
    let phase = lh(m, at(w, 0x1c6));
    if phase != 0 {
        return phase;
    }
    let k = options(m);
    let kind = m.lbu(at(k, 5));
    if kind == 0 {
        m.sb(at(k, 1), 1);
        call(m, SET_CURSOR, [1, 1, 0, 0x28]);
        return call(m, SET_LAMP, [2, 1, 0, 0]);
    }
    if kind == 1 {
        m.sb(k, 7);
    }
    let k = options(m);
    if m.lbu(at(k, 5)) == 2 {
        m.sb(k, 8);
    }
    let k = options(m);
    if m.lbu(at(k, 5)) == 3 {
        m.sb(k, 9);
    }
    let k = options(m);
    m.sb(at(k, 1), 0);
    k
}

/// 800D3088 (KEY CONFIG start): the screen's phase advances (`K+1` + 1,
/// `K+2` = 0), cursors 1 and 2 = 0, lamps 2 = 0xC and 0 = 1, panel 2
/// entries 2 and 3 start, the screen's bank (`[[W+0x1C]+0xC]`) installed;
/// `K+0xE`, `K+0x14` and the four modes cleared, `K+0xE0` = 1, flag bytes
/// 0xB = 4 and 0xA = 0, hub flag 0x3E = 0; then the settings load
/// (800D1B6C, returned).
pub fn key_config_d3088(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let k = options(m);
    let phase = m.lbu(at(k, 1)).wrapping_add(1);
    m.sb(at(k, 1), phase);
    let k = options(m);
    m.sb(at(k, 2), 0);
    call(m, SET_CURSOR, [1, 0, 0, 0]);
    call(m, SET_CURSOR, [2, 0, 0, 0]);
    call(m, SET_LAMP, [2, 0xc, 0, 0]);
    call(m, SET_LAMP, [0, 1, 0, 0]);
    call(m, PANEL_START, [2, 2, 0, 0]);
    call(m, PANEL_START, [2, 3, 0, 0]);
    let w = work(m);
    let block = m.lw(at(w, 0x1c));
    let bank = m.lw(at(block, 0xc));
    call(m, INSTALL_BANK, [bank, 0, 0, 0]);
    let k = options(m);
    m.sh(at(k, 0xe), 0);
    m.sb(at(k, 0x14), 0);
    for n in 0..4u32 {
        let k = options(m);
        m.sb(at(k, n).wrapping_add(0x15), 0);
    }
    let k = options(m);
    let flags = m.lw(FLAGS);
    m.sw(at(k, 0xe0), 1);
    let bytes = m.lw(at(flags, 0x5c));
    m.sb(at(bytes, 0xb), 4);
    let flags = m.lw(FLAGS);
    let bytes = m.lw(at(flags, 0x5c));
    m.sb(at(bytes, 0xa), 0);
    call(m, SET_FLAG, [0x3e, 0, 0, 0]);
    let result = load_settings_d1b6c(m);
    m.pop_frame(0x18);
    result
}

/// 800D31A8 (JP; PAL 800CBC70 is a different routine): `jr ra; nop`, the
/// text setup of controller type 0: nothing, `v0` unchanged.
pub fn type_text_d31a8() {}

/// 800D31B0, 800D31E0, 800D3210: the text setup of controller types 1..3:
/// font 6, grey 0x80. Return 800F33D0's result.
pub fn type_text_d31b0(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    call(m, TEXT_FONT, [6, 0, 0, 0]);
    let result = call(m, TEXT_COLOR, [0x80, 0x80, 0x80, 0]);
    m.pop_frame(0x18);
    result
}

/// 800D31E0: see [`type_text_d31b0`].
pub fn type_text_d31e0(m: &mut Machine) -> u32 {
    type_text_d31b0(m)
}

/// 800D3210: see [`type_text_d31b0`].
pub fn type_text_d3210(m: &mut Machine) -> u32 {
    type_text_d31b0(m)
}

/// 800D3240: the text setup of the controller type `K+0xC` (0..3, the
/// halfword re-read for each test). Returns 800F33D0's result for type 3,
/// else 3 (the last test's constant).
pub fn type_text_d3240(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let k = options(m);
    if lh(m, at(k, 0xc)) == 0 {
        type_text_d31a8();
    }
    let k = options(m);
    if lh(m, at(k, 0xc)) == 1 {
        type_text_d31b0(m);
    }
    let k = options(m);
    if lh(m, at(k, 0xc)) == 2 {
        type_text_d31e0(m);
    }
    let k = options(m);
    let result = if lh(m, at(k, 0xc)) == 3 {
        type_text_d3210(m)
    } else {
        3
    };
    m.pop_frame(0x18);
    result
}

/// 800D32DC (JP; changed from PAL 800E1EA0): text-layer setting 1
/// (800F34F4). Returns its result.
pub fn text_setting_d32dc(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = call(m, TEXT_SETTING, [1, 0, 0, 0]);
    m.pop_frame(0x18);
    result
}

/// 800D32FC: the label of action `action`: 0x13 becomes 0x15 with analog
/// option 1 (`K+0xB4`), 0xE and 0xF become 0x16 and 0x17 with option 2
/// (`K+0xB5`); otherwise unchanged.
pub fn action_label_d32fc(m: &mut Machine, action: u32) -> u32 {
    if action == 0x13 {
        let k = options(m);
        if m.lbu(at(k, 0xb4)) == 1 {
            return 0x15;
        }
    }
    if action == 0xe {
        let k = options(m);
        if m.lbu(at(k, 0xb5)) == 1 {
            return 0x16;
        }
    }
    if action == 0xf {
        let k = options(m);
        if m.lbu(at(k, 0xb5)) == 1 {
            return 0x17;
        }
    }
    action
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_1588 => quad_d1588(m, a0, a1, a2, a3),
        0x800d_1b6c => load_settings_d1b6c(m),
        0x800d_1f54 => save_settings_d1f54(m),
        0x800d_2d74 => wait_d2d74(m),
        0x800d_2df0 => choose_type_d2df0(m),
        0x800d_2fa8 => open_type_d2fa8(m),
        0x800d_3088 => key_config_d3088(m),
        0x800d_31a8 => {
            type_text_d31a8();
            0
        }
        0x800d_31b0 => type_text_d31b0(m),
        0x800d_31e0 => type_text_d31e0(m),
        0x800d_3210 => type_text_d3210(m),
        0x800d_3240 => type_text_d3240(m),
        0x800d_32dc => text_setting_d32dc(m),
        0x800d_32fc => action_label_d32fc(m, a0),
        _ => return None,
    })
}
