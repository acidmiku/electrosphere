//! JP overlay 2, part 5: the aircraft/weapon selection screen driven by the
//! block `[0x80112E10]` (800F9B10..800FB2A0), translated from the JP
//! instructions.
//!
//! [`update`] (800FB10C) runs the phase routine of the table 0x8010ECF8 by
//! the block's phase halfword +0:
//!
//! | Phase | Routine | Rust |
//! | ---: | --- | --- |
//! | 0 | `800F9E68` | [`phase_open`] |
//! | 1 | `800F9EC8` | [`phase_choose`]: left/right through the aircraft list |
//! | 2 | `800FA4E4` | [`phase_load`] |
//! | 3 | `800FA860` | [`phase_wait`] |
//! | 4 | `800FA8E0` | [`phase_confirm`] |
//! | 5 | `800FAA48` | [`phase_finish`] |
//!
//! (the table's entries 6 and 7 are [`initialize`] and [`update`]).
//!
//! Callees outside this file (called by JP address): 800DEF94, 800DEE08,
//! 800DEE84, 800DEEA8, 800DECA8, 800DEDF0, 800DEBB8, 800DEB5C (menu
//! items/layers), 800FEBB8, 800FE6E0, 800FE8D8, 800FEAD4, 800FE9C0, 800F7994,
//! 800F7C24, 800F7524, 800F7514, 800F78B0, 800F7BE8, 800F7EA4, 800F8AC4,
//! 800F8C0C, 800F8FA8, 800FC148, 800FC488, 800F35B8, 800F33D0, 800F33A8,
//! 800F4A78, 80100994, the resident 8002DB4C (sound effect), 8002DDA8,
//! 8002C6A8, 80027410/80027538 (arena) and the SDK 800A8440, 800A8194,
//! 800A8680.
use super::ov2_p5::{
    MODULE, alloc, call, ctx, framed, hub, lbu, lh, lhu, lw, sb, sh, slt, sw, sx16,
};
use crate::common::{ENTITIES, GAMEPLAY, loaded_module};
use crate::machine::{Machine, SmallData};

/// `[0x80112E10]`: the selection block (0x13C bytes from the arena): +0 the
/// phase (halfword), +2 its step, +4 a 0x4000-byte buffer, +8 the record
/// `[hub+0xB8] + 0x18 * [hub+0xC0]`, +0xC 0x8010EB0C, +0x10 flags, +0x14 /
/// +0x15 the weapon choices, +0x16/+0x17, +0x118, +0x11A/+0x11B, +0x120.
pub const NET: u32 = 0x8011_2e10;
/// The phase routine table (8 words).
const PHASES: u32 = 0x8010_ecf8;

/// 800DEF94(a0, a1) (another part; its a2/a3 are leftovers, passed as 0).
fn item(m: &mut Machine, a0: u32, a1: u32) {
    call(m, 0x800d_ef94, [a0, a1, 0, 0]);
}
/// 800DEE08(a0, a1, a2, a3).
fn layer(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) {
    call(m, 0x800d_ee08, [a0, a1, a2, a3]);
}
/// 8002DB4C: start sound effect `id`; returns its v0.
fn sound(m: &mut Machine, id: u32) -> u32 {
    call(m, 0x8002_db4c, [id, 0, 0, 0])
}
/// The common tail: with context +0x96C bit 0x10, draw [`model_panel`] and
/// return its v0, else 0 (the `andi` result).
fn panel_tail(m: &mut Machine) -> u32 {
    let context = ctx(m);
    if lw(m, context, 0x96c) & 0x10 != 0 {
        model_panel(m)
    } else {
        0
    }
}

