//! JP overlay 1, part 2: the visual block, its draw scripts and the menu
//! screens drawn with them (`800DA6FC`..`800DCAA4`).
//!
//! The visual block `[0x800DD9DC]` (0x58 bytes, [`init_visuals`]): +0x4 the
//! packet cursor, +0x8/+0xC the two buffers' packet areas (0x8000 bytes),
//! +0x10 the current ordering table and +0x14/+0x18 the two buffers' ones
//! (16 entries), +0x1C the running scripts (bit n: script n), +0x20 32
//! script records (12 bytes: +4/+6 offset, +8, +0xA), +0x24 the script
//! table (32 pointers), +0x2C the animation (set by command 5; the default
//! at +0x30..+0x3E), +0x40/+0x44/+0x48 16 timers (value, limit, state:
//! 1 counting up, 2 done, 3 counting down), +0x4C 64 condition bytes,
//! +0x50/+0x52 the account box position, +0x54 flags (bit 0 the box, 1 the
//! account list, 2 the cursor lines, 3 the list's left column).
//!
//! A script is a list of commands (a word: 0..6, -1 ends it) run by
//! [`run_script`] through the handlers at 0x800DD934 ([`draw_tile`],
//! [`draw_sprite`], [`draw_mode`], [`draw_quad`], [`draw_area`],
//! [`set_animation`], [`draw_line`]); every drawing command first asks the
//! animation's gate (0x800DD924: 800DA9D4 always, 800DA9DC the
//! interpolating clip of the scratchpad rectangle `[0x800DD9D8]`, 800DAC98
//! done, 800DACD0 the growing clip) and links its packet into the
//! animation's bin. Then the script's callback (0x800DD950) runs.
//!
//! **Map boundary:** the matcher's `800DA9D8` (704 bytes) was the delay
//! slot of the 8-byte gate at 800DA9D4 (`jr ra; li v0,1`) followed by the
//! gate at 800DA9DC; `tools/jp/fix_map.py` splits it into those two map
//! functions, which the gate table holds and [`gate_always`] and
//! [`gate_clip`] claim. It also makes the callback table's `jr ra; nop`
//! stubs (800DC424, 800DC42C, 800DC5E4, 800DC694..800DC6BC) map functions,
//! claimed by [`dispatch`] ([`run_script`] calls them through it).
use super::ov1_p2::{
    VISUAL, call, context, div_trap, lbu, lh, lhu, loaded, mul, off, sext16, slt, sra,
};
use super::ov1_p2_menu::{line2_double, poly_g4};
use super::ov1_p2_save::{account_bits, account_name};
use super::ov1_p2_text::{
    get_position, print, set_blend, set_color, set_font, set_line, set_position, set_skew,
};
use crate::{arena, machine::Machine, sdk};

/// `[0x800DD9D8]`: the clip rectangle (the scratchpad, 0x1F800000).
const RECT: u32 = 0x800d_d9d8;
/// `[0x800DD9D0]`: the menu state block of the first half (+8 its packet
/// cursor, +0x5C the list page, +0x5D the excluded account, +0x64 lists).
const MENU_STATE: u32 = 0x800d_d9d0;
/// 0x800DD924: the four animation gates.
const GATES: u32 = 0x800d_d924;
/// 0x800DD934: the seven command handlers.
const HANDLERS: u32 = 0x800d_d934;
/// 0x800DD950: the 32 script callbacks.
const CALLBACKS: u32 = 0x800d_d950;
/// 800D3D90 (first half): the load-screen status of a card slot.
const SLOT_STATE: u32 = 0x800d_3d90;
/// 0x800CC5B0 "empty", 0x800CC5B8 "New Account", 0x800CC5C4 "Download",
/// 0x800CC5D0 "re-Open".
const EMPTY: u32 = 0x800c_c5b0;
const NEW_ACCOUNT: u32 = 0x800c_c5b8;
const DOWNLOAD: u32 = 0x800c_c5c4;
const REOPEN: u32 = 0x800c_c5d0;

fn visual(m: &mut Machine) -> u32 {
    m.lw(VISUAL)
}
fn animation(m: &mut Machine) -> u32 {
    let v = visual(m);
    m.lw(off(v, 0x2c))
}

// ---- visual helpers -------------------------------------------------------------------

/// 800DA6FC (JP; PAL 800D52DC, changed): the current ordering table.
pub fn current_bin(m: &mut Machine) -> u32 {
    let v = visual(m);
    m.lw(off(v, 0x10))
}

/// 800DA714: script record `index` (< 32): +0xA `state`, +4/+6 the offset,
/// +8 the fifth argument (`[sp+0x10]`).
pub fn set_record(m: &mut Machine, index: u32, state: u32, x: u32, y: u32) {
    let sp = m.sp;
    let extra = m.lw(off(sp, 0x10));
    if index >= 0x20 {
        return;
    }
    let at = (index << 1).wrapping_add(index) << 2;
    let v = visual(m);
    for (offset, value) in [(0xa, state), (4, x), (6, y), (8, extra)] {
        let records = m.lw(off(v, 0x20));
        m.sh(off(off(at, records), offset), value as u16);
    }
}

/// 800DA77C: reset script record `index` (state 1, no offset).
pub fn reset_record(m: &mut Machine, index: u32) {
    let frame = m.push_frame(0x20);
    m.sw(off(frame, 0x10), 0);
    set_record(m, index, 1, 0, 0);
    m.pop_frame(0x20);
}

/// 800DA7A8 (unreferenced): script record `index` to state 2, +8 negated.
pub fn mirror_record(m: &mut Machine, index: u32) {
    let frame = m.push_frame(0x20);
    let v = visual(m);
    let records = m.lw(off(v, 0x20));
    let at = off((index << 1).wrapping_add(index) << 2, records);
    let extra = lhu(m, off(at, 8));
    let x = lh(m, off(at, 4));
    let y = lh(m, off(at, 6));
    m.sw(off(frame, 0x10), sext16(0u32.wrapping_sub(extra)));
    set_record(m, index, 2, x, y);
    m.pop_frame(0x20);
}

/// 800DA800 (JP; PAL 800D18F8 of overlay 2, changed; unreferenced): the
/// state of script record `index`.
pub fn record_state(m: &mut Machine, index: u32) -> u32 {
    let v = visual(m);
    let records = m.lw(off(v, 0x20));
    let at = ((index << 1).wrapping_add(index) << 2).wrapping_add(records);
    lh(m, off(at, 0xa))
}

