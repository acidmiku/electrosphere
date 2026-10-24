//! JP overlay 2, part 3 (800E18B8..800EB73C): the story screen's gauges
//! and display lists, and the shared helpers of the part's other files
//! (`ov2_p3_*.rs`).
//!
//! Translated from the JP instructions (module 2, loaded at
//! [`OVERLAY_BASE`](crate::common::OVERLAY_BASE)); the dispatchers answer
//! only while module 2 is loaded. Calls into the other parts of overlay 2
//! go through [`dispatch::call`](crate::dispatch::call).
//!
//! Overlay blocks used here: `[0x80112DC0]` the drawing block (+4 the
//! primitive cursor, +0x3C the ordering-table selector, +0x60 a frame
//! counter); `[0x80112DC4]` the scene block (+8 the ordering table,
//! +0x160 per-layer display lists, +0x16C per-layer object records, +0x178
//! per-layer enable masks, +0x184 per-gauge centres, +0x194 / +0x19C
//! per-gauge angle and speed).
use crate::common::loaded_module;
use crate::dispatch::call;
use crate::machine::Machine;

/// The overlay module of this part.
pub const MODULE: u32 = 2;

/// `[0x80112DC0]`: the drawing block.
pub const DRAW: u32 = 0x8011_2dc0;
/// `[0x80112DC4]`: the scene block.
pub const SCENE: u32 = 0x8011_2dc4;
/// 0x80112DB4: the scratchpad base word the frame routine sets.
const SCRATCH_BASE: u32 = 0x8011_2db4;
/// 0x8010AEC4: display-list opcode handlers (overlay data).
const LIST_HANDLERS: u32 = 0x8010_aec4;
/// 0x8010B060: per-layer tables of the end-of-list handlers.
const LIST_ENDS: u32 = 0x8010_b060;

/// Resident SDK routines (JP addresses; claimed by `sdk`).
pub const VSYNC: u32 = 0x800a_2098;
pub const SET_DRAW_MODE: u32 = 0x800a_c7f8;
pub const RSIN: u32 = 0x800a_60f8;
pub const RCOS: u32 = 0x800a_61c8;
pub const RAND: u32 = 0x800a_dd48;

/// Other parts of overlay 2.
const GAUGE_MODE: u32 = 0x800d_ee84;
const GAUGE_LEVEL: u32 = 0x800d_eea8;
const FRAME_END: u32 = 0x800d_eecc;
const LIST_START: u32 = 0x800e_08d4;
const LINE: u32 = 0x800e_0f88;
const SPOT: u32 = 0x800e_1040;
const SPOT_MODE: u32 = 0x800e_1118;
const F_E1258: u32 = 0x800e_1258;
const F_E1334: u32 = 0x800e_1334;
const F_E14D0: u32 = 0x800e_14d0;
const TEXT_AT: u32 = 0x800f_33a8;
const TEXT_RGB: u32 = 0x800f_33d0;
const TEXT_F33F8: u32 = 0x800f_33f8;
const TEXT_F34B0: u32 = 0x800f_34b0;
const TEXT_F34C8: u32 = 0x800f_34c8;
const TEXT_F35B8: u32 = 0x800f_35b8;
const TEXT_PRINT: u32 = 0x800f_4ae0;

/// `mfhi` of a signed `mult`.
pub fn mult_hi(a: u32, b: u32) -> u32 {
    ((i64::from(a as i32) * i64::from(b as i32)) >> 32) as u32
}

/// `(mfhi(mult x, magic) >> shift) - (x >> 31)` (the compiler's signed
/// division by a constant).
pub fn magic_div(x: u32, magic: u32, shift: u32) -> u32 {
    ((mult_hi(x, magic) as i32 >> shift).wrapping_sub(x as i32 >> 31)) as u32
}

/// `sll 16; sra 16`.
pub fn sext16(v: u32) -> u32 {
    v as u16 as i16 as i32 as u32
}