/// 800F9B10 (JP; PAL 800DA440, changed): commit the weapon choices: map
/// `[NET]+0x14` through 800FEBB8(mission, aircraft, 0, choice) into context
/// +0x3A9 (jump table 0x800CD3A4) and `[NET]+0x15` through 800FEBB8(.., 1,
/// ..) into context +0x3AA/+0x3AB (jump table 0x800CD3BC). Returns the
/// original's v0: 0x800D0000 for a second code >= 9, 3 or 5 for the codes
/// 3 and 7, else the context pointer.
pub fn commit_weapons(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = ctx(m);
        let net = m.lw(NET);
        let mission = lh(m, context, 0x38c);
        let aircraft = lh(m, context, 0x38e);
        let choice = lbu(m, net, 0x14);
        let code = call(m, 0x800f_ebb8, [mission, aircraft, 0, choice]);
        // Jump table 0x800CD3A4: cases 0..5 (800F9B68..800F9BB8) store
        // code + 1.
        if code < 6 {
            let context = ctx(m);
            sb(m, context, 0x3a9, code.wrapping_add(1));
        }
        let context = ctx(m);
        let net = m.lw(NET);
        let mission = lh(m, context, 0x38c);
        let aircraft = lh(m, context, 0x38e);
        let choice = lbu(m, net, 0x15);
        let code = call(m, 0x800f_ebb8, [mission, aircraft, 1, choice]);
        // Jump table 0x800CD3BC: 0 800F9C18 (0, 0), 1 800F9C34 (1, 1),
        // 2 800F9C44 (2, 2), 3 800F9C54 (1, 3), 4 800F9C74 (4, 4),
        // 5 800F9C84 (0xA, 0xA), 6 800F9C94 (7, 7), 7 800F9CA4 (1, 5),
        // 8 800F9CC4 (6, 6) into (+0x3AA, +0x3AB).
        let (low, high) = match code {
            0 => (0, 0),
            1 => (1, 1),
            2 => (2, 2),
            3 => (1, 3),
            4 => (4, 4),
            5 => (0xa, 0xa),
            6 => (7, 7),
            7 => (1, 5),
            8 => (6, 6),
            _ => return 0x800d_0000,
        };
        let context = ctx(m);
        sb(m, context, 0x3aa, low);
        let context = ctx(m);
        sb(m, context, 0x3ab, high);
        match code {
            3 => 3,
            7 => 5,
            _ => context,
        }
    })
}

/// 800F9CF0: the "MODEL ID" panel: a black TILE (0, 0x1D, 0x84 x 0x13E) at
/// the hub's packet pointer +0x274 linked into the hub's ordering table,
/// then "MODEL ID" and the three lines of the 12-byte record
/// `0x8010EB3C + 12 * model` (model = `[hub+0x1E0 + 2*[hub+0xC0]]` clamped
/// to 1..0x24). Returns the last 800F4A78's v0.
pub fn model_panel(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let h = hub(m);
        let p = lw(m, h, 0x274);
        sh(m, p, 0xc, 0x84);
        sh(m, p, 0xe, 0x13e);
        sh(m, p, 0xa, 0x1d);
        sb(m, p, 3, 3);
        sb(m, p, 4, 0);
        sb(m, p, 5, 0);
        sb(m, p, 6, 0);
        sb(m, p, 7, 0x60);
        let h = hub(m);
        sh(m, p, 8, 0);
        let ot = lw(m, h, 8);
        let head = lw(m, p, 0);
        let word = lw(m, ot, 0);
        sw(m, p, 0, (head & 0xff00_0000) | (word & 0x00ff_ffff));
        let ot = lw(m, h, 8);
        let word = lw(m, ot, 0);
        sw(m, ot, 0, (word & 0xff00_0000) | (p & 0x00ff_ffff));
        let index = lw(m, h, 0xc0);
        sw(m, h, 0x274, p.wrapping_add(0x10));
        let mut model = lh(m, h.wrapping_add(index << 1), 0x1e0);
        if !slt(0, model) {
            model = 1;
        }
        if !slt(model, 0x25) {
            model = 0x24;
        }
        call(m, 0x800f_35b8, [7, 0, 0, 0]);
        call(m, 0x800f_33d0, [0, 0x80, 0, 0]);
        call(m, 0x800f_33a8, [0xc, 0x40, 0, 0]);
        call(m, 0x800f_4a78, [0x800c_d66c, 0, 0, 0]);
        call(m, 0x800f_33a8, [0xc, 0x60, 0, 0]);
        let names = ((model << 1).wrapping_add(model) << 2).wrapping_add(0x8010_eb3c);
        let line = lw(m, names, 0);
        call(m, 0x800f_4a78, [line, 0, 0, 0]);
        call(m, 0x800f_33a8, [0xc, 0x70, 0, 0]);
        let line = lw(m, names, 4);
        call(m, 0x800f_4a78, [line, 0, 0, 0]);
        call(m, 0x800f_33a8, [0xc, 0x90, 0, 0]);
        let line = lw(m, names, 8);
        call(m, 0x800f_4a78, [line, 0, 0, 0])
    })
}

/// 800F9E68 (JP; PAL 800D71E0, changed), phase 0: go to phase 1 step 0,
/// item 800DEF94(4, 0), and the model panel when context +0x96C has bit
/// 0x10. Returns the panel's v0, else 0.
pub fn phase_open(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let net = m.lw(NET);
        sh(m, net, 2, 0);
        sh(m, net, 0, 1);
        item(m, 4, 0);
        panel_tail(m)
    })
}