/// 800DA828 (JP; PAL 800D54C0, changed): the running scripts.
pub fn set_scripts(m: &mut Machine, bits: u32) {
    let v = visual(m);
    m.sw(off(v, 0x1c), bits);
}

/// 800DA838: timer `index` (< 16): state `state`, limit `limit` (when not
/// negative) and value `value` (when not negative).
pub fn set_timer(m: &mut Machine, index: u32, state: u32, value: u32, limit: u32) {
    if index >= 0x10 {
        return;
    }
    let v = visual(m);
    let at = index << 1;
    let states = m.lw(off(v, 0x48));
    m.sh(off(at, states), state as u16);
    if (limit as i32) >= 0 {
        let limits = m.lw(off(v, 0x44));
        m.sh(off(at, limits), limit as u16);
    }
    if (value as i32) >= 0 {
        let v = visual(m);
        let values = m.lw(off(v, 0x40));
        m.sh(off(at, values), value as u16);
    }
}

/// 800DA898 (JP; PAL 800D570C, changed): the state of timer `index`.
pub fn timer_state(m: &mut Machine, index: u32) -> u32 {
    let v = visual(m);
    let states = m.lw(off(v, 0x48));
    lh(m, off(index << 1, states))
}

/// 800DA8BC (JP; PAL 800E9854 of overlay 2, changed): the value of timer
/// `index`.
pub fn timer_value(m: &mut Machine, index: u32) -> u32 {
    let v = visual(m);
    let values = m.lw(off(v, 0x40));
    lh(m, off(index << 1, values))
}

/// 800DA8E0: advance the 16 timers: state 1 counts up to the limit (then
/// state 2), state 3 counts down to 0 (then state 0).
pub fn tick_timers(m: &mut Machine) {
    let v = visual(m);
    for i in 0..0x10u32 {
        let at = i << 1;
        let states = m.lw(off(v, 0x48));
        let state = off(at, states);
        if lh(m, state) == 1 {
            let values = m.lw(off(v, 0x40));
            let limits = m.lw(off(v, 0x44));
            let value = off(at, values);
            let limit = off(at, limits);
            let below = slt(lh(m, value), lh(m, limit));
            let next = lhu(m, value).wrapping_add(1);
            if below {
                m.sh(value, next as u16);
            } else {
                m.sh(state, 2);
            }
        }
        let states = m.lw(off(v, 0x48));
        let state = off(at, states);
        if lh(m, state) == 3 {
            let values = m.lw(off(v, 0x40));
            let value = off(at, values);
            let current = lh(m, value);
            let next = lhu(m, value).wrapping_sub(1);
            if (current as i32) > 0 {
                m.sh(value, next as u16);
            } else {
                m.sh(state, 0);
            }
        }
    }
}

/// 800DA9A8 (JP; PAL 800F1770 of overlay 2, changed): condition byte
/// `index` (< 64).
pub fn set_condition(m: &mut Machine, index: u32, value: u32) {
    if index >= 0x40 {
        return;
    }
    let v = visual(m);
    let conditions = m.lw(off(v, 0x4c));
    m.sb(off(conditions, index), value as u8);
}

// ---- animation gates --------------------------------------------------------------------

/// 800DA9D4 (gate 0, `jr ra; li v0,1`): always draw (1).
pub fn gate_always() -> u32 {
    1
}

/// The animation's timer: (value, state); the start (+4), middle (+6)
/// and end (+8) times.
fn animation_times(m: &mut Machine) -> (u32, u32, u32, u32, u32) {
    let v = visual(m);
    let anim = m.lw(off(v, 0x2c));
    let values = m.lw(off(v, 0x40));
    let index = lh(m, off(anim, 0xc));
    let middle = lh(m, off(anim, 6));
    let start = lhu(m, off(anim, 4));
    let value = lh(m, off(index << 1, values));
    let packed = start | (middle << 16);
    let anim = m.lw(off(v, 0x2c));
    let end = lhu(m, off(anim, 8));
    let anim = m.lw(off(v, 0x2c));
    let index = lh(m, off(anim, 0xc));
    let states = m.lw(off(v, 0x48));
    let state = lh(m, off(index << 1, states));
    (
        value,
        state,
        sext16(packed),
        sext16(packed >> 16),
        sext16(end),
    )
}

/// 800DA9DC (gate 1): the clip rectangle grows from the animation's origin
/// (+0, +2) to its full width between the start and middle times (the
/// height 2, the y one step from +2 towards the rectangle's), then to its
/// full height until the end time. 0 before the start or while the timer
/// is idle, else 1.
pub fn gate_clip(m: &mut Machine) -> u32 {
    let (value, state, start, middle, end) = animation_times(m);
    let anim = animation(m);
    let rect = m.lw(RECT);
    if state == 0 {
        return 0;
    }
    if state == 2 {
        return 1;
    }
    if slt(value, start) {
        return 0;
    }
    if slt(value, middle) {
        let dx = lh(m, rect).wrapping_sub(lh(m, anim));
        let product = mul(dx, value.wrapping_sub(start));
        let q = div_trap(
            m,
            product,
            middle.wrapping_sub(start),
            0x800d_aac0,
            0x800d_aad8,
        )
        .0;
        let x = lhu(m, anim).wrapping_add(q);
        m.sh(rect, x as u16);
        let anim = animation(m);
        let dy = lh(m, off(rect, 2)).wrapping_sub(lh(m, off(anim, 2)));
        let size = if (dy as i32) < 0 {
            0u32.wrapping_sub(dy)
        } else {
            dy
        };
        let q = div_trap(m, dy, size, 0x800d_ab1c, 0x800d_ab34).0;
        let y = lhu(m, off(anim, 2)).wrapping_add(q);
        m.sh(off(rect, 2), y as u16);
        let w = lh(m, off(rect, 4));
        let product = mul(w, value.wrapping_sub(start));
        let q = div_trap(
            m,
            product,
            middle.wrapping_sub(start),
            0x800d_ab80,
            0x800d_ab98,
        )
        .0;
        m.sh(off(rect, 6), 2);
        m.sh(off(rect, 4), q as u16);
        return 1;
    }
    if !slt(value, end) {
        return 1;
    }
    let dy = lh(m, off(rect, 2)).wrapping_sub(lh(m, off(anim, 2)));
    let product = mul(dy, value.wrapping_sub(middle));
    let q = div_trap(
        m,
        product,
        end.wrapping_sub(middle),
        0x800d_abfc,
        0x800d_ac14,
    )
    .0;
    let y = lhu(m, off(anim, 2)).wrapping_add(q);
    m.sh(off(rect, 2), y as u16);
    let h = lh(m, off(rect, 6));
    let product = mul(h, value.wrapping_sub(middle));
    let q = div_trap(
        m,
        product,
        end.wrapping_sub(middle),
        0x800d_ac68,
        0x800d_ac80,
    )
    .0;
    m.sh(off(rect, 6), q as u16);
    1
}