/// `sra 16`.
pub fn sra16(v: u32) -> u32 {
    ((v as i32) >> 16) as u32
}

/// `bgez; addiu 0xFFF; sra 12`: signed division by 4096.
pub fn div4096(v: u32) -> u32 {
    let v = v as i32;
    (if v < 0 { v.wrapping_add(0xfff) } else { v } >> 12) as u32
}

/// A call with a fifth argument, stored at `frame + 0x10` (the caller's
/// pushed original frame).
pub fn call5(m: &mut Machine, frame: u32, target: u32, args: [u32; 4], fifth: u32) -> u32 {
    m.sw(frame.wrapping_add(0x10), fifth);
    call(m, target, args)
}

/// `addPrim` into ordering-table entry 0 of the scene block `scene`
/// (`[scene + 8]`, read again for the second store, as the original does).
fn add_prim_first(m: &mut Machine, scene: u32, prim: u32) {
    let ot = m.lw(scene.wrapping_add(8));
    let tag = m.lw(prim);
    let next = m.lw(ot);
    m.sw(prim, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let ot = m.lw(scene.wrapping_add(8));
    let next = m.lw(ot);
    m.sw(ot, (next & 0xff00_0000) | (prim & 0x00ff_ffff));
}

/// 800E18B8: the two gauges of the story screen: by mode
/// (800DEE84(2)) 1 or 3 their filling by level 800DEEA8(2), 2 full.
pub fn gauges_e18b8(m: &mut Machine) {
    let frame = m.push_frame(0x28);
    let level = call(m, GAUGE_LEVEL, [2, 0, 0, 0]);
    let mode = call(m, GAUGE_MODE, [2, 0, 0, 0]);
    match mode {
        1 | 3 => {
            let mut row = 0x0102_0000u32;
            for i in 0..0x11i32 {
                if (level as i32) < i {
                    break;
                }
                let mut v = (level as i32).wrapping_sub(i).wrapping_mul(7);
                if v >= 0x8d {
                    v = 0x8c;
                }
                call5(m, frame, SPOT, [0xa8, sra16(row), sext16(v as u32), 2], 0);
                row = row.wrapping_add(0x9_0000);
            }
            let mut v = magic_div(level.wrapping_mul(75), 0x7878_7879, 3);
            if (v as i32) >= 0x4c {
                v = 0x4b;
            }
            call5(m, frame, SPOT, [0xe8, 0xa0, sext16(v), 2], 0);
            let mut column = 0x00e8_0000u32;
            for i in 0..0x10i32 {
                if (level as i32) < i {
                    break;
                }
                let d = (level as i32).wrapping_sub(i) as u32;
                let mut v = magic_div(d.wrapping_mul(242), 0x6666_6667, 3);
                if (v as i32) >= 0xf3 {
                    v = 0xf2;
                }
                let x = sra16(column);
                call(m, LINE, [x, 0xa0, x, sext16(v.wrapping_add(0xa0))]);
                column = column.wrapping_add(0x5_0000);
            }
            let mut v = magic_div(level.wrapping_mul(144), 0x7878_7879, 3);
            if (v as i32) >= 0x91 {
                v = 0x90;
            }
            call(m, LINE, [0xa8, 0x102, 0xa8, sext16(v.wrapping_add(0x102))]);
        }
        2 => {
            let mut row = 0x0102_0000u32;
            for _ in 0..0x11 {
                call5(m, frame, SPOT, [0xa8, sra16(row), 0x8c, 2], 0);
                row = row.wrapping_add(0x9_0000);
            }
            call5(m, frame, SPOT, [0xe8, 0xa0, 0x4b, 2], 0);
            let mut column = 0x00e8_0000u32;
            for _ in 0..0x10 {
                let x = sra16(column);
                call(m, LINE, [x, 0xa0, x, 0x192]);
                column = column.wrapping_add(0x5_0000);
            }
            call(m, LINE, [0xa8, 0x102, 0xa8, 0x192]);
        }
        _ => {}
    }
    m.pop_frame(0x28);
}

/// 800E1B48: the four rotating gauge needles: per needle its speed
/// (+0x19C) runs up to 0x80 while it is the selected one (+0x7E) and
/// winds down otherwise (passing 5 turns it back by 0x20), the angle
/// (+0x194) advances by it, and a flat quad (POLY_F4) at the needle's
/// centre is linked twice into the ordering table.
pub fn needles_e1b48(m: &mut Machine) {
    m.push_frame(0x40);
    let scene = m.lw(SCENE);
    let ot = m.lw(scene.wrapping_add(8));
    let mut offset = 0u32;
    for i in 0..4u32 {
        let scene = m.lw(SCENE);
        let selected = u32::from(m.lbu(scene.wrapping_add(0x7e)));
        let at = scene.wrapping_add(offset);
        if i == selected {
            let speed = m.lhu(at.wrapping_add(0x19c));
            if speed < 0x80 {
                m.sh(at.wrapping_add(0x19c), speed.wrapping_add(1));
            }
        } else {
            let speed = m.lh(at.wrapping_add(0x19c));
            let raw = m.lhu(at.wrapping_add(0x19c));
            if speed > 0 {
                m.sh(at.wrapping_add(0x19c), raw.wrapping_sub(1));
            }
            let at = m.lw(SCENE).wrapping_add(offset);
            if m.lh(at.wrapping_add(0x19c)) == 5 {
                let angle = m.lhu(at.wrapping_add(0x194));
                m.sh(at.wrapping_add(0x194), angle.wrapping_sub(0x20));
            }
            let at = m.lw(SCENE).wrapping_add(offset);
            if m.lh(at.wrapping_add(0x19c)) < 5 {
                m.sh(at.wrapping_add(0x19c), 0);
            }
        }
        let at = m.lw(SCENE).wrapping_add(offset);
        let angle = m.lhu(at.wrapping_add(0x194));
        let speed = m.lhu(at.wrapping_add(0x19c));
        let draw = m.lw(DRAW);
        m.sh(at.wrapping_add(0x194), angle.wrapping_add(speed));
        let prim = m.lw(draw.wrapping_add(4));
        m.sw(draw.wrapping_add(4), prim.wrapping_add(0x18));
        m.sb(prim.wrapping_add(3), 5);
        m.sb(prim.wrapping_add(7), 0x28);
        m.sb(prim.wrapping_add(4), 0);
        m.sb(prim.wrapping_add(5), 0);
        m.sb(prim.wrapping_add(6), 0);
        let scene = m.lw(SCENE);
        let mut angle = m.lh(scene.wrapping_add(offset).wrapping_add(0x194)) as i32 as u32;
        let centre = scene.wrapping_add(i * 4);
        let cx = m.lh(centre.wrapping_add(0x184)) as i32 as u32;
        let object = m.lw(scene.wrapping_add(0x16c));
        let cy = m.lh(centre.wrapping_add(0x186)) as i32 as u32;
        let back = angle.wrapping_sub(0x80);
        let ox = m.lh(object.wrapping_add(0x10)) as i32 as u32;
        let oy = m.lh(object.wrapping_add(0x12)) as i32 as u32;
        let x = |m: &mut Machine, a: u32, k: u32| {
            let c = call(m, RCOS, [a, 0, 0, 0]);
            div4096(c.wrapping_mul(k)).wrapping_add(cx).wrapping_add(ox)
        };
        let y = |m: &mut Machine, a: u32, k: u32| {
            let s = call(m, RSIN, [a, 0, 0, 0]);
            div4096(s.wrapping_mul(k)).wrapping_add(cy).wrapping_add(oy)
        };
        let v = x(m, back, 22);
        m.sh(prim.wrapping_add(8), v as u16);
        let v = y(m, back, 40);
        m.sh(prim.wrapping_add(0xa), v as u16);
        angle = angle.wrapping_add(0x80);
        let v = x(m, angle, 22);
        m.sh(prim.wrapping_add(0xc), v as u16);
        let v = y(m, angle, 40);
        m.sh(prim.wrapping_add(0xe), v as u16);
        let v = x(m, back, 18);
        m.sh(prim.wrapping_add(0x10), v as u16);
        let v = y(m, back, 29);
        m.sh(prim.wrapping_add(0x12), v as u16);
        let v = x(m, angle, 18);
        m.sh(prim.wrapping_add(0x14), v as u16);
        let v = y(m, angle, 29);
        let draw = m.lw(DRAW);
        m.sh(prim.wrapping_add(0x16), v as u16);
        let selector = m.lw(draw.wrapping_add(0x3c));
        let z = m.lh(selector.wrapping_add(0xe)) as i32 as u32;
        let entry = (z << 2).wrapping_add(ot);
        let tag = m.lw(prim);
        let next = m.lw(entry);
        m.sw(prim, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
        let selector = m.lw(draw.wrapping_add(0x3c));
        offset = offset.wrapping_add(2);
        let z = m.lh(selector.wrapping_add(0xe)) as i32 as u32;
        let entry = (z << 2).wrapping_add(ot);
        let next = m.lw(entry);
        m.sw(entry, (next & 0xff00_0000) | (prim & 0x00ff_ffff));
    }
    m.pop_frame(0x40);
}

/// 800E1F94: a record's summary panel: the text settings, then for a
/// present record (800E42AC(`a0`, `a1`)) its five labelled values
/// (800E44E0, 800E435C, 800E43B4, 800E4518, 800E45AC of `a1`), else the
/// empty-slot text 0x800CC934.
pub fn summary_e1f94(m: &mut Machine, a0: u32, a1: u32) {
    m.push_frame(0x20);
    call(m, TEXT_F35B8, [4, 0, 0, 0]);
    call(m, TEXT_F33F8, [2, 0, 0, 0]);
    call(m, TEXT_F34C8, [1, 0xa, 0x14, 0]);
    let scale = call(m, GAUGE_LEVEL, [0xf, 0, 0, 0]);
    call(m, TEXT_F34B0, [scale, 0, 0, 0]);
    call(m, TEXT_RGB, [0x80, 0x80, 0x80, 0]);
    call(m, TEXT_F35B8, [3, 0, 0, 0]);
    let slot = a1 & 0xff;
    let present = super::ov2_p3_records::present_e42ac(m, a0 & 0xff, slot);
    if present == 0 {
        call(m, TEXT_AT, [0x8c, 0x141, 0, 0]);
        call(m, TEXT_PRINT, [0x800c_c934, 0, 0, 0]);
    } else {
        use super::ov2_p3_records as records;
        call(m, TEXT_AT, [0x60, 0x11c, 0, 0]);
        let text = records::name_e44e0(m, slot);
        call(m, TEXT_PRINT, [text, 0, 0, 0]);
        type Value = fn(&mut Machine, u32) -> u32;
        let rows: [(u32, u32, Value); 4] = [
            (0x135, 0x800c_c914, records::value_e435c),
            (0x145, 0x800c_c91c, records::value_e43b4),
            (0x155, 0x800c_c924, records::value_e4518),
            (0x165, 0x800c_c92c, records::value_e45ac),
        ];
        for (y, label, value) in rows {
            call(m, TEXT_AT, [0x60, y, 0, 0]);
            call(m, TEXT_PRINT, [label, 0, 0, 0]);
            call(m, TEXT_AT, [0x8e, y, 0, 0]);
            let text = value(m, slot);
            call(m, TEXT_PRINT, [text, 0, 0, 0]);
        }
    }
    m.pop_frame(0x20);
}

/// 800E2124: the starfield frame: a moving spot (x from the drawing
/// block's counter +0x60 modulo 156), the counter advanced, twenty random
/// spots, then the three star layers.
pub fn starfield_e2124(m: &mut Machine) {
    let frame = m.push_frame(0x28);
    call(m, SPOT_MODE, [0, 0, 0, 0]);
    let draw = m.lw(DRAW);
    let counter = m.lw(draw.wrapping_add(0x60));
    let scene = m.lw(SCENE);
    let object = m.lw(scene.wrapping_add(0x16c));
    let x = u32::from(m.lhu(object.wrapping_add(0x10)));
    let y = u32::from(m.lhu(object.wrapping_add(0x12)));
    let x = sext16(x.wrapping_add(0xe));
    let modulo = counter.wrapping_sub(div156(counter).wrapping_mul(156));
    let y = sext16(modulo.wrapping_add(y.wrapping_add(0xfc)));
    call5(m, frame, SPOT, [x, y, 0x80, 4], 1);
    call(m, SPOT_MODE, [3, 0, 0, 0]);
    let draw = m.lw(DRAW);
    let counter = m.lw(draw.wrapping_add(0x60));
    m.sw(draw.wrapping_add(0x60), counter.wrapping_add(1));
    for _ in 0..0x14 {
        let r = call(m, RAND, [0, 0, 0, 0]);
        let scene = m.lw(SCENE);
        let object = m.lw(scene.wrapping_add(0x16c));
        let rounded = if (r as i32) < 0 {
            r.wrapping_add(0x7f)
        } else {
            r
        };
        let modulo = r.wrapping_sub(((rounded as i32 >> 7) << 7) as u32);
        let base = u32::from(m.lhu(object.wrapping_add(0x10)));
        let x = sext16(base.wrapping_add(modulo.wrapping_add(0xe)));
        let r = call(m, RAND, [0, 0, 0, 0]);
        let scene = m.lw(SCENE);
        let object = m.lw(scene.wrapping_add(0x16c));
        let base = u32::from(m.lhu(object.wrapping_add(0x12)));
        let modulo = r.wrapping_sub(div156(r).wrapping_mul(156));
        let y = sext16(base.wrapping_add(modulo).wrapping_add(0xfc));
        call5(m, frame, SPOT, [x, y, 1, 2], 1);
    }
    call(m, SPOT_MODE, [2, 0, 0, 0]);
    call(m, F_E1334, [0, 0, 0, 0]);
    call(m, F_E14D0, [0, 0, 0, 0]);
    call(m, F_E1258, [0, 0, 0, 0]);
    m.pop_frame(0x28);
}

/// The `0xD20D20D3` (add, `sra 7`) signed division by 156.
fn div156(x: u32) -> u32 {
    let hi = mult_hi(x, 0xd20d_20d3);
    ((hi.wrapping_add(x) as i32 >> 7).wrapping_sub(x as i32 >> 31)) as u32
}

/// 800E22E4: run display list `index` of layer `layer`: each entry is an
/// opcode word (0xFFFFFFFF ends the list) and its operands; the opcode's
/// handler (table 0x8010AEC4) gets the operands, the ordering table, the
/// object record (layer records +0x16C, 12 bytes per list) and its table
/// slot, and returns the next entry. Layer 0 starts the list first
/// (800E08D4). The list's end handler (0x8010B060) runs last.
pub fn run_list_e22e4(m: &mut Machine, index: u32, layer: u32) {
    m.push_frame(0x30);
    let scene = m.lw(SCENE);
    let lists = m.lw(scene.wrapping_add(layer << 2).wrapping_add(0x160));
    let ot = m.lw(scene.wrapping_add(8));
    let mut entry = m.lw((index << 2).wrapping_add(lists));
    let mut a0 = scene;
    if layer == 0 {
        call(m, LIST_START, [index, 0, 0, 0]);
        a0 = index;
    }
    let record = index.wrapping_mul(12);
    if m.lw(entry) != u32::MAX {
        loop {
            let scene = m.lw(SCENE);
            let slot = (m.lw(entry) << 2).wrapping_add(LIST_HANDLERS);
            let object = m
                .lw(scene.wrapping_add(layer << 2).wrapping_add(0x16c))
                .wrapping_add(record);
            let handler = m.lw(slot);
            entry = call(m, handler, [entry.wrapping_add(4), ot, object, slot]);
            a0 = entry.wrapping_add(4);
            if m.lw(entry) == u32::MAX {
                break;
            }
        }
    }
    let ends = m.lw((layer << 2).wrapping_add(LIST_ENDS));
    let end = m.lw((index << 2).wrapping_add(ends));
    call(m, end, [a0, 0, 0, 0]);
    m.pop_frame(0x30);
}

/// 800E1F04, 800E1F0C, .. 800E1F8C: eighteen `jr ra; nop` routines after
/// 800E1B48, end handlers of display lists (the per-layer tables of
/// 0x8010B060, whose results 800E22E4 ignores). They do nothing.
pub fn empty_e1f04(_m: &mut Machine) {}

/// 800E23FC: run every enabled display list: layers 0..3, lists 0..32 by
/// the layer's enable mask (+0x178).
pub fn run_lists_e23fc(m: &mut Machine) {
    m.push_frame(0x28);
    for layer in 0..3u32 {
        for index in 0..0x20u32 {
            let scene = m.lw(SCENE);
            let mask = m.lw(scene.wrapping_add(layer << 2).wrapping_add(0x178));
            if mask & (1 << index) != 0 {
                run_list_e22e4(m, index, layer);
            }
        }
    }
    m.pop_frame(0x28);
}

/// 800E2494: the scene's frame from the primitive buffer `buffer`: the
/// scratchpad base, VSync(1), a draw-mode primitive first in the ordering
/// table, every display list, a second draw-mode primitive, 800DEECC.
/// Returns the primitive cursor.
pub fn frame_e2494(m: &mut Machine, buffer: u32) -> u32 {
    let frame = m.push_frame(0x30);
    m.sw(SCRATCH_BASE, 0x1f80_0000);
    call(m, VSYNC, [1, 0, 0, 0]);
    let draw = m.lw(DRAW);
    m.sw(draw.wrapping_add(4), buffer.wrapping_add(0xc));
    call5(m, frame, SET_DRAW_MODE, [buffer, 0, 0, 0], 0);
    let scene = m.lw(SCENE);
    add_prim_first(m, scene, buffer);
    run_lists_e23fc(m);
    let draw = m.lw(DRAW);
    let prim = m.lw(draw.wrapping_add(4));
    m.sw(draw.wrapping_add(4), prim.wrapping_add(0xc));
    call5(m, frame, SET_DRAW_MODE, [prim, 0, 0, 0], 0);
    let scene = m.lw(SCENE);
    add_prim_first(m, scene, prim);
    call(m, FRAME_END, [scene, 0, 0, 0]);
    let draw = m.lw(DRAW);
    let cursor = m.lw(draw.wrapping_add(4));
    m.pop_frame(0x30);
    cursor
}

/// The routines of this file, while module 2 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if loaded_module(m) != MODULE {
        return None;
    }
    let [a0, a1, _, _] = args;
    Some(match target {
        0x800e_18b8 => {
            gauges_e18b8(m);
            0
        }
        0x800e_1b48 => {
            needles_e1b48(m);
            0
        }
        0x800e_1f04 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f0c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f14 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f1c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f24 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f2c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f34 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f3c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f44 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f4c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f54 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f5c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f64 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f6c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f74 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f7c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f84 => {
            empty_e1f04(m);
            0
        }
        0x800e_1f8c => {
            empty_e1f04(m);
            0
        }
        0x800e_1f94 => {
            summary_e1f94(m, a0, a1);
            0
        }
        0x800e_2124 => {
            starfield_e2124(m);
            0
        }
        0x800e_22e4 => {
            run_list_e22e4(m, a0, a1);
            0
        }
        0x800e_23fc => {
            run_lists_e23fc(m);
            0
        }
        0x800e_2494 => frame_e2494(m, a0),
        _ => return None,
    })
}