/// 800F9EC8, phase 1: the aircraft choice. Sets the items 1..4 from the
/// inputs (context +0x128 held, +0x12C pressed, +0x130 repeat), moves the
/// hub index +0xC0 through the list (+0x1E0 entries, +0x262 count; +0xE4
/// in the other mode) with sound 0x3D, draws the panel (or 800F7994(1)),
/// then on confirm (+0x12C bit 0x10) goes to phase 2 step 1 and on cancel
/// (+0x12C bit 0x20) to phase 2 step 2. Returns 2 after a cancel, else
/// the last tested input value (0, or context +0x128 & 0x5F).
pub fn phase_choose(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let a1 = ctx(m);
        if lw(m, a1, 0x96c) & 0x10 != 0 {
            let h = hub(m);
            if lh(m, h, 0x1c6) != 3 {
                let repeat = lw(m, a1, 0x130);
                let pressed = lw(m, a1, 0x12c);
                sw(m, a1, 0x130, repeat & !0x3c);
                sw(m, a1, 0x12c, pressed & !0x3c);
            }
        }
        item(m, 1, 0);
        let a0 = ctx(m);
        let (last, state) = if lw(m, a0, 0x96c) & 0x10 != 0 {
            if lh(m, a0, 0x38c) == 0x2a {
                item(m, 3, 2);
                (2, 2)
            } else {
                let state = u32::from(lw(m, a0, 0x128) & 4 != 0);
                item(m, 2, state);
                let context = ctx(m);
                (3, u32::from(lw(m, context, 0x128) & 8 != 0))
            }
        } else {
            let h = hub(m);
            let state = if lw(m, h, 0xc0) == 0 {
                2
            } else {
                u32::from(lw(m, a0, 0x128) & 4 != 0)
            };
            item(m, 2, state);
            let h = hub(m);
            let count = lw(m, h, 0xe4);
            let index = lw(m, h, 0xc0);
            if index == count.wrapping_sub(1) {
                (3, 2)
            } else {
                let context = ctx(m);
                (3, u32::from(lw(m, context, 0x128) & 8 != 0))
            }
        };
        item(m, last, state);
        let h = hub(m);
        let list = lw(m, h, 0x16c);
        let state = u32::from(lh(m, list, 0x6a) == 2);
        item(m, 4, state);
        let a0 = ctx(m);
        if lh(m, a0, 0x38c) != 0x2a {
            let repeat = lw(m, a0, 0x130);
            let moved = if repeat & 8 != 0 {
                if lw(m, a0, 0x96c) & 0x10 != 0 {
                    let h = hub(m);
                    let index = lw(m, h, 0xc0);
                    let model = lh(m, h.wrapping_add(index << 1), 0x1e0);
                    call(m, 0x800f_e6e0, [0, model, 0, 0]);
                    let h = hub(m);
                    let count = lh(m, h, 0x262);
                    let index = lw(m, h, 0xc0);
                    if slt(index, count.wrapping_sub(1)) {
                        sw(m, h, 0xc0, index.wrapping_add(1));
                    } else {
                        sw(m, h, 0xc0, 0);
                    }
                    let h = hub(m);
                    sw(m, h, 0xc4, 0);
                    true
                } else {
                    let h = hub(m);
                    let count = lw(m, h, 0xe4);
                    let index = lw(m, h, 0xc0);
                    if slt(index, count.wrapping_sub(1)) {
                        sw(m, h, 0xc0, index.wrapping_add(1));
                        true
                    } else {
                        false
                    }
                }
            } else if repeat & 4 != 0 {
                if lw(m, a0, 0x96c) & 0x10 != 0 {
                    let h = hub(m);
                    let index = lw(m, h, 0xc0);
                    let model = lh(m, h.wrapping_add(index << 1), 0x1e0);
                    call(m, 0x800f_e6e0, [0, model, 0, 0]);
                    let h = hub(m);
                    let index = lw(m, h, 0xc0);
                    let next = if slt(0, index) {
                        index.wrapping_sub(1)
                    } else {
                        lh(m, h, 0x262).wrapping_sub(1)
                    };
                    sw(m, h, 0xc0, next);
                    let h = hub(m);
                    sw(m, h, 0xc4, 0);
                    true
                } else {
                    let h = hub(m);
                    let index = lw(m, h, 0xc0);
                    if slt(0, index) {
                        sw(m, h, 0xc0, index.wrapping_sub(1));
                        true
                    } else {
                        false
                    }
                }
            } else {
                false
            };
            if moved {
                sound(m, 0x3d);
                layer(m, 2, 3, u32::MAX, u32::MAX);
                layer(m, 8, 3, u32::MAX, u32::MAX);
            }
        }
        let context = ctx(m);
        if lw(m, context, 0x96c) & 0x10 != 0 {
            model_panel(m);
        } else {
            call(m, 0x800f_7994, [1, 0, 0, 0]);
        }
        let a0 = ctx(m);
        if lw(m, a0, 0x96c) & 0x10 != 0 {
            if lw(m, a0, 0x12c) & 0x10 != 0 && lw(m, a0, 0x128) & 0x2f == 0 {
                sound(m, 0x5b);
                sound(m, 0x3c);
                let net = m.lw(NET);
                sh(m, net, 2, 1);
                sh(m, net, 0, 2);
            }
        } else {
            let h = hub(m);
            let allowed = lh(m, h, 0x1c6) == 3;
            if lw(m, a0, 0x12c) & 0x10 != 0 && lw(m, a0, 0x128) & 0x2f == 0 {
                let s1 = call(m, 0x800d_ee84, [2, 0, 0, 0]);
                if s1 == 2 && allowed {
                    for a0 in [5, 6, 3, 8] {
                        layer(m, a0, 3, u32::MAX, u32::MAX);
                    }
                    sound(m, 0x5b);
                    sound(m, 0x3c);
                    let net = m.lw(NET);
                    sh(m, net, 2, 1);
                    sh(m, net, 0, s1);
                }
            }
        }
        let v1 = ctx(m);
        if lw(m, v1, 0x12c) & 0x20 == 0 {
            return 0;
        }
        let held = lw(m, v1, 0x128) & 0x5f;
        if held != 0 {
            return held;
        }
        for a0 in [2, 1, 3, 0xb, 5, 6, 7, 8] {
            layer(m, a0, 3, u32::MAX, u32::MAX);
        }
        sound(m, 0x5b);
        sound(m, 0x3b);
        call(m, 0x8010_0994, [0; 4]);
        let net = m.lw(NET);
        sh(m, net, 2, 2);
        sh(m, net, 0, 2);
        2
    })
}