/// 800DAC98 (gate 2): draw once the animation's timer is done.
pub fn gate_done(m: &mut Machine) -> u32 {
    let v = visual(m);
    let anim = m.lw(off(v, 0x2c));
    let index = lh(m, off(anim, 0xc));
    let states = m.lw(off(v, 0x48));
    u32::from(lh(m, off(index << 1, states)) ^ 2 == 0)
}

/// 800DACD0 (gate 3): the clip rectangle grows from the animation's x to
/// its full width between the start and middle times; 0 before the start,
/// at the end time and while the timer is idle or done.
pub fn gate_grow(m: &mut Machine) -> u32 {
    let (value, state, start, middle, _) = animation_times(m);
    let anim = animation(m);
    let rect = m.lw(RECT);
    if state == 0 || state == 2 {
        return if state == 0 { 0 } else { 1 };
    }
    if slt(value, start) {
        return 0;
    }
    if !slt(value, middle) {
        return 1;
    }
    let dx = lh(m, rect).wrapping_sub(lh(m, anim));
    let product = mul(dx, value.wrapping_sub(start));
    let q = div_trap(
        m,
        product,
        middle.wrapping_sub(start),
        0x800d_adb0,
        0x800d_adc8,
    )
    .0;
    let x = lhu(m, anim).wrapping_add(q);
    m.sh(rect, x as u16);
    let w = lh(m, off(rect, 4));
    let product = mul(w, value.wrapping_sub(start));
    let q = div_trap(
        m,
        product,
        middle.wrapping_sub(start),
        0x800d_ae14,
        0x800d_ae2c,
    )
    .0;
    m.sh(off(rect, 4), q as u16);
    1
}

// ---- commands --------------------------------------------------------------------------

/// The command's condition: condition byte `[args+index]` (when the index
/// halfword is nonzero) must equal the halfword after it.
fn condition_holds(m: &mut Machine, args: u32, index_at: u32) -> bool {
    let index = lh(m, off(args, index_at));
    if index == 0 {
        return true;
    }
    let v = visual(m);
    let conditions = m.lw(off(v, 0x4c));
    let flag = lbu(m, off(conditions, index));
    lh(m, off(args, index_at + 2)) == flag
}

/// Reserve `bytes` of packet space: the packet.
fn reserve(m: &mut Machine, bytes: u32) -> u32 {
    let v = visual(m);
    let packet = m.lw(off(v, 4));
    m.sw(off(v, 4), packet.wrapping_add(bytes));
    packet
}

/// The clip rectangle from the command's four halfwords at `args`.
fn set_rect(m: &mut Machine, args: u32) {
    let rect = m.lw(RECT);
    let y = lh(m, off(args, 2));
    let x = lhu(m, args);
    m.sw(rect, x | (y << 16));
    let h = lh(m, off(args, 6));
    let w = lhu(m, off(args, 4));
    m.sw(off(rect, 4), w | (h << 16));
}

/// Ask the animation's gate (0x800DD924 + 4 * anim+0xA).
fn gate(m: &mut Machine) -> u32 {
    let anim = animation(m);
    let index = lh(m, off(anim, 0xa));
    let target = m.lw(off(GATES, index << 2));
    call(m, target, [0; 4])
}

/// Link `packet` (`words` long) as the head of the animation's bin of `ot`
/// (the entry takes the packet's address alone).
fn link_head(m: &mut Machine, ot: u32, packet: u32, words: u32) {
    let v = visual(m);
    let anim = m.lw(off(v, 0x2c));
    let bin = lh(m, off(anim, 0xe));
    let next = m.lw(off(ot, bin << 2));
    m.sw(packet, next | (words << 24));
    let anim = m.lw(off(v, 0x2c));
    let bin = lh(m, off(anim, 0xe));
    m.sw(off(ot, bin << 2), (packet << 8) >> 8);
}

/// addPrim of `packet` into entry `bin` of `ot`.
fn add_prim(m: &mut Machine, ot: u32, bin: u32, packet: u32) {
    let tag = m.lw(packet);
    let next = m.lw(off(ot, bin << 2));
    m.sw(packet, (tag & 0xff00_0000) | (next & 0x00ff_ffff));
    let entry = m.lw(off(ot, bin << 2));
    m.sw(
        off(ot, bin << 2),
        (entry & 0xff00_0000) | (packet & 0x00ff_ffff),
    );
}

/// Colour bytes +4..+6 and the code byte (code, or code | 2 when the flag
/// byte at `flag` is set) of a flat packet, in the original's order.
fn colour_and_code(m: &mut Machine, args: u32, colour: u32, packet: u32, size: u8, code: u8) {
    let r = lbu(m, off(args, colour));
    m.sb(off(packet, 4), r as u8);
    let g = lbu(m, off(args, colour + 1));
    m.sb(off(packet, 5), g as u8);
    let b = lbu(m, off(args, colour + 2));
    m.sb(off(packet, 3), size);
    m.sb(off(packet, 7), code);
    m.sb(off(packet, 6), b as u8);
    let blend = lbu(m, off(args, colour + 3));
    m.sb(off(packet, 7), if blend != 0 { code | 2 } else { code });
}

/// 800DAE44 (command 0): a TILE {x, y, w, h} (clipped by the gate) offset
/// by the record, colour +8..+0xA, blended by +0xB; condition +0xC/+0xE.
/// Returns the next command (+0x14).
pub fn draw_tile(m: &mut Machine, args: u32, ot: u32, record: u32) -> u32 {
    m.push_frame(0x30);
    let next = off(args, 0x14);
    if condition_holds(m, args, 0xc) {
        let packet = reserve(m, 0x10);
        set_rect(m, args);
        if gate(m) != 0 {
            let rect = m.lw(RECT);
            let ox = lh(m, off(record, 4));
            let x = lh(m, rect).wrapping_add(ox);
            let oy = lh(m, off(record, 6));
            let y = lh(m, off(rect, 2)).wrapping_add(oy);
            m.sw(off(packet, 8), (x & 0xffff) | (y << 16));
            let h = lh(m, off(rect, 6));
            let w = lhu(m, off(rect, 4));
            m.sw(off(packet, 0xc), w | (h << 16));
            colour_and_code(m, args, 8, packet, 3, 0x60);
            link_head(m, ot, packet, 3);
        }
    }
    m.pop_frame(0x30);
    next
}