/// 800FA4E4, phase 2: while item 6 runs (800DEE84(6) != 0) feed
/// 800F7C24(0x19 - 800DEEA8(6)); else at step 1 start the load (phase 3,
/// flag bit 1 cleared, aircraft code context +0x38E = 800FE8D8(mission,
/// [hub+0xC0]), weapon codes +0x14/+0x16 and +0x15/+0x17 from 800FEAD4,
/// +0x11A/+0x11B from 800FE9C0), and at step 2 (cancel) leave the screen:
/// the mission table 0x801127AC entry decides the next items, the five
/// sound handles 0x5D..0x61 go to hub +0x130..+0x140, the block is freed
/// (80027538) and gameplay +0x20/+0x24 cleared. Ends with the panel tail;
/// returns its v0 or 0.
pub fn phase_load(m: &mut Machine) -> u32 {
    framed(m, 0x28, |m, sp| {
        item(m, 1, 0);
        let net = m.lw(NET);
        let state = u32::from(lh(m, net, 2) == 1);
        item(m, 4, state);
        if call(m, 0x800d_ee84, [6, 0, 0, 0]) != 0 {
            let v0 = call(m, 0x800d_eea8, [6, 0, 0, 0]);
            call(m, 0x800f_7c24, [0x19u32.wrapping_sub(v0), 0, 0, 0]);
            return panel_tail(m);
        }
        let net = m.lw(NET);
        if lh(m, net, 2) == 1 {
            layer(m, 8, 1, 0, 0x19);
            call(m, 0x800d_eca8, [0, 0xb, 0, 0]);
            call(m, 0x800d_edf0, [0, 0x7a01, 0, 0]);
            sound(m, 0x5a);
            let a0 = m.lw(NET);
            sh(m, a0, 0, 3);
            let flags = lw(m, a0, 0x10);
            sw(m, a0, 0x10, flags & !2);
            let v1 = ctx(m);
            let h = hub(m);
            let mission = lh(m, v1, 0x38c);
            let index = lw(m, h, 0xc0);
            let code = call(m, 0x800f_e8d8, [mission, index, 0, 0]);
            let v1 = ctx(m);
            let mission = lh(m, v1, 0x38c);
            sh(m, v1, 0x38e, code);
            let v0 = call(m, 0x800f_ead4, [mission, sx16(code), 0, 0]);
            let net = m.lw(NET);
            sb(m, net, 0x16, v0);
            sb(m, net, 0x14, v0);
            let codes = |m: &mut Machine| {
                let context = ctx(m);
                (lh(m, context, 0x38c), lh(m, context, 0x38e))
            };
            let (mission, aircraft) = codes(m);
            let v0 = call(m, 0x800f_ead4, [mission, aircraft, 1, 0]);
            let net = m.lw(NET);
            sb(m, net, 0x17, v0);
            sb(m, net, 0x15, v0);
            let (mission, aircraft) = codes(m);
            let v0 = call(m, 0x800f_e9c0, [mission, aircraft, 0, 0]);
            let net = m.lw(NET);
            sb(m, net, 0x11a, v0);
            let (mission, aircraft) = codes(m);
            let v0 = call(m, 0x800f_e9c0, [mission, aircraft, 1, 0]);
            let net = m.lw(NET);
            sb(m, net, 0x11b, v0);
        }
        let net = m.lw(NET);
        if lh(m, net, 2) != 2 {
            return panel_tail(m);
        }
        call(m, 0x8002_c6a8, [0, 0, 0, 0]);
        let a1 = ctx(m);
        let mission = lh(m, a1, 0x38c);
        let entry = ((mission << 1).wrapping_add(mission) << 2).wrapping_add(0x8011_27ac);
        if lh(m, entry, 0) != u32::MAX && lw(m, a1, 0x96c) & 0x10 == 0 {
            call(m, 0x800d_ebb8, [0, 5, 0, 0]);
            call(m, 0x800d_ebb8, [0, 6, 0, 0]);
            sw(m, sp, 0x10, 0x1f0);
            sw(m, sp, 0x14, 0);
            call(m, 0x800d_eb5c, [0, 7, 0, 0]);
            call(m, 0x800d_edf0, [0, 0xe1, 0, 0]);
            sound(m, 0x5a);
            let h = hub(m);
            sh(m, h, 0x84, 1);
        } else {
            for offset in [0x7c, 0x7d, 0x7e] {
                let h = hub(m);
                sb(m, h, offset, 0);
            }
            call(m, 0x800d_ebb8, [0, 1, 0, 0]);
            let v1 = ctx(m);
            if lw(m, v1, 0x96c) & 0x10 != 0 {
                sh(m, v1, 0x38c, 0x3b);
            }
        }
        for (id, offset) in [(0x5d, 0x130), (0x5e, 0x134), (0x5f, 0x138), (0x60, 0x13c)] {
            let handle = sound(m, id);
            let h = hub(m);
            sw(m, h, offset, handle);
        }
        let handle = sound(m, 0x61);
        let h = hub(m);
        sw(m, h, 0x140, handle);
        sh(m, h, 0x86, 0);
        layer(m, 1, 1, 0, 0x28);
        let net = m.lw(NET);
        sw(m, net, 0x10, 0);
        call(m, 0x8002_7538, [net, 0, 0, 0]);
        let gameplay = m.gw(GAMEPLAY);
        sw(m, gameplay, 0x24, 0);
        sw(m, gameplay, 0x20, 0);
        panel_tail(m)
    })
}

/// 800FA860, phase 3: items (1, 1) and (4, 0); when item 8 is done
/// (800DEE84(8) == 2) go to phase 4 step 0. Ends with the panel tail;
/// returns its v0 or 0.
pub fn phase_wait(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        item(m, 1, 1);
        item(m, 4, 0);
        if call(m, 0x800d_ee84, [8, 0, 0, 0]) == 2 {
            let net = m.lw(NET);
            sh(m, net, 0, 4);
            sh(m, net, 2, 0);
        }
        panel_tail(m)
    })
}

/// 800FA8E0, phase 4: the panel (context +0x96C bit 0x10), then confirm
/// (+0x12C bit 0x10, no +0x128 & 0x2F) goes to phase 5 step 1 and cancel
/// (+0x12C bit 0x20, no +0x128 & 0x5F) to phase 5 step 2. Returns 2 after a
/// cancel, else 0 or context +0x128 & 0x5F.
pub fn phase_confirm(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let context = ctx(m);
        if lw(m, context, 0x96c) & 0x10 != 0 {
            model_panel(m);
        }
        let v1 = ctx(m);
        if lw(m, v1, 0x12c) & 0x10 != 0 && lw(m, v1, 0x128) & 0x2f == 0 {
            for a0 in [8, 2, 8, 1, 7] {
                layer(m, a0, 3, u32::MAX, u32::MAX);
            }
            sound(m, 0x5b);
            sound(m, 0x3c);
            let net = m.lw(NET);
            sh(m, net, 0, 5);
            sh(m, net, 2, 1);
        }
        let v1 = ctx(m);
        if lw(m, v1, 0x12c) & 0x20 == 0 {
            return 0;
        }
        let held = lw(m, v1, 0x128) & 0x5f;
        if held != 0 {
            return held;
        }
        layer(m, 8, 3, u32::MAX, u32::MAX);
        sound(m, 0x5b);
        sound(m, 0x3b);
        let net = m.lw(NET);
        sh(m, net, 0, 5);
        sh(m, net, 2, 2);
        2
    })
}