/// 800DB024 (command 1): a SPRT at {+0, +2} offset by the record, size
/// {4 * +0x10, +0x12}, texture {+0xC, +0xE}, CLUT {+8, +0xA}, colour
/// +4..+6 blended by +7; drawn when the gate allows; condition +0x14/+0x16.
/// Returns the next command (+0x1C).
pub fn draw_sprite(m: &mut Machine, args: u32, ot: u32, record: u32) -> u32 {
    m.push_frame(0x28);
    let next = off(args, 0x1c);
    if condition_holds(m, args, 0x14) {
        let packet = reserve(m, 0x14);
        if gate(m) != 0 {
            let ox = lh(m, off(record, 4));
            let oy = lh(m, off(record, 6));
            let x = lh(m, args).wrapping_add(ox);
            let y = lh(m, off(args, 2)).wrapping_add(oy);
            m.sw(off(packet, 8), (x & 0xffff) | (y << 16));
            let w = lh(m, off(args, 0x10));
            let h = lh(m, off(args, 0x12));
            m.sw(off(packet, 0x10), ((w << 2) & 0xffff) | (h << 16));
            let tx = lh(m, off(args, 0xc));
            let rounded = if (tx as i32) < 0 {
                tx.wrapping_add(0x3f)
            } else {
                tx
            };
            let ty = lh(m, off(args, 0xe));
            let u = sra(tx.wrapping_sub(sra(rounded, 6) << 6) << 16, 14);
            let rounded = if (ty as i32) < 0 {
                ty.wrapping_add(0xff)
            } else {
                ty
            };
            let v = sra(ty.wrapping_sub(sra(rounded, 8) << 8) << 16, 8);
            let cy = lh(m, off(args, 0xa));
            let cx = lhu(m, off(args, 8));
            let clut = ((cy << 6) | ((cx >> 4) & 0x3f)) << 16;
            m.sw(off(packet, 0xc), u | v | clut);
            colour_and_code(m, args, 4, packet, 4, 0x64);
            link_head(m, ot, packet, 4);
        }
    }
    m.pop_frame(0x28);
    next
}

/// 800DB254 (command 2): a DR_MODE with the texture page of {+0, +2} and
/// mode +7, linked into the animation's bin. Returns the next command
/// (+0xC).
pub fn draw_mode(m: &mut Machine, args: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let packet = reserve(m, 0xc);
    let mode = lbu(m, off(args, 7));
    let ty = lhu(m, off(args, 2));
    let tx = lhu(m, args);
    m.sw(off(frame, 0x10), 0);
    let tpage = ((mode & 3) << 5)
        | sext16(sra((ty & 0x100) << 16, 20))
        | ((tx & 0x3ff) >> 6)
        | (sext16(ty & 0x200) << 2);
    sdk::set_draw_mode(m, packet, 0, 0, tpage, 0);
    let anim = animation(m);
    let bin = lh(m, off(anim, 0xe));
    add_prim(m, ot, bin, packet);
    m.pop_frame(0x30);
    off(args, 0xc)
}

/// 800DB374 (command 3): a textured POLY_FT4 over the clip rectangle (from
/// {+0, +2, +0x14, +0x16}, offset by the record): texture {+0xC, +0xE}
/// (mirrored for a negative size), texture size {+0x10, +0x12} (in 8- or
/// 4-bit units by +0x1E), CLUT {+8, +0xA}, colour +4..+6, blending +7
/// (0xFF: none) and its mode, semi-transparency +0x1E; condition
/// +0x18/+0x1A. Returns the next command (+0x20).
pub fn draw_quad(m: &mut Machine, args: u32, ot: u32, record: u32) -> u32 {
    m.push_frame(0x30);
    let next = off(args, 0x20);
    if condition_holds(m, args, 0x18) {
        draw_quad_body(m, args, ot, record);
    }
    m.pop_frame(0x30);
    next
}