/// 800FAA48, phase 5: item (4, step != 2); while item 8 runs feed
/// 800F7C24(800DEEA8(8)); else at step 1 commit the choices
/// ([`commit_weapons`], hub +0x84 = 3, +0x86 = 0, the block freed, stream
/// stop 800F7524(3, 0), sound 0x5A) and at step 2 go back to phase 0 with
/// flag bit 1 (items 3, 5, 6, 8 or just 3, 800DEDF0(0, 0x7701), sound
/// 0x5A). Ends with the panel tail; returns its v0 or 0.
pub fn phase_finish(m: &mut Machine) -> u32 {
    framed(m, 0x18, |m, _| {
        let net = m.lw(NET);
        let state = u32::from(lh(m, net, 2) != 2);
        item(m, 4, state);
        if call(m, 0x800d_ee84, [8, 0, 0, 0]) != 0 {
            let v0 = call(m, 0x800d_eea8, [8, 0, 0, 0]);
            call(m, 0x800f_7c24, [v0, 0, 0, 0]);
            return panel_tail(m);
        }
        let a0 = m.lw(NET);
        if lh(m, a0, 2) == 1 {
            let h = hub(m);
            sw(m, a0, 0x10, 0);
            sh(m, h, 0x84, 3);
            sh(m, h, 0x86, 0);
            commit_weapons(m);
            let net = m.lw(NET);
            call(m, 0x8002_7538, [net, 0, 0, 0]);
            call(m, 0x800f_7524, [3, 0, 0, 0]);
            sound(m, 0x5a);
        }
        let net = m.lw(NET);
        if lh(m, net, 2) == 2 {
            let context = ctx(m);
            let last = if lw(m, context, 0x96c) & 0x10 == 0 {
                for a0 in [3, 5, 6] {
                    layer(m, a0, 1, 0, 0x19);
                }
                8
            } else {
                3
            };
            layer(m, last, 1, 0, 0x19);
            call(m, 0x800d_edf0, [0, 0x7701, 0, 0]);
            sound(m, 0x5a);
            let v1 = m.lw(NET);
            let flags = lw(m, v1, 0x10);
            sh(m, v1, 0, 0);
            sw(m, v1, 0x10, flags | 2);
        }
        panel_tail(m)
    })
}