fn draw_quad_body(m: &mut Machine, args: u32, ot: u32, record: u32) {
    let packet = reserve(m, 0x28);
    let blend = lbu(m, off(args, 7));
    let blended = blend != 0xff;
    let mode = if blended { lbu(m, off(args, 7)) } else { 0 };
    let rect = m.lw(RECT);
    let y = lh(m, off(args, 2));
    let x = lhu(m, args);
    m.sw(rect, x | (y << 16));
    let h = lh(m, off(args, 0x16));
    let w = lhu(m, off(args, 0x14));
    m.sw(off(rect, 4), w | (h << 16));
    if gate(m) == 0 {
        return;
    }
    let scale = if lbu(m, off(args, 0x1e)) != 0 { 2 } else { 4 };
    let tx = lh(m, off(args, 0xc));
    let rounded = if (tx as i32) < 0 {
        tx.wrapping_add(0x3f)
    } else {
        tx
    };
    let u = mul(sext16(tx.wrapping_sub(sra(rounded, 6) << 6)), scale);
    let u0 = if (lh(m, off(args, 0x14)) as i32) < 0 {
        u.wrapping_sub(1)
    } else {
        u
    };
    let ty = lh(m, off(args, 0xe));
    let rounded = if (ty as i32) < 0 {
        ty.wrapping_add(0xff)
    } else {
        ty
    };
    let v = ty.wrapping_sub(sra(rounded, 8) << 8);
    let v0 = if (lh(m, off(args, 0x16)) as i32) < 0 {
        v.wrapping_sub(1)
    } else {
        v
    };
    let tw = mul(lh(m, off(args, 0x10)), scale);
    let width = lh(m, off(args, 0x14));
    let mut du = if width == tw {
        width
    } else {
        tw.wrapping_sub(1)
    };
    let height = lh(m, off(args, 0x16));
    let th = lh(m, off(args, 0x12));
    let th_raw = lhu(m, off(args, 0x12));
    let mut dv = if height == th {
        th_raw
    } else {
        th_raw.wrapping_sub(1)
    };
    if sext16(u0).wrapping_add(sext16(du)) == 0x100 {
        du = du.wrapping_sub(1);
    }
    if sext16(v0).wrapping_add(sext16(dv)) == 0x100 {
        dv = dv.wrapping_sub(1);
    }
    let r = lbu(m, off(args, 4));
    m.sb(off(packet, 4), r as u8);
    let g = lbu(m, off(args, 5));
    m.sb(off(packet, 5), g as u8);
    let b = lbu(m, off(args, 6));
    m.sb(off(packet, 3), 9);
    m.sb(off(packet, 7), 0x2c);
    m.sb(off(packet, 6), b as u8);
    m.sb(off(packet, 7), if blended { 0x2e } else { 0x2c });
    let u0 = sext16(u0);
    let v0 = sext16(v0);
    let v_top = v0 << 8;
    let cy = lh(m, off(args, 0xa));
    let cx = lhu(m, off(args, 8));
    let clut = ((cy << 6) | ((cx >> 4) & 0x3f)) << 16;
    let v_bottom = v0.wrapping_add(sext16(dv)) << 8;
    m.sw(off(packet, 0xc), u0 | v_top | clut);
    let semi = lbu(m, off(args, 0x1e));
    let ty = lhu(m, off(args, 0xe));
    let tx = lhu(m, off(args, 0xc));
    m.sw(off(packet, 0x1c), u0 | v_bottom);
    let u1 = u0.wrapping_add(sext16(du));
    m.sw(off(packet, 0x24), u1 | v_bottom);
    let tpage = ((semi & 3) << 7)
        | ((mode & 3) << 5)
        | sext16(sra((ty & 0x100) << 16, 20))
        | ((tx & 0x3ff) >> 6)
        | (sext16(ty & 0x200) << 2);
    m.sw(off(packet, 0x14), u1 | v_top | (tpage << 16));
    let rect = m.lw(RECT);
    let point = |m: &mut Machine, right: bool, bottom: bool| {
        let oy = lh(m, off(record, 6));
        let ox = lh(m, off(record, 4));
        let mut y = lh(m, off(rect, 2)).wrapping_add(oy);
        if bottom {
            y = y.wrapping_add(lh(m, off(rect, 6)));
        }
        let mut x = lh(m, rect).wrapping_add(ox);
        if right {
            x = x.wrapping_add(lh(m, off(rect, 4)));
        }
        (y << 16) | (x & 0xffff)
    };
    let p = point(m, false, false);
    m.sw(off(packet, 8), p);
    let p = point(m, true, false);
    m.sw(off(packet, 0x10), p);
    let p = point(m, false, true);
    m.sw(off(packet, 0x18), p);
    let p = point(m, true, true);
    m.sw(off(packet, 0x20), p);
    link_head(m, ot, packet, 9);
}

/// 800DB834 (command 4): a DR_AREA of {+4, +6, +8, +0xA} linked into
/// entry `[+0x10]` of `ot`. Returns the next command (+0x14).
pub fn draw_area(m: &mut Machine, args: u32, ot: u32) -> u32 {
    let frame = m.push_frame(0x30);
    let packet = reserve(m, 0xc);
    let rect = off(args, 4);
    for k in 0..4 {
        let value = lhu(m, off(rect, 2 * k));
        m.sh(off(frame, 0x10 + 2 * k), value as u16);
    }
    sdk::set_draw_area(m, packet, off(frame, 0x10));
    let bin = m.lw(off(rect, 0xc));
    add_prim(m, ot, bin, packet);
    m.pop_frame(0x30);
    off(args, 0x14)
}

/// 800DB910 (JP; PAL 800D54D8, changed; command 5): the animation of the
/// following commands (16 bytes). Returns the next command (+0x10).
pub fn set_animation(m: &mut Machine, args: u32) -> u32 {
    let v = visual(m);
    m.sw(off(v, 0x2c), args);
    off(args, 0x10)
}

/// 800DB928 (command 6): a LINE_F2 over the clip rectangle's two corners
/// (from {+0, +2, +4, +6}, offset by the record), colour +8..+0xA blended
/// by +0xB; condition +0xC/+0xE. Returns the next command (+0x14).
pub fn draw_line(m: &mut Machine, args: u32, ot: u32, record: u32) -> u32 {
    m.push_frame(0x30);
    let next = off(args, 0x14);
    if condition_holds(m, args, 0xc) {
        let packet = reserve(m, 0x10);
        set_rect(m, args);
        if gate(m) != 0 {
            let rect = m.lw(RECT);
            for (k, offset) in [(0u32, 4u32), (1, 6), (2, 4), (3, 6)] {
                let value = lhu(m, off(rect, 2 * k));
                let delta = lhu(m, off(record, offset));
                m.sh(off(packet, 8 + 2 * k), value.wrapping_add(delta) as u16);
            }
            colour_and_code(m, args, 8, packet, 3, 0x40);
            let anim = animation(m);
            let bin = lh(m, off(anim, 0xe));
            add_prim(m, ot, bin, packet);
        }
    }
    m.pop_frame(0x30);
    next
}

// ---- screens ---------------------------------------------------------------------------

/// 800DBB30: the four colour bars (black to red, green, blue, white) of the
/// screen settings as POLY_G4 packets from the menu state's cursor.
pub fn colour_bars(m: &mut Machine) {
    let frame = m.push_frame(0x58);
    let r = off(frame, 0x10);
    let bars: [(u16, u16, [u8; 3]); 4] = [
        (0x5a, 0x5f, [0xff, 0, 0]),
        (0x66, 0x6b, [0, 0xff, 0]),
        (0x71, 0x76, [0, 0, 0xff]),
        (0x81, 0x86, [0xff, 0xff, 0xff]),
    ];
    for (i, (left, right, colour)) in bars.into_iter().enumerate() {
        m.sh(off(r, 4), right);
        m.sh(off(r, 0xc), right);
        m.sh(r, left);
        m.sh(off(r, 2), 0xd5);
        m.sh(off(r, 6), 0xd5);
        m.sh(off(r, 8), left);
        m.sh(off(r, 0xa), 0x17d);
        m.sh(off(r, 0xe), 0x17d);
        if i == 0 {
            m.sb(off(r, 0x13), 0);
            for k in 0..3 {
                m.sb(off(r, 0x10 + k), 0);
                m.sb(off(r, 0x14 + k), 0);
            }
        }
        for k in 0..3 {
            m.sb(off(r, 0x18 + k), colour[k as usize]);
        }
        for k in 0..3 {
            m.sb(off(r, 0x1c + k), colour[k as usize]);
        }
        let state = m.lw(MENU_STATE);
        let packet = m.lw(off(state, 8));
        let ot = current_bin(m);
        let end = poly_g4(m, r, packet, ot);
        let state = m.lw(MENU_STATE);
        m.sw(off(state, 8), end);
    }
    m.pop_frame(0x58);
}

/// 800DBD20: the name of account `account` at `x`, `y` (font 4, skewed by
/// timer 15) and, with flag 2 of +0x54, the cursor box after it (LINE_F2
/// pairs from the menu state's cursor).
pub fn account_box(m: &mut Machine, x: u32, y: u32, account: u32) {
    let frame = m.push_frame(0x38);
    set_font(m, 4);
    set_blend(m, 2);
    set_skew(m, 1, 0x1e, 0x28);
    let line = timer_value(m, 0xf);
    set_line(m, line);
    set_color(m, 0x80, 0x80, 0x80);
    set_position(m, sext16(x), sext16(y));
    let name = account_name(m, account);
    print(m, [name, 0, 0, 0]);
    let v = visual(m);
    if m.lw(off(v, 0x54)) & 4 != 0 {
        let r = off(frame, 0x10);
        m.sb(off(r, 0xb), 0);
        for k in 0..3 {
            m.sb(off(r, 8 + k), 0x28);
        }
        get_position(m, off(frame, 0x20), off(frame, 0x22));
        // (dx0, dy0, dx1, dy1) of the six lines
        let lines: [(u32, u32, u32, u32); 6] = [
            (0, 0, 8, 0),
            (0, 9, 8, 9),
            (0, 0x12, 8, 0x12),
            (0, 0, 0, 0x12),
            (4, 0, 4, 0x12),
            (8, 0, 8, 0x12),
        ];
        for (dx0, dy0, dx1, dy1) in lines {
            let cx = lhu(m, off(frame, 0x20));
            let cy = lhu(m, off(frame, 0x22));
            m.sh(r, cx.wrapping_add(dx0) as u16);
            m.sh(off(r, 2), cy.wrapping_add(dy0) as u16);
            m.sh(off(r, 4), cx.wrapping_add(dx1) as u16);
            m.sh(off(r, 6), cy.wrapping_add(dy1) as u16);
            let state = m.lw(MENU_STATE);
            let packet = m.lw(off(state, 8));
            let ot = current_bin(m);
            let end = line2_double(m, r, packet, ot);
            let state = m.lw(MENU_STATE);
            m.sw(off(state, 8), end);
        }
    }
    m.pop_frame(0x38);
}

/// 800DBF6C: the account box position and flags 0 (`box` 1) and 2
/// (`cursor` 1) of +0x54.
pub fn set_account_box(m: &mut Machine, x: u32, y: u32, shown: u32, cursor: u32) {
    let v = visual(m);
    m.sh(off(v, 0x50), x as u16);
    m.sh(off(v, 0x52), y as u16);
    let flags = m.lw(off(v, 0x54));
    m.sw(
        off(v, 0x54),
        if shown == 1 { flags | 1 } else { flags & !1 },
    );
    let v = visual(m);
    let flags = m.lw(off(v, 0x54));
    m.sw(
        off(v, 0x54),
        if cursor == 1 { flags | 4 } else { flags & !4 },
    );
}

/// 800DBFE4: draw the account box (account 0) when flag 0 of +0x54 is set.
pub fn draw_account_box(m: &mut Machine) {
    m.push_frame(0x18);
    let v = visual(m);
    if m.lw(off(v, 0x54)) & 1 != 0 {
        let x = lh(m, off(v, 0x50));
        let y = lh(m, off(v, 0x52));
        account_box(m, x, y, 0);
    }
    m.pop_frame(0x18);
}

/// 800DC028: the account list flags of +0x54: `mode` 2 sets 0xA, 1 sets 2
/// and clears 8, else clears 0xA (the original's mask -0xB).
pub fn set_list_mode(m: &mut Machine, mode: u32) {
    let v = visual(m);
    let flags = m.lw(off(v, 0x54));
    let flags = match mode {
        2 => flags | 0xa,
        1 => (flags | 2) & 0xffff_fff7,
        _ => flags & 0xffff_fff5,
    };
    m.sw(off(v, 0x54), flags);
}

/// One account line of [`account_list`]: bright with the account's name
/// when present, else dim "empty"; the condition byte `condition` (when
/// given) set to 0 or 1.
fn account_line(m: &mut Machine, present: bool, account: u32, condition: Option<u32>) {
    let text = if present {
        set_color(m, 0x80, 0x80, 0x80);
        if let Some(index) = condition {
            set_condition(m, index, 0);
        }
        account_name(m, account)
    } else {
        set_color(m, 0x40, 0x40, 0x40);
        if let Some(index) = condition {
            set_condition(m, index, 1);
        }
        EMPTY
    };
    print(m, [text, 0, 0, 0]);
}

/// 800DC098: the three saved accounts (page `page` 0 or 1; in `mode` 1 the
/// menu state's +0x5D account counts as absent) when flag 1 of +0x54 is
/// set.
pub fn account_list(m: &mut Machine, page: u32, mode: u32) {
    m.push_frame(0x20);
    let mut bits = account_bits(m);
    if mode == 1 {
        let state = m.lw(MENU_STATE);
        let excluded = lbu(m, off(state, 0x5d));
        bits &= !(1u32 << (excluded & 31));
    }
    let v = visual(m);
    if m.lw(off(v, 0x54)) & 2 != 0 {
        if page == 0 {
            set_font(m, 4);
            set_skew(m, 1, 1, 1);
            set_blend(m, 2);
            set_line(m, 1);
            for (k, y) in [(0u32, 0x96u32), (1, 0xcc), (2, 0x101)] {
                set_position(m, 0x6a, y);
                account_line(m, sra(bits, k) & 1 != 0, k + 1, None);
            }
        }
        if page == 1 {
            set_font(m, 4);
            set_blend(m, 2);
            set_skew(m, 1, 1, 1);
            set_line(m, 1);
            for (k, y, condition) in [
                (0u32, 0xbdu32, Some(2)),
                (1, 0xf2, None),
                (2, 0x125, Some(4)),
            ] {
                let v = visual(m);
                let x = if m.lw(off(v, 0x54)) & 8 != 0 {
                    0x51
                } else {
                    0x6a
                };
                set_position(m, x, y);
                account_line(m, sra(bits, k) & 1 != 0, k + 1, condition);
            }
        }
    }
    m.pop_frame(0x20);
}