/// 800FABE8 (phase table entry 6): set up the selection screen: hub +0x26C =
/// 800F78B0(), allocate the block (0x13C) and its 0x4000-byte buffer, and in
/// the mode context +0x96C bit 0x10 unlock entries from the 800F78B0 bits
/// and build the aircraft list (hub +0x1E0, count +0x262, start +0x260)
/// from the flags context +0x974..+0x9B3 (0x18 excluded; mission 0x2A:
/// just 0x18). Then the panels (800FC148, 800FC488), the stream table
/// 0x8010EB00, the five sounds, the items, the entity `[[gp+0x2E8]+8]`,
/// LoadImage(hub+0xD8, [hub+0xD4]), DrawSync(0), flags 3, 800F8AC4 and
/// 800F7994(0). Returns the incremented hub +0x86 (`lhu + 1`).
pub fn initialize(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let s1 = call(m, 0x800f_78b0, [0; 4]);
        let h = hub(m);
        sw(m, h, 0x26c, s1);
        call(m, 0x8002_c6a8, [0, 0, 0, 0]);
        let block = alloc(m, 0x13c);
        m.sw(NET, block);
        let buffer = alloc(m, 0x4000);
        let v1 = m.lw(NET);
        let a0 = hub(m);
        let a1 = ctx(m);
        sw(m, v1, 4, buffer);
        sh(m, v1, 0, 0);
        sh(m, v1, 2, 0);
        sw(m, v1, 0x120, 0);
        sw(m, a0, 0xc0, 0);
        if lw(m, a1, 0x96c) & 0x10 != 0 {
            if s1 & 4 != 0 {
                sb(m, a1, 0x98d, 1);
            }
            if s1 & 2 != 0 {
                let context = ctx(m);
                sb(m, context, 0x98f, 1);
            }
            if s1 & 1 != 0 {
                for offset in [
                    0x993, 0x994, 0x995, 0x996, 0x997, 0x998, 0x975, 0x976, 0x986, 0x987,
                ] {
                    let context = ctx(m);
                    sb(m, context, offset, 1);
                }
            }
            let a1 = hub(m);
            let a2 = ctx(m);
            sh(m, a1, 0x262, 0);
            sh(m, a1, 0x260, 0);
            if lh(m, a2, 0x38c) == 0x2a {
                sh(m, a1, 0x1e0, 0x18);
                sh(m, a1, 0x262, 1);
            } else {
                for a0 in 0..0x40u32 {
                    if lbu(m, a2.wrapping_add(a0), 0x974) == 1 && a0 != 0x18 {
                        let count = lh(m, a1, 0x262);
                        sh(m, a1.wrapping_add(count << 1), 0x1e0, a0);
                        if lh(m, a2, 0x38e) == a0 {
                            let count = lhu(m, a1, 0x262);
                            sh(m, a1, 0x260, count);
                        }
                        let count = lhu(m, a1, 0x262).wrapping_add(1);
                        sh(m, a1, 0x262, count);
                    }
                }
            }
            let a1 = hub(m);
            let v1 = ctx(m);
            let start = lh(m, a1, 0x260);
            sw(m, a1, 0xc0, start);
            let model = lh(m, a1.wrapping_add(start << 1), 0x1e0);
            if model != lh(m, v1, 0x38e) {
                sw(m, a1, 0xc0, 0);
                sh(m, a1, 0x260, 0);
            }
            let h = hub(m);
            sw(m, h, 0xc4, 0);
        }
        call(m, 0x800f_c148, [0; 4]);
        call(m, 0x800f_c488, [0; 4]);
        let a1 = m.lw(NET);
        let v1 = hub(m);
        sw(m, a1, 0xc, 0x8010_eb0c);
        let index = lw(m, v1, 0xc0);
        let records = lw(m, v1, 0xb8);
        sb(m, a1, 0x118, 0);
        let a0 = ctx(m);
        let record = records.wrapping_add(((index << 1).wrapping_add(index)) << 3);
        sw(m, a1, 8, record);
        if lw(m, a0, 0x96c) & 0x10 == 0 {
            call(m, 0x800f_7514, [0x8010_eb00, 0, 0, 0]);
        }
        for offset in [0x130, 0x134, 0x138, 0x13c, 0x140] {
            let h = hub(m);
            let handle = lw(m, h, offset);
            call(m, 0x8002_dda8, [handle, 0, 0, 0]);
        }
        let context = ctx(m);
        if lw(m, context, 0x96c) & 0x10 == 0 {
            call(m, 0x800f_7524, [1, 0, 0, 0]);
        }
        sound(m, 0x5a);
        let context = ctx(m);
        let items: &[u32] = if lw(m, context, 0x96c) & 0x10 != 0 {
            &[3, 0xb, 7]
        } else {
            &[2, 8, 1, 3, 0xb, 5, 6, 7]
        };
        for &a0 in items {
            layer(m, a0, 1, 0, 0x19);
        }
        call(m, 0x800d_edf0, [0, 0x7701, 0, 0]);
        // the entity [[gp+0x2E8]+8]
        let a1 = m.gw(ENTITIES);
        let a0 = lw(m, a1, 8);
        let flags = lw(m, a0, 0) | 0x1_c000;
        sw(m, a0, 0, flags);
        let v1 = lw(m, a1, 8);
        sb(m, v1, 4, 2);
        let v1 = m.gw(ENTITIES);
        let h = hub(m);
        let entity = lw(m, v1, 8);
        sw(m, entity, 8, 0);
        let entity = lw(m, v1, 8);
        sw(m, entity, 0xc, 0);
        let entity = lw(m, v1, 8);
        sw(m, entity, 0x10, 0);
        let entity = lw(m, v1, 8);
        sh(m, entity, 0x18, 0);
        let entity = lw(m, v1, 8);
        sh(m, entity, 0x1a, 0x1000);
        let entity = lw(m, v1, 8);
        sh(m, entity, 0x1c, 0x1000);
        let image = lw(m, h, 0xd4);
        call(m, 0x800a_8440, [h.wrapping_add(0xd8), image, 0, 0]);
        call(m, 0x800a_8194, [0, 0, 0, 0]);
        let net = m.lw(NET);
        sw(m, net, 0x10, 3);
        call(m, 0x800f_8ac4, [0; 4]);
        call(m, 0x800f_7994, [0, 0, 0, 0]);
        let context = ctx(m);
        if lw(m, context, 0x96c) & 0x10 != 0 {
            layer(m, 2, 3, u32::MAX, u32::MAX);
            layer(m, 8, 3, u32::MAX, u32::MAX);
        }
        let h = hub(m);
        let count = lhu(m, h, 0x86).wrapping_add(1);
        sh(m, h, 0x86, count);
        count
    })
}

/// 800FB10C (phase table entry 7): the selection screen frame: keep the
/// stream playing (800F7524(1, 0) unless context +0x96C bit 0x10 or phase
/// 5), unless the block's word 0 is 0x00020002 clear the gameplay ordering
/// table (ClearOTagR([gameplay+0x10], 0x40)), link it with the hub's table
/// and run 800F7BE8; in the phases 3..5 run 800F8C0C and 800F8FA8; draw
/// 800F7EA4; run the phase routine `[0x8010ECF8 + 4 * phase]`; finally
/// 800F7524(0, 0). Returns its v0.
pub fn update(m: &mut Machine) -> u32 {
    framed(m, 0x20, |m, _| {
        let context = ctx(m);
        if lw(m, context, 0x96c) & 0x10 == 0 {
            let net = m.lw(NET);
            if lh(m, net, 0) != 5 {
                call(m, 0x800f_7524, [1, 0, 0, 0]);
            }
        }
        let net = m.lw(NET);
        if lw(m, net, 0) != 0x0002_0002 {
            let gameplay = m.gw(GAMEPLAY);
            let ot = lw(m, gameplay, 0x10);
            call(m, 0x800a_8680, [ot, 0x40, 0, 0]);
            let a3 = m.gw(GAMEPLAY);
            let a2 = hub(m);
            let a0 = lw(m, a3, 0x10);
            let v0 = lw(m, a2, 8);
            let v1 = lw(m, a0, 0);
            let v0 = lw(m, v0, 0xc);
            sw(m, a0, 0, (v1 & 0xff00_0000) | (v0 & 0x00ff_ffff));
            let v0 = lw(m, a3, 0x10);
            let a0 = lw(m, a2, 8);
            let v1 = lw(m, a0, 0xc);
            let link = (v1 & 0xff00_0000) | (v0.wrapping_add(0xfc) & 0x00ff_ffff);
            sw(m, a0, 0xc, link);
            call(m, 0x800f_7be8, [0; 4]);
        }
        let net = m.lw(NET);
        if lhu(m, net, 0).wrapping_sub(3) < 3 {
            call(m, 0x800f_8c0c, [0; 4]);
            let h = hub(m);
            let packets = lw(m, h, 0x274);
            let ot = lw(m, h, 8);
            let v0 = call(m, 0x800f_8fa8, [packets, ot, 0, 0]);
            let h = hub(m);
            sw(m, h, 0x274, v0);
        }
        let h = hub(m);
        let packets = lw(m, h, 0x274);
        let v0 = call(m, 0x800f_7ea4, [packets, 0, 0, 0]);
        let h = hub(m);
        sw(m, h, 0x274, v0);
        let net = m.lw(NET);
        let phase = lh(m, net, 0);
        // Phase table 0x8010ECF8: 0 800F9E68, 1 800F9EC8, 2 800FA4E4,
        // 3 800FA860, 4 800FA8E0, 5 800FAA48, 6 800FABE8, 7 800FB10C.
        let routine = m.lw((phase << 2).wrapping_add(PHASES));
        call(m, routine, [0; 4]);
        call(m, 0x800f_7524, [0, 0, 0, 0])
    })
}

/// The routines of this file by JP address, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, _args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    Some(match target {
        0x800f_9b10 => commit_weapons(m),
        0x800f_9cf0 => model_panel(m),
        0x800f_9e68 => phase_open(m),
        0x800f_9ec8 => phase_choose(m),
        0x800f_a4e4 => phase_load(m),
        0x800f_a860 => phase_wait(m),
        0x800f_a8e0 => phase_confirm(m),
        0x800f_aa48 => phase_finish(m),
        0x800f_abe8 => initialize(m),
        0x800f_b10c => update(m),
        _ => return None,
    })
}