/// One link-mode entry of [`link_mode`]: normal, or dimmed (blend 1,
/// colour 8, 0x30, 0x28) when `enabled` is false.
fn link_entry(m: &mut Machine, enabled: bool, text: u32) {
    if enabled {
        print(m, [text, 0, 0, 0]);
    } else {
        set_blend(m, 1);
        set_color(m, 8, 0x30, 0x28);
        print(m, [text, 0, 0, 0]);
        set_blend(m, 2);
        set_color(m, 0x80, 0x80, 0x80);
    }
}

/// 800DC434 (callback 7): the title's link modes: "New Account",
/// "Download" (dimmed unless a card slot reports status 5, 800D3D90) and
/// "re-Open" (dimmed unless context +0x964 bit 0).
pub fn link_mode(m: &mut Machine) {
    m.push_frame(0x20);
    let card = if call(m, SLOT_STATE, [0, 0, 0, 0]) == 5 {
        true
    } else {
        call(m, SLOT_STATE, [1, 0, 0, 0]) == 5
    };
    let ctx = context(m);
    let reopen = m.lw(off(ctx, 0x964)) & 1 != 0;
    set_font(m, 4);
    set_blend(m, 2);
    set_skew(m, 1, 0x1e, 0x28);
    let line = timer_value(m, 3);
    set_line(m, line);
    set_color(m, 0x80, 0x80, 0x80);
    set_position(m, 0x6a, 0x96);
    print(m, [NEW_ACCOUNT, 0, 0, 0]);
    set_position(m, 0x6a, 0xcc);
    link_entry(m, card, DOWNLOAD);
    set_position(m, 0x6a, 0x101);
    link_entry(m, reopen, REOPEN);
    set_skew(m, 1, 0x1e, 0x28);
    let line = timer_value(m, 4);
    set_line(m, line);
    set_color(m, 0x80, 0x80, 0x80);
    m.pop_frame(0x20);
}

/// 800DC5EC (callback 11): the 50 texts {x, y, text} of the menu state's
/// list +0x64 + 4 * page (+0x5C), font 3, skewed by timer 15.
pub fn text_list(m: &mut Machine) {
    m.push_frame(0x20);
    let state = m.lw(MENU_STATE);
    let page = lbu(m, off(state, 0x5c));
    let mut entry = m.lw(off(off(state, page << 2), 0x64));
    set_font(m, 3);
    set_blend(m, 2);
    set_skew(m, 1, 0x1e, 0x28);
    let line = timer_value(m, 0xf);
    set_line(m, line);
    set_color(m, 0x80, 0x80, 0x80);
    for _ in 0..0x32 {
        let x = lh(m, entry);
        let y = lh(m, off(entry, 2));
        set_position(m, x, y);
        print(m, [off(entry, 4), 0, 0, 0]);
        entry = off(entry, 8);
    }
    m.pop_frame(0x20);
}

// ---- scripts ---------------------------------------------------------------------------

/// 800DC6C4: run script `index`: each command (0..6) through its handler
/// (0x800DD934) with the current ordering table and the script's record,
/// until -1 (another value repeats forever, as in the original); then the
/// script's callback (0x800DD950).
pub fn run_script(m: &mut Machine, index: u32) {
    m.push_frame(0x28);
    let v = visual(m);
    let table = m.lw(off(v, 0x24));
    let mut command = m.lw(off(index << 2, table));
    let ot = m.lw(off(v, 0x10));
    if m.lw(command) != u32::MAX {
        let record = (index << 1).wrapping_add(index) << 2;
        loop {
            let opcode = m.lw(command);
            if opcode < 7 {
                let handler = m.lw(off(opcode << 2, HANDLERS));
                let v = visual(m);
                let records = m.lw(off(v, 0x20));
                command = call(
                    m,
                    handler,
                    [off(command, 4), ot, records.wrapping_add(record), 0],
                );
            }
            if m.lw(command) == u32::MAX {
                break;
            }
            if m.fault().is_some() {
                break;
            }
        }
    }
    let callback = m.lw(off(index << 2, CALLBACKS));
    call(m, callback, [0; 4]);
    m.pop_frame(0x28);
}

/// 800DC7AC: run the scripts whose bits are set in +0x1C (bit n: script n).
pub fn run_scripts(m: &mut Machine) {
    m.push_frame(0x20);
    let v = visual(m);
    let mut bits = m.lw(off(v, 0x1c));
    let mut index = 0u32;
    while bits != 0 {
        if bits & 1 != 0 {
            run_script(m, index);
        }
        index = index.wrapping_add(1);
        bits >>= 1;
        if !slt(index, 0x20) {
            break;
        }
    }
    m.pop_frame(0x20);
}

/// 800DC810: the visual block: 32 cleared script records, the script
/// table, two ordering tables and packet areas, the default animation
/// (+0x30..+0x3C cleared, +0x3E 1), the timers and condition bytes cleared,
/// no running script; then every record reset ([`reset_record`]).
pub fn init_visuals(m: &mut Machine) {
    m.push_frame(0x20);
    let block = arena::alloc(m, 0x58);
    m.sw(VISUAL, block);
    let records = arena::alloc(m, 0x180);
    let v = visual(m);
    m.sw(off(v, 0x20), records);
    let table = arena::alloc(m, 0x80);
    let v = visual(m);
    m.sw(off(v, 0x24), table);
    for i in 0..0x20u32 {
        let at = i.wrapping_mul(0xc);
        let records = m.lw(off(v, 0x20));
        m.sh(off(at, records), 0);
        let records = m.lw(off(v, 0x20));
        m.sh(off(off(at, records), 2), 0);
    }
    for buffer in 0..2u32 {
        let ot = arena::alloc(m, 0x40);
        let v = visual(m);
        m.sw(off(off(v, buffer << 2), 0x14), ot);
        let packets = arena::alloc(m, 0x8000);
        let v = visual(m);
        m.sw(off(off(v, buffer << 2), 8), packets);
    }
    let v = visual(m);
    m.sh(off(v, 0x3e), 1);
    for offset in (0x30..=0x3c).step_by(2) {
        m.sh(off(v, offset), 0);
    }
    m.sw(off(v, 0x2c), off(v, 0x30));
    let values = arena::alloc(m, 0x20);
    let v = visual(m);
    m.sw(off(v, 0x40), values);
    let states = arena::alloc(m, 0x20);
    let v = visual(m);
    m.sw(off(v, 0x48), states);
    let limits = arena::alloc(m, 0x20);
    let v = visual(m);
    m.sw(off(v, 0x44), limits);
    for i in 0..0x10u32 {
        let at = i << 1;
        let values = m.lw(off(v, 0x40));
        m.sh(off(at, values), 0);
        let states = m.lw(off(v, 0x48));
        m.sh(off(at, states), 0);
        let limits = m.lw(off(v, 0x44));
        m.sh(off(at, limits), 0);
    }
    let conditions = arena::alloc(m, 0x40);
    let v = visual(m);
    m.sw(off(v, 0x4c), conditions);
    for i in 0..0x40 {
        let v = visual(m);
        let conditions = m.lw(off(v, 0x4c));
        m.sb(off(conditions, i), 0);
    }
    let v = visual(m);
    m.sw(off(v, 0x1c), 0);
    m.sw(off(v, 0x54), 0);
    for i in 0..0x20 {
        reset_record(m, i);
    }
    m.pop_frame(0x20);
}

/// 800DC9EC (JP; PAL 800D566C, changed): start the visual frame: the
/// current buffer's ordering table and packet area, the table cleared and
/// spliced in front of the entry `ot`.
pub fn begin_visual_frame(m: &mut Machine, ot: u32) {
    m.push_frame(0x20);
    let ctx = context(m);
    let buffer = m.lw(off(ctx, 4));
    let v = visual(m);
    let table = m.lw(off(off(v, buffer << 2), 0x14));
    m.sw(off(v, 0x10), table);
    let buffer = m.lw(off(ctx, 4));
    let table = m.lw(off(v, 0x10));
    let packets = m.lw(off(off(v, buffer << 2), 8));
    m.sw(off(v, 4), packets);
    sdk::clear_otag_r(m, table, 0x10);
    let v = visual(m);
    let table = m.lw(off(v, 0x10));
    let next = m.lw(ot);
    let first = m.lw(table);
    m.sw(table, (first & 0xff00_0000) | (next & 0x00ff_ffff));
    let entry = m.lw(ot);
    let table = m.lw(off(v, 0x10));
    m.sw(
        ot,
        (entry & 0xff00_0000) | (table.wrapping_add(0x3c) & 0x00ff_ffff),
    );
    m.pop_frame(0x20);
}

/// 800DCAA4: the visual frame's work: the clip rectangle on the
/// scratchpad, VSync(1), the packet cursor of the current buffer, the
/// scripts and the timers.
pub fn visual_frame(m: &mut Machine) {
    m.push_frame(0x18);
    m.sw(RECT, 0x1f80_0000);
    sdk::vsync(m, 1);
    let ctx = context(m);
    let buffer = m.lw(off(ctx, 4));
    let v = visual(m);
    let packets = m.lw(off(off(v, buffer << 2), 8));
    m.sw(off(v, 4), packets);
    run_scripts(m);
    tick_timers(m);
    m.pop_frame(0x18);
}

/// The routines of this file by JP address, while module 1 is loaded.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if !loaded(m) {
        return None;
    }
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_a6fc => current_bin(m),
        0x800d_a714 => {
            set_record(m, a0, a1, a2, a3);
            0
        }
        0x800d_a77c => {
            reset_record(m, a0);
            0
        }
        0x800d_a7a8 => {
            mirror_record(m, a0);
            0
        }
        0x800d_a800 => record_state(m, a0),
        0x800d_a828 => {
            set_scripts(m, a0);
            0
        }
        0x800d_a838 => {
            set_timer(m, a0, a1, a2, a3);
            0
        }
        0x800d_a898 => timer_state(m, a0),
        0x800d_a8bc => timer_value(m, a0),
        0x800d_a8e0 => {
            tick_timers(m);
            0
        }
        0x800d_a9a8 => {
            set_condition(m, a0, a1);
            0
        }
        0x800d_a9d4 => gate_always(),
        0x800d_a9dc => gate_clip(m),
        0x800d_ac98 => gate_done(m),
        0x800d_acd0 => gate_grow(m),
        0x800d_ae44 => draw_tile(m, a0, a1, a2),
        0x800d_b024 => draw_sprite(m, a0, a1, a2),
        0x800d_b254 => draw_mode(m, a0, a1),
        0x800d_b374 => draw_quad(m, a0, a1, a2),
        0x800d_b834 => draw_area(m, a0, a1),
        0x800d_b910 => set_animation(m, a0),
        0x800d_b928 => draw_line(m, a0, a1, a2),
        0x800d_bb30 => {
            colour_bars(m);
            0
        }
        0x800d_bd20 => {
            account_box(m, a0, a1, a2);
            0
        }
        0x800d_bf6c => {
            set_account_box(m, a0, a1, a2, a3);
            0
        }
        0x800d_bfe4 => {
            draw_account_box(m);
            0
        }
        0x800d_c028 => {
            set_list_mode(m, a0);
            0
        }
        0x800d_c098 => {
            account_list(m, a0, a1);
            0
        }
        // the callback table's `jr ra; nop` stubs return at once, `v0` as the
        // caller had it, which a dispatch result cannot carry (`run_script`
        // ignores it): 0, as the oracle reads it
        0x800d_c424 | 0x800d_c42c | 0x800d_c5e4 | 0x800d_c694 | 0x800d_c69c | 0x800d_c6a4
        | 0x800d_c6ac | 0x800d_c6b4 | 0x800d_c6bc => 0,
        0x800d_c434 => {
            link_mode(m);
            0
        }
        0x800d_c5ec => {
            text_list(m);
            0
        }
        0x800d_c6c4 => {
            run_script(m, a0);
            0
        }
        0x800d_c7ac => {
            run_scripts(m);
            0
        }
        0x800d_c810 => {
            init_visuals(m);
            0
        }
        0x800d_c9ec => {
            begin_visual_frame(m, a0);
            0
        }
        0x800d_caa4 => {
            visual_frame(m);
            0
        }
        _ => return None,
    })
}
