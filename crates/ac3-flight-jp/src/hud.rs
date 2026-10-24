//! HUD and overlays drawn by the flight frame body 800339E0: 8003A2B8,
//! 800337BC, 800337DC and 80033814 with their call trees.
//!
//! Every routine follows its original instructions and store order. Pointer
//! arguments are guest addresses, as in the original ABI. Locals whose
//! address the original passes to a callee live in a guest stack frame
//! (`Machine::push_frame` with the original frame size); routines whose
//! callees use such frames allocate their own original frames too, so that
//! nested frames keep their original addresses. Other modules call these
//! routines by original address through [`dispatch`].
//!
//! Layout: core helpers and 8003B7B0 here; `popups` (800337BC), `stream`
//! (800337DC) with `stream_draw`, `status` (8003A2B8) with `tree_39d84`,
//! and `compose` (80033814, 8003A0B4, 8003C5B0, 80063E4C) with the drawer
//! subtrees `tree_40a24`, `tree_3bdf8_a`/`_b`, `tree_3e828` with
//! `marker_shapes`, `tree_62680_a`/`_b` with `tree_62ea0`, `tree_5cbf4`,
//! `tree_41ff0` and `extras`. Routines owned by other modules are called by
//! original address through `dispatch::call`.
use crate::{
    common::{CONTEXT, ENTITIES, GAMEPLAY},
    machine::{Machine, SmallData},
};

/// Low 24 bits of an ordering-table or packet tag word (the link).
pub const LINK: u32 = 0x00ff_ffff;
/// High 8 bits of a tag word (the packet length).
pub const LENGTH: u32 = 0xff00_0000;

/// `[gp+0x2E0]`: the gameplay block (+0x10 ordering table, +0x14 next free
/// primitive).
pub fn gameplay(m: &mut Machine) -> u32 {
    m.gw(GAMEPLAY)
}

/// `[gp+0x2EC]`: the game context.
pub fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT)
}

/// `[gp+0x2E8]`: the 64-entry entity pointer table.
pub fn entities(m: &mut Machine) -> u32 {
    m.gw(ENTITIES)
}

/// libgpu `addPrim(slot, prim)` as compiled inline: the packet tag keeps its
/// length and takes the slot's link, then the slot (reloaded) links to the
/// packet.
pub fn add_prim(m: &mut Machine, slot: u32, prim: u32) {
    let tag = m.lw(prim);
    let link = m.lw(slot);
    m.sw(prim, (tag & LENGTH) | (link & LINK));
    let link = m.lw(slot);
    m.sw(slot, (link & LENGTH) | (prim & LINK));
}

/// Signed `div` quotient. The compiled code guards a zero divisor with
/// `break 7` and `0x80000000 / -1` with `break 6`, so the original traps in
/// those cases; the translation keeps the R3000 quotient there (-1, or 1 for
/// a negative dividend, for a zero divisor; `i32::MIN` for the overflow).
pub fn mips_div(numerator: i32, divisor: i32) -> i32 {
    if divisor == 0 {
        if numerator < 0 { 1 } else { -1 }
    } else {
        numerator.wrapping_div(divisor)
    }
}

/// 8002DB4C: start sound effect `id` (37 callers across the engine; owned
/// by `script`, called by address). Returns the voice slot or 0xFF.
pub fn sound_2d724(m: &mut Machine, id: u32) -> u32 {
    crate::dispatch::call(m, 0x8002_db4c, [id, 0, 0, 0])
}

/// 800B1074/800B107C/800B1084: the three glyph fonts of [`glyph_3b00c`]
/// (u, v, -, height, then the width halfword / per-code u stride word at +4).
const GLYPH_FONTS: [u32; 3] = [0x800b_1074, 0x800b_107c, 0x800b_1084];

/// 8003B7B0: fill the sprite `prim` (`SPRT`) with glyph `code` of font
/// `font` (0, 1, else 2) at (`x`, `y`). `color` 0..3 selects green, red,
/// yellow or grey (0x83 components); other values keep the color bytes.
/// Codes 10, 11 and -1 are fixed glyphs; any other code is `u = font.u +
/// code * stride`. The original takes `prim` and `color` as stack arguments
/// (`[sp+0x10]`, `[sp+0x14]`).
pub fn glyph_3b00c(m: &mut Machine, code: u32, x: u32, y: u32, font: u32, prim: u32, color: u32) {
    let table = GLYPH_FONTS[font.min(2) as usize];
    let rgb = match color {
        0 => Some([0, 0x83, 0]),
        1 => Some([0x83, 0, 0]),
        2 => Some([0x83, 0x83, 0]),
        3 => Some([0x83, 0x83, 0x83]),
        _ => None,
    };
    if let Some([r, g, b]) = rgb {
        m.sb(prim + 4, r);
        m.sb(prim + 5, g);
        m.sb(prim + 6, b);
    }
    match code {
        10 => {
            m.sh(prim + 0xa, y.wrapping_add(3) as u16);
            m.sh(prim + 0x10, 4);
            m.sh(prim + 8, x as u16);
            m.sh(prim + 0x12, 3);
            m.sb(prim + 0xc, 0);
            m.sb(prim + 0xd, 0);
        }
        11 => {
            m.sh(prim + 0xa, y.wrapping_add(2) as u16);
            m.sh(prim + 0x10, 6);
            m.sh(prim + 0x12, 2);
            m.sh(prim + 8, x as u16);
            m.sb(prim + 0xc, 0x2c);
            m.sb(prim + 0xd, 10);
        }
        0xffff_ffff => {
            m.sh(prim + 0x10, 4);
            m.sh(prim + 8, x as u16);
            m.sh(prim + 0xa, y as u16);
            m.sh(prim + 0x12, 6);
            m.sb(prim + 0xc, 0x28);
            m.sb(prim + 0xd, 6);
        }
        _ => {
            m.sh(prim + 8, x as u16);
            m.sh(prim + 0xa, y as u16);
            let width = m.lhu(table + 4);
            m.sh(prim + 0x10, width);
            let height = m.lbu(table + 3);
            m.sh(prim + 0x12, u16::from(height));
            let stride = m.lw(table + 4);
            let offset = (code as i32).wrapping_mul(stride as i32) as u32;
            let u = u32::from(m.lbu(table)).wrapping_add(offset);
            m.sb(prim + 0xc, u as u8);
            let v = m.lbu(table + 1);
            m.sb(prim + 0xd, v);
        }
    }
}

pub mod popups {
    //! 800337BC -> 8004C998: the popup sprite list at `[gp+0x478]`. Each of
    //! the `+0x110` entries (8 bytes from +8: state byte, picture byte,
    //! counter halfword, x, y) is drawn as a sprite (state 1) or a quad that
    //! fades in (state 2) or shrinks away (state 3), into the per-buffer
    //! primitive block `[gp+0x474]`: sprites (0x14 bytes) from +0, quads
    //! (0x28 bytes) from +0x280, a texture-page packet at +0x780.
    use super::*;

    /// `[gp+0x478]`: the popup list.
    pub const LIST: u32 = 0x478;
    /// `[gp+0x474]`: this frame's primitive block (from list +0, +4 by buffer).
    pub const BLOCK: u32 = 0x474;
    /// 800B1914: per-picture texture rectangles (u, v, w, h bytes).
    const PICTURES: u32 = 0x800b_1914;
    /// Context flag 0x20000000: frame step paused (counters do not advance).
    const PAUSED: u32 = 0x2000_0000;

    /// 800337BC: draw the popup list.
    pub fn overlay_33524(m: &mut Machine) {
        popups_4b248(m);
    }

    /// 8004C998: with any of gameplay +4 flags 0x2C0E set, rebuild and link
    /// the popup primitives (counters advance unless paused); otherwise
    /// clear a still-active list.
    pub fn popups_4b248(m: &mut Machine) {
        let gameplay = gameplay(m);
        if m.lw(gameplay + 4) & 0x2c0e == 0 {
            let list = m.gw(LIST);
            if m.lw(list + 0x11c) != 0xff {
                clear_4b74c(m);
            }
            return;
        }
        begin_4b0d4(m);
        build_4b2d8(m);
        let context = context(m);
        if !m.lw(context) & PAUSED != 0 {
            link_4b11c(m);
        }
    }

    /// 8004C824: reset the sprite and quad counts and select this buffer's
    /// primitive block.
    pub fn begin_4b0d4(m: &mut Machine) {
        let list = m.gw(LIST);
        let context = context(m);
        m.sw(list + 0x10c, 0);
        m.sw(list + 0x108, 0);
        let buffer = m.lw(context + 4);
        let block = m.lw(list.wrapping_add(buffer << 2));
        m.sw(crate::GP + BLOCK, block);
    }

    /// 8004C86C: link the quads, the sprites and the texture-page packet into
    /// ordering-table slot 0.
    pub fn link_4b11c(m: &mut Machine) {
        let list = m.gw(LIST);
        let gameplay = gameplay(m);
        let slot = m.lw(gameplay + 0x10);
        if (m.lw(list + 0x108) as i32) > 0 {
            let block = m.gw(BLOCK);
            let mut index = 0;
            let mut quad = block.wrapping_add(0x280);
            loop {
                index += 1;
                add_prim(m, slot, quad);
                quad = quad.wrapping_add(0x28);
                if index >= m.lw(list + 0x108) as i32 {
                    break;
                }
            }
        }
        let list = m.gw(LIST);
        if (m.lw(list + 0x10c) as i32) > 0 {
            let mut index = 0;
            let mut sprite = m.gw(BLOCK);
            loop {
                index += 1;
                add_prim(m, slot, sprite);
                sprite = sprite.wrapping_add(0x14);
                if index >= m.lw(list + 0x10c) as i32 {
                    break;
                }
            }
        }
        let block = m.gw(BLOCK);
        add_prim(m, slot, block.wrapping_add(0x780));
    }

    /// Advance an entry's counter (halfword +2) unless the frame is paused.
    fn step(m: &mut Machine, entry: u32) {
        let context = context(m);
        if !m.lw(context) & PAUSED != 0 {
            let counter = m.lhu(entry + 2);
            m.sh(entry + 2, counter.wrapping_add(1));
        }
    }

    /// The four texture corners of a quad from a picture rectangle.
    fn quad_uv(m: &mut Machine, quad: u32, picture: u32) {
        let u = m.lbu(picture);
        m.sb(quad + 0xc, u);
        let v = m.lbu(picture + 1);
        m.sb(quad + 0xd, v);
        let right = m.lbu(picture).wrapping_add(m.lbu(picture + 2));
        m.sb(quad + 0x14, right);
        let v = m.lbu(picture + 1);
        m.sb(quad + 0x15, v);
        let u = m.lbu(picture);
        m.sb(quad + 0x1c, u);
        let bottom = m.lbu(picture + 1).wrapping_add(m.lbu(picture + 3));
        m.sb(quad + 0x1d, bottom);
        let right = m.lbu(picture).wrapping_add(m.lbu(picture + 2));
        m.sb(quad + 0x24, right);
        let bottom = m.lbu(picture + 1).wrapping_add(m.lbu(picture + 3));
        m.sb(quad + 0x25, bottom);
    }

    /// Quad positions: (left, top, right, bottom) halfwords of the four
    /// vertices, in the original store order.
    fn quad_xy(m: &mut Machine, quad: u32, left: u32, top: u32, right: u32, bottom: u32) {
        m.sh(quad + 0xa, top as u16);
        m.sh(quad + 0x12, top as u16);
        m.sh(quad + 8, left as u16);
        m.sh(quad + 0x10, right as u16);
        m.sh(quad + 0x18, left as u16);
        m.sh(quad + 0x1a, bottom as u16);
        m.sh(quad + 0x20, right as u16);
        m.sh(quad + 0x22, bottom as u16);
    }

    /// Next free quad: `block + 0x280 + count * 0x28`.
    fn next_quad(m: &mut Machine, list: u32) -> u32 {
        let count = m.lw(list + 0x108);
        let block = m.gw(BLOCK);
        block.wrapping_add(count.wrapping_mul(0x28).wrapping_add(0x280))
    }

    /// 8004CA28: build this frame's popup primitives. State 1 is a 0x80-grey
    /// sprite until the counter reaches `[list+0x114]` (then state 3); state
    /// 2 a quad whose grey level is counter * 32, becoming state 1 at 8;
    /// state 3 a quad shrinking by `size >> counter` around its center until
    /// both halves are 0 (then free). With no entry left active the list is
    /// cleared (count 0, `+0x11C` = 0xFF).
    pub fn build_4b2d8(m: &mut Machine) {
        let list = m.gw(LIST);
        let tpage = u32::from(m.lhu(list + 0x118)).wrapping_add(0x7f58) as u16;
        let mut active = 0u32;
        if (m.lw(list + 0x110) as i32) > 0 {
            let mut index: i32 = 0;
            let mut offset = 8u32;
            loop {
                let list = m.gw(LIST);
                let entry = list.wrapping_add(offset);
                let picture = (u32::from(m.lbu(entry + 1)) << 2).wrapping_add(PICTURES);
                let state = m.lbu(entry);
                if state != 0 {
                    match state {
                        1 => sprite_state(m, list, entry, picture, tpage),
                        2 => fade_in_state(m, list, entry, picture, tpage),
                        3 => shrink_state(m, list, entry, picture, tpage),
                        _ => {}
                    }
                    if m.lbu(entry) != 0 {
                        active += 1;
                    }
                }
                let list = m.gw(LIST);
                index += 1;
                offset = offset.wrapping_add(8);
                if index >= m.lw(list + 0x110) as i32 {
                    break;
                }
            }
        }
        if active == 0 {
            let list = m.gw(LIST);
            m.sw(list + 0x110, 0);
            m.sw(list + 0x11c, 0xff);
        }
    }

    /// State 1: a sprite; after `[list+0x114]` steps it starts shrinking.
    fn sprite_state(m: &mut Machine, list: u32, entry: u32, picture: u32, tpage: u16) {
        let count = m.lw(list + 0x10c);
        let block = m.gw(BLOCK);
        let sprite = block.wrapping_add(count.wrapping_mul(0x14));
        let code = m.lbu(sprite + 7);
        m.sb(sprite + 4, 0x80);
        m.sb(sprite + 5, 0x80);
        m.sb(sprite + 6, 0x80);
        m.sb(sprite + 7, code | 2);
        let x = m.lhu(entry + 4);
        m.sh(sprite + 8, x);
        let y = m.lhu(entry + 6);
        m.sh(sprite + 0xa, y);
        let u = m.lbu(picture);
        m.sb(sprite + 0xc, u);
        let v = m.lbu(picture + 1);
        m.sh(sprite + 0xe, tpage);
        m.sb(sprite + 0xd, v);
        let w = m.lbu(picture + 2);
        let list = m.gw(LIST);
        m.sh(sprite + 0x10, u16::from(w));
        let h = m.lbu(picture + 3);
        m.sh(sprite + 0x12, u16::from(h));
        let count = m.lw(list + 0x10c);
        m.sw(list + 0x10c, count.wrapping_add(1));
        step(m, entry);
        let list = m.gw(LIST);
        let counter = i32::from(m.lh(entry + 2));
        if counter >= m.lw(list + 0x114) as i32 {
            m.sb(entry, 3);
            m.sh(entry + 2, 0);
        }
    }

    /// State 2: a quad brightening by 32 per step; after 8 steps a sprite.
    fn fade_in_state(m: &mut Machine, list: u32, entry: u32, picture: u32, tpage: u16) {
        let x = m.lh(entry + 4) as i32 as u32;
        let y = m.lh(entry + 6) as i32 as u32;
        let w = u32::from(m.lbu(picture + 2));
        let h = u32::from(m.lbu(picture + 3));
        let quad = next_quad(m, list);
        let code = m.lbu(quad + 7);
        quad_xy(m, quad, x, y, x.wrapping_add(w), y.wrapping_add(h));
        m.sb(quad + 7, code | 2);
        quad_uv(m, quad, picture);
        m.sh(quad + 0xe, tpage);
        m.sh(quad + 0x16, 0x56);
        let level = (m.lh(entry + 2) as i32 as u32) << 5;
        m.sb(quad + 4, level as u8);
        m.sb(quad + 5, level as u8);
        m.sb(quad + 6, level as u8);
        let list = m.gw(LIST);
        let count = m.lw(list + 0x108);
        m.sw(list + 0x108, count.wrapping_add(1));
        step(m, entry);
        if m.lh(entry + 2) >= 8 {
            m.sb(entry, 1);
            m.sh(entry + 2, 0);
        }
    }

    /// State 3: a quad shrinking toward its center by `size >> counter`;
    /// freed once both shifted sizes are 0.
    fn shrink_state(m: &mut Machine, list: u32, entry: u32, picture: u32, tpage: u16) {
        let w = i32::from(m.lbu(picture + 2));
        let counter = m.lh(entry + 2) as i32 as u32;
        let h = i32::from(m.lbu(picture + 3));
        let x = m.lh(entry + 4) as i32;
        let y = m.lh(entry + 6) as i32;
        let quad = next_quad(m, list);
        let width = w >> (counter & 31);
        let left = x.wrapping_add(w.wrapping_sub(width) >> 1);
        let height = h >> (counter & 31);
        let top = y.wrapping_add(h.wrapping_sub(height) >> 1);
        let code = m.lbu(quad + 7);
        let right = left.wrapping_add(width);
        let bottom = top.wrapping_add(height);
        m.sh(quad + 0xa, top as u16);
        m.sh(quad + 0x12, top as u16);
        m.sb(quad + 4, 0x80);
        m.sb(quad + 5, 0x80);
        m.sb(quad + 6, 0x80);
        m.sh(quad + 8, left as u16);
        m.sh(quad + 0x10, right as u16);
        m.sh(quad + 0x18, left as u16);
        m.sh(quad + 0x1a, bottom as u16);
        m.sh(quad + 0x20, right as u16);
        m.sh(quad + 0x22, bottom as u16);
        m.sb(quad + 7, code | 2);
        quad_uv(m, quad, picture);
        m.sh(quad + 0xe, tpage);
        let list = m.gw(LIST);
        m.sh(quad + 0x16, 0x56);
        let count = m.lw(list + 0x108);
        m.sw(list + 0x108, count.wrapping_add(1));
        step(m, entry);
        if width == 0 && height == 0 {
            m.sb(entry, 0);
            m.sh(entry + 2, 0);
        }
    }

    /// 8004CE9C: clear the list (count 0, `+0x11C` = 0xFF).
    pub fn clear_4b74c(m: &mut Machine) {
        let list = m.gw(LIST);
        m.sw(list + 0x110, 0);
        m.sw(list + 0x11c, 0xff);
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, _args: [u32; 4]) -> Option<u32> {
        match target {
            0x8003_37bc => overlay_33524(m),
            0x8004_c998 => popups_4b248(m),
            0x8004_c824 => begin_4b0d4(m),
            0x8004_c86c => link_4b11c(m),
            0x8004_ca28 => build_4b2d8(m),
            0x8004_ce9c => clear_4b74c(m),
            _ => return None,
        }
        Some(0)
    }
}

pub mod stream {
    //! 800337DC -> 8002D3F4: the stream status overlay. 8002C980 fetches the
    //! current word of the stream channel selected by `[gp+0x38]` (a
    //! bit-packed position: 7 bits at 25, 18, 11, 6 bits at 5, 3 bits at 2,
    //! unpacked into small-data bytes), 8002CD24 tracks the three bytes at
    //! `gp+0x320` against the block `[gp+0x314]`, and depending on the block
    //! mode one of the three drawing routines of [`super::stream_draw`]
    //! appends primitives.
    use super::*;

    /// `[gp+0x314]`: overlay block (+0 mode, +4..+6 shown digits, +7..+9
    /// last digits, +0xA.. per-digit hold counters).
    const BLOCK: u32 = 0x314;
    /// 800B03AC: 20-byte records per block mode (+0xE: hold time / 30).
    const MODES: u32 = 0x800b_03ac;
    /// Context flag 0x20000000: frame step paused.
    const PAUSED: u32 = 0x2000_0000;

    /// 800337DC: run 8002D3F4 on the gameplay ordering table and primitive
    /// pointer and store the returned primitive pointer.
    pub fn overlay_33544(m: &mut Machine) {
        // The original 0x18-byte frame keeps 8002D3F4's frame at its
        // original address.
        const FRAME: u32 = 0x18;
        m.push_frame(FRAME);
        let gameplay = gameplay(m);
        let ot = m.lw(gameplay + 0x10);
        let primitives = m.lw(gameplay + 0x14);
        let next = overlay_2cfcc(m, ot, primitives);
        let gameplay = super::gameplay(m);
        m.sw(gameplay + 0x14, next);
        m.pop_frame(FRAME);
    }

    // JP-REVIEW(R0104): near function: PAL 8002CFCC -> JP 8002D3F4, imm:1, 1 instructions differ (whole routine)
    /// 8002D3F4 (`ot`, `primitives`) -> next free primitive. Unpacks a new
    /// stream word into `gp+0x320..0x322/0x31A/0x31B`, updates the digits,
    /// then, when the block mode and `[context+0x3A0]`/`gp+0x318` allow,
    /// draws through 8002D330 (mode 9), 8002D10C (mode 10) or 8002CE70.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud::overlay_2cfcc` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn overlay_2cfcc(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
        crate::jp::res_p3_hud::overlay_2cfcc(m, ot, primitives)
    }

    /// 8002C980 (`word`, `value`) -> 1 when the stream channel (`[gp+0x48]`
    /// +0 < +4, +0x148 clear) has a current entry with a non-zero first
    /// halfword: stores its word +4 at `word` and its signed halfword +2 at
    /// `value` (each when non-null); 0 otherwise.
    pub fn stream_word_2c558(m: &mut Machine, word: u32, value: u32) -> u32 {
        let channel = m.gw(0x48);
        let head = m.lw(channel) as i32;
        let tail = m.lw(channel + 4) as i32;
        if head >= tail {
            return 0;
        }
        if m.lw(channel + 0x148) != 0 {
            return 0;
        }
        let entry = entry_2b188(m);
        if entry == 0 || m.lhu(entry) == 0 {
            return 0;
        }
        if word != 0 {
            let data = m.lw(entry + 4);
            m.sw(word, data);
        }
        if value != 0 {
            let half = m.lh(entry + 2) as i32 as u32;
            m.sw(value, half);
        }
        1
    }

    /// 8002B5B0 -> the 16-byte entry (`[gp+0x3C]` + 4 + 16 * n) of channel n
    /// = `[gp+0x38]` byte +0x1D when n < 32 and bit n of `[[gp+0x3C]]` is
    /// set; 0 otherwise.
    pub fn entry_2b188(m: &mut Machine) -> u32 {
        let state = m.gw(0x38);
        let channel = u32::from(m.lbu(state + 0x1d));
        if channel >= 0x20 {
            return 0;
        }
        let table = m.gw(0x3c);
        if m.lw(table) & (1 << channel) == 0 {
            return 0;
        }
        table.wrapping_add((channel << 4) + 4)
    }

    /// 8002CD24: for each of the three bytes at `gp+0x320`: a change is
    /// copied to block +7+i and +4+i with its hold counter (+0xA+2i) reset;
    /// otherwise the counter advances (unless paused) until the mode's hold
    /// time (record +0xE * 30). When all three are held the shown digits
    /// +4..+6 are cleared; when the first changed, +5/+6 are cleared.
    pub fn digits_2c8fc(m: &mut Machine) {
        let mut held: u32 = 0;
        for i in 0..3u32 {
            let current_at = crate::GP + 0x320 + i;
            let block = m.gw(BLOCK);
            let current = m.lbu(current_at);
            let last = block + i;
            if current != m.lbu(last + 7) {
                m.sb(last + 7, current);
                let block = m.gw(BLOCK);
                let current = m.lbu(current_at);
                m.sb(block + i + 4, current);
                let block = m.gw(BLOCK);
                held = u32::from(i < 1) << 7;
                m.sh(block + 2 * i + 0xa, 0);
                continue;
            }
            let mode = m.lw(block);
            let counter_at = block + 2 * i;
            let record = mode.wrapping_mul(20).wrapping_add(MODES);
            let hold = u32::from(m.lbu(record + 0xe)).wrapping_mul(30) as i32;
            let counter = m.lhu(counter_at + 0xa);
            if i32::from(m.lh(counter_at + 0xa)) < hold {
                let context = context(m);
                if !m.lw(context) & PAUSED != 0 {
                    m.sh(counter_at + 0xa, counter.wrapping_add(1));
                }
            } else {
                held = held.wrapping_add(1);
            }
        }
        let first = if held & 0xff == 3 {
            0
        } else if held & 0x80 != 0 {
            1
        } else {
            return;
        };
        for i in first..3 {
            let block = m.gw(BLOCK);
            m.sb(block + i + 4, 0);
        }
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, ..] = args;
        Some(match target {
            0x8003_37dc => {
                overlay_33544(m);
                0
            }
            0x8002_d3f4 => overlay_2cfcc(m, a0, a1),
            0x8002_c980 => stream_word_2c558(m, a0, a1),
            0x8002_b5b0 => entry_2b188(m),
            0x8002_cd24 => {
                digits_2c8fc(m);
                0
            }
            _ => return None,
        })
    }
}

pub mod tree_5cbf4 {
    //! 8005E31C: the instrument panel (heading digits and compass lines,
    //! altitude digits and ladder, timer, damage percentage, missile count
    //! bars and a framed block). 8005E31C copies the values from the
    //! 80033814 view block into a 0x2C-byte panel block in its stack frame
    //! and 8005E178 runs one drawer per panel flag bit. Each drawer fills the
    //! prebuilt packet chain of this buffer (`[[gp+0x4EC] + 4 * buffer]`)
    //! and links it into ordering-table slot `ot` (`[gameplay+0x10]`).
    //!
    //! Panel block (8005E31C frame +0x10): +0x00 flags (0x7F); +0x04
    //! altitude word; +0x08 heading; +0x0C damage (0..0x1000); +0x10 count;
    //! +0x14 capacity; +0x18 reserve; +0x1C..+0x1E and +0x20..+0x22 two RGB
    //! colors; +0x24 glyph color; +0x26 timer mode; +0x28 timer value.
    use super::*;
    use crate::sdk;

    /// 800104AC: 16-bit sine table (0x1000 per quarter turn, cosine at
    /// +0x1000 entries).
    const SINE: u32 = 0x8001_04ac;
    /// 800C6D60: HUD state block (colors at +8..+0xD, +0x11, +4, word +0).
    const STATE: u32 = 0x800c_6d60;
    /// `[gp+0x4EC]`: per-buffer panel packet chains.
    const CHAINS: u32 = 0x4ec;

    /// Link the packets `first..=last` (already chained) into `slot`: the
    /// last tag takes the slot's link, the slot then points at `first`.
    fn link_chain(m: &mut Machine, slot: u32, first: u32, last: u32) {
        let tag = m.lw(last);
        let link = m.lw(slot);
        m.sw(last, (tag & LENGTH) | (link & LINK));
        let link = m.lw(slot);
        m.sw(slot, (link & LENGTH) | (first & LINK));
    }

    /// Copy the RGB bytes at `panel + from` into `prim + 4..`.
    fn color(m: &mut Machine, prim: u32, panel: u32, from: u32) {
        for i in 0..3 {
            let value = m.lbu(panel + from + i);
            m.sb(prim + 4 + i, value);
        }
    }

    /// 8005E31C (`view`): build the panel block from the view block and the
    /// 800C6D60 state, then draw it.
    pub fn panel_5cbf4(m: &mut Machine, view: u32) {
        const FRAME: u32 = 0x48;
        let frame = m.push_frame(FRAME);
        let panel = frame + 0x10;
        m.sw(panel, 0x7f);
        let entity = m.lw(view + 4);
        let base = m.gw(0x33c);
        let height = m.lw(entity + 4);
        let offset = m.lw(base + 4);
        m.sw(
            panel + 4,
            height.wrapping_add(offset).wrapping_add(0x0004_2000),
        );
        for (to, from) in [(8, 0x14), (0xc, 0x18), (0x10, 0x1c), (0x14, 0x24)] {
            let value = m.lw(view + from);
            m.sw(panel + to, value);
        }
        let timer_mode = m.lhu(STATE + 4);
        let reserve = m.lw(view + 0x28);
        let ticks = m.lw(STATE);
        let glyph = m.lbu(STATE + 0x11);
        let bytes: [u8; 6] = std::array::from_fn(|i| m.lbu(STATE + 8 + i as u32));
        m.sh(panel + 0x26, timer_mode);
        m.sw(panel + 0x28, ticks);
        m.sh(panel + 0x24, u16::from(glyph));
        m.sb(panel + 0x1c, bytes[0]);
        m.sb(panel + 0x1d, bytes[1]);
        m.sb(panel + 0x1e, bytes[2]);
        m.sb(panel + 0x20, bytes[3]);
        m.sb(panel + 0x21, bytes[4]);
        m.sw(panel + 0x18, reserve);
        m.sb(panel + 0x22, bytes[5]);
        let context = context(m);
        let chains = m.gw(CHAINS);
        let gameplay = gameplay(m);
        let buffer = m.lw(context + 4);
        let ot = m.lw(gameplay + 0x10);
        let chain = m.lw(chains.wrapping_add(buffer << 2));
        draw_5ca50(m, panel, ot, chain);
        m.pop_frame(FRAME);
    }

    /// 8005E178 (`panel`, `ot`, `chain`): run the drawers selected by the
    /// panel flags on their packet ranges, then link the chain head.
    pub fn draw_5ca50(m: &mut Machine, panel: u32, ot: u32, chain: u32) {
        // The original 0x20-byte frame keeps 8005CB48's frame at its
        // original address.
        const FRAME: u32 = 0x20;
        m.push_frame(FRAME);
        if m.lw(panel) & 1 != 0 {
            heading_5af88(m, panel, ot, chain + 8);
        }
        if m.lw(panel) & 2 != 0 {
            altitude_5b450(m, panel, ot, chain + 0x9c);
        }
        if m.lw(panel) & 4 != 0 {
            timer_5b908(m, panel, ot, chain + 0x1b8);
        }
        if m.lw(panel) & 0x10 != 0 {
            damage_5bd40(m, panel, ot, chain + 0x30c);
        }
        if m.lw(panel) & 8 != 0 {
            missiles_5c270(m, panel, ot, chain + 0x244);
        }
        if m.lw(panel) & 0x40 != 0 {
            frame_5c718(m, panel, ot, chain + 0x384);
        }
        if m.lw(panel) & 0x80 != 0 {
            super::extras::dim_frame_5c8f0(m, panel, ot, chain + 0x3dc);
        }
        add_prim(m, ot, chain);
        m.pop_frame(FRAME);
    }

    // JP-REVIEW(R0108): near function: PAL 8005AF88 -> JP 8005C680, imm:6, 6 instructions differ (whole routine)
    /// 8005C680: heading. Up to four digits of `([panel+8] * 28) >> 12`
    /// right to left from x = 0x40 (sprites from `block + 0x14`), then two
    /// four-point compass lines (`block + 0x64`, `block + 0x7C`) rotated by
    /// the heading.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud::heading_5af88` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn heading_5af88(m: &mut Machine, panel: u32, ot: u32, block: u32) {
        crate::jp::res_p3_hud::heading_5af88(m, panel, ot, block)
    }

    // JP-REVIEW(R0109): near function: PAL 8005B450 -> JP 8005CB48, imm:6, 6 instructions differ (whole routine)
    /// 8005CB48: altitude. `[panel+4] >> 12` scaled by -2/3, clamped to
    /// -9999..99999: a minus sign (`block + 0xF0`), up to five digits from
    /// x = 0x11B (`block + 0x8C`), a ladder of five ticks offset by the tens
    /// digit, and two drawing-area packets (0x104, 0x110) clipping it.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud::altitude_5b450` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn altitude_5b450(m: &mut Machine, panel: u32, ot: u32, block: u32) {
        crate::jp::res_p3_hud::altitude_5b450(m, panel, ot, block)
    }

    // JP-REVIEW(R0110): changed function: PAL 8005B908 -> JP 8005D000, del:4 imm:10 ins:16 op:9 reg:16, 55 instructions differ (whole routine)
    /// 8005D000: mission timer `mm:ss` from `[panel+0x28]` (in 1/25 s);
    /// in timer mode 1 below 1500 it shows `ss:ff` in red, and its separator
    /// blinks.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud::timer_5b908` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn timer_5b908(m: &mut Machine, panel: u32, ot: u32, block: u32) {
        crate::jp::res_p3_hud::timer_5b908(m, panel, ot, block)
    }

    // JP-REVIEW(R0111): near function: PAL 8005BD40 -> JP 8005D468, del:28 imm:4 ins:28 op:4 reg:3, 67 instructions differ (whole routine)
    /// 8005D468: damage. `[panel+0xC]` (0..0x1000) as a percentage: the
    /// gauge color turns from green to red (darker when low and blinking),
    /// the gauge texture page steps with damage, and `100 - percent` is
    /// printed with up to three digits.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud::damage_5bd40` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn damage_5bd40(m: &mut Machine, panel: u32, ot: u32, block: u32) {
        crate::jp::res_p3_hud::damage_5bd40(m, panel, ot, block)
    }

    // JP-REVIEW(R0112): near function: PAL 8005C270 -> JP 8005D998, imm:3, 3 instructions differ (whole routine)
    /// 8005D998: missiles. One bar per capacity slot (`[panel+0x14]`),
    /// dimmed below `[panel+0x18]` and below `capacity - count`, then the
    /// count (`[panel+0x10]`) with up to three digits.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud::missiles_5c270` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn missiles_5c270(m: &mut Machine, panel: u32, ot: u32, block: u32) {
        crate::jp::res_p3_hud::missiles_5c270(m, panel, ot, block)
    }

    /// 8005DE40: the panel frame: two packets in the first color, three in
    /// half the second color, linked as one chain ending at `block + 0x48`.
    pub fn frame_5c718(m: &mut Machine, panel: u32, ot: u32, block: u32) {
        for i in 0..2 {
            color(m, block + i * 0x14, panel, 0x20);
        }
        for i in 0..3 {
            let at = block + 0x2c + i * 0x10;
            for c in 0..3 {
                let value = m.lbu(panel + 0x1c + c) >> 1;
                m.sb(at + c, value);
            }
        }
        link_chain(m, ot, block, block + 0x48);
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        match target {
            0x8005_e31c => panel_5cbf4(m, a0),
            0x8005_e178 => draw_5ca50(m, a0, a1, a2),
            0x8005_c680 => heading_5af88(m, a0, a1, a2),
            0x8005_cb48 => altitude_5b450(m, a0, a1, a2),
            0x8005_d000 => timer_5b908(m, a0, a1, a2),
            0x8005_d468 => damage_5bd40(m, a0, a1, a2),
            0x8005_d998 => missiles_5c270(m, a0, a1, a2),
            0x8005_de40 => frame_5c718(m, a0, a1, a2),
            _ => return None,
        }
        Some(0)
    }
}

pub mod tree_3e828 {
    //! 8003EFE0: target markers. For each of the 64 entities with a screen
    //! record, 8003F520 draws its box (corner brackets or a full box), its
    //! distance digits, a type icon and a status label; 80040E70 draws the
    //! lock-on reticle; 8003F054 links the four primitive groups of this
    //! buffer's marker block (`[gp+0x448]`, counts in `[gp+0x444]`):
    //! 0x10-byte lines from +8 (count +8), 0x18-byte three-point lines from
    //! +0x408 (+0xC), sprites from +0x1008 (+0x10) and glyphs from +0x1508
    //! (+0x14).
    use super::*;

    /// `[gp+0x444]`: marker list (+0/+4 per-buffer blocks, +8..+0x14 counts).
    const LIST: u32 = 0x444;
    /// `[gp+0x448]`: this frame's marker block.
    const BLOCK: u32 = 0x448;
    /// 800C67A0: per-entity screen records (+0 kind, +2 shape, +3 label,
    /// +5 status, +6 half size, +8 x, +A y).
    const RECORDS: u32 = 0x800c_67a0;
    /// 800C6ED0: per-entity distance records (+0 flags, +4 distance).
    const DISTANCES: u32 = 0x800c_6ed0;
    /// 800C6D60: HUD state word (bit 1: blink phase).
    const STATE: u32 = 0x800c_6d60;
    /// 800104AC: 16-bit sine table (cosine at +0x1000 entries).
    const SINE: u32 = 0x8001_04ac;
    /// 800B10DC: label sprite rectangles (u, v, w, h) of 8004084C.
    const LABELS: u32 = 0x800b_10dc;
    /// 800B10CC: status sprite rectangles (u, v, w, h) of 80040740.
    const STATUSES: u32 = 0x800b_10cc;

    /// Next element of the group with count word `list + count_at`,
    /// starting at `block + base` with `stride` bytes per element.
    fn next(m: &mut Machine, count_at: u32, base: u32, stride: u32) -> u32 {
        let list = m.gw(LIST);
        let count = m.lw(list + count_at);
        let block = m.gw(BLOCK);
        block.wrapping_add(count.wrapping_mul(stride).wrapping_add(base))
    }

    /// Increment the count word `list + count_at`.
    fn count(m: &mut Machine, count_at: u32) {
        let list = m.gw(LIST);
        let value = m.lw(list + count_at);
        m.sw(list + count_at, value.wrapping_add(1));
    }

    /// RGB bytes of a packet.
    fn rgb(m: &mut Machine, prim: u32, [r, g, b]: [u8; 3]) {
        m.sb(prim + 4, r);
        m.sb(prim + 5, g);
        m.sb(prim + 6, b);
    }

    /// 8003EFE0: draw the target markers.
    pub fn markers_3e828(m: &mut Machine) {
        begin_3e860(m);
        targets_3ed64(m);
        reticle_4049c(m);
        link_3e89c(m);
    }

    /// 8003F018: clear the four counts and select this buffer's block.
    pub fn begin_3e860(m: &mut Machine) {
        let list = m.gw(LIST);
        let context = context(m);
        m.sw(list + 0x14, 0);
        m.sw(list + 0x10, 0);
        m.sw(list + 0xc, 0);
        m.sw(list + 8, 0);
        let buffer = m.lw(context + 4);
        let block = m.lw(list.wrapping_add(buffer << 2));
        m.set_gw(BLOCK, block);
    }

    /// 8003F054: link the four groups and the block head (+0) into
    /// ordering-table slot `[gameplay+0x10]`.
    pub fn link_3e89c(m: &mut Machine) {
        let gameplay = gameplay(m);
        let slot = m.lw(gameplay + 0x10);
        for (count_at, base, stride) in [
            (8u32, 8u32, 0x10u32),
            (0xc, 0x408, 0x18),
            (0x10, 0x1008, 0x14),
            (0x14, 0x1508, 0x14),
        ] {
            let list = m.gw(LIST);
            if (m.lw(list + count_at) as i32) <= 0 {
                continue;
            }
            let block = m.gw(BLOCK);
            let mut index = 0i32;
            let mut prim = block.wrapping_add(base);
            loop {
                index += 1;
                add_prim(m, slot, prim);
                prim = prim.wrapping_add(stride);
                if index >= m.lw(list + count_at) as i32 {
                    break;
                }
            }
        }
        let block = m.gw(BLOCK);
        add_prim(m, slot, block);
    }

    // JP-REVIEW(R0113): changed function: PAL 8003ED64 -> JP 8003F520, call+:1 del:9 imm:44 ins:23 op:3 reg:10, 89 instructions differ (whole routine)
    /// 8003F520: per-entity markers. `color` (the box color word) carries
    /// over from the previous entity when the record kind is not 1..3.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_marks::targets_3ed64` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn targets_3ed64(m: &mut Machine) {
        crate::jp::res_p3_hud_marks::targets_3ed64(m)
    }

    // JP-REVIEW(R0115): near function: PAL 8003F28C -> JP 8003FA80, imm:1 ins:2 op:1, 4 instructions differ (whole routine)
    /// 8003FA80 (`index`, `color`): the box of entity `index` as four
    /// corner brackets, or a full box when its distance record flag 0x100
    /// is set: three-point lines with code word `color | 0x4A000000`.
    pub fn box_3f28c(m: &mut Machine, index: u32, color: u32) {
        let word = color | 0x4a00_0000;
        let record = m.lw(RECORDS + index * 4);
        if record == 0 {
            return;
        }
        let distance = m.lw(DISTANCES + index * 4);
        if distance == 0 {
            return;
        }
        let size_bits = u32::from(m.lhu(record + 6)) << 16;
        let size = (size_bits as i32 >> 16) as u32;
        let x = m.lh(record + 8) as i32 as u32;
        let left = x.wrapping_sub(size) as u16;
        let x = m.lh(record + 8) as i32 as u32;
        let half = m.lh(record + 6) as i32 as u32;
        let right = x.wrapping_add(half) as u16;
        let y = m.lh(record + 0xa) as i32 as u32;
        let half = m.lh(record + 6) as i32 as u32;
        let top = y.wrapping_sub(half) as u16;
        let y = m.lh(record + 0xa) as i32 as u32;
        let half = m.lh(record + 6) as i32 as u32;
        let bottom = y.wrapping_add(half) as u16;
        let quarter = ((size_bits as i32) >> 18) as u32;
        let full = m.lw(distance) & 0x100 != 0;
        let list = m.gw(LIST);
        let mut line = next(m, 0xc, 0x408, 0x18);
        if full {
            // Two three-point lines: left/top..right..bottom and back.
            m.sw(line + 4, word);
            m.sh(line + 0xc, left);
            m.sh(line + 8, left);
            m.sh(line + 0x10, right);
            m.sh(line + 0xa, bottom);
            m.sh(line + 0x12, top);
            m.sh(line + 0xe, top);
            count(m, 0xc);
            line += 0x18;
            m.sw(line + 4, word);
            m.sh(line + 0xc, right);
            m.sh(line + 8, right);
            m.sh(line + 0x10, left);
            m.sh(line + 0xa, top);
            m.sh(line + 0x12, bottom);
            m.sh(line + 0xe, bottom);
        } else {
            // Four corner brackets: (corner x, corner y, x step, y step).
            let corners = [
                (left, top, quarter, quarter),
                (right, top, quarter.wrapping_neg(), quarter),
                (left, bottom, quarter, quarter.wrapping_neg()),
                (
                    right,
                    bottom,
                    quarter.wrapping_neg(),
                    quarter.wrapping_neg(),
                ),
            ];
            for (i, (cx, cy, dx, dy)) in corners.into_iter().enumerate() {
                if i > 0 {
                    count(m, 0xc);
                    line += 0x18;
                }
                m.sw(line + 4, word);
                m.sh(line + 0x10, cx);
                m.sh(line + 0xc, cx);
                m.sh(line + 8, u32::from(cx).wrapping_add(dx) as u16);
                m.sh(line + 0x12, cy);
                m.sh(line + 0xe, cy);
                m.sh(line + 0xa, cy);
                m.sh(line + 0x12, u32::from(cy).wrapping_add(dy) as u16);
            }
        }
        let value = m.lw(list + 0xc);
        m.sw(list + 0xc, value.wrapping_add(1));
    }

    /// 80040530 (`index`) -> 1 without a screen record, else 0: a heading
    /// arrow (two green lines through three rotated points) above the box,
    /// rotated by the entity yaw relative to the player's.
    pub fn arrow_3fd24(m: &mut Machine, index: u32) -> u32 {
        let record = m.lw(RECORDS + index * 4);
        if record == 0 {
            return 1;
        }
        let entities = entities(m);
        let gameplay = gameplay(m);
        let size_bits = u32::from(m.lhu(record + 6)) << 16;
        let entity = m.lw(entities.wrapping_add(index * 4));
        let player = m.lw(gameplay + 0x28);
        let yaw = m.lhu(entity + 0x1a);
        let own = m.lhu(player + 0x16);
        let angle = u32::from(yaw).wrapping_sub(u32::from(own)) & 0x3fff;
        let sin = i32::from(m.lh(SINE + angle * 2));
        let cos = i32::from(m.lh(SINE + (angle + 0x1000) * 2));
        let half = (size_bits as i32) >> 17;
        let shift_x = cos.wrapping_sub(sin).wrapping_mul(half) >> 14;
        let shift_y = sin.wrapping_add(cos).wrapping_mul(half) >> 14;
        let size = m.lhu(record + 6);
        let mut points: [u16; 6] = [
            0,
            size.wrapping_sub(4),
            4,
            m.lhu(record + 6).wrapping_add(4),
            0xfffc,
            m.lhu(record + 6).wrapping_add(4),
        ];
        for point in points.chunks_exact_mut(2) {
            let x = i32::from(point[0] as i16);
            let y = i32::from(point[1] as i16);
            let rx = cos.wrapping_mul(x).wrapping_sub(sin.wrapping_mul(y)) >> 14;
            let px = i32::from(m.lhu(record + 8))
                .wrapping_add(rx)
                .wrapping_add(shift_x);
            point[0] = px as u16;
            let ry = sin.wrapping_mul(x).wrapping_add(cos.wrapping_mul(y)) >> 14;
            let py = i32::from(m.lhu(record + 0xa))
                .wrapping_add(ry)
                .wrapping_add(shift_y);
            point[1] = py as u16;
        }
        for pair in [[0usize, 1, 2, 3], [0, 1, 4, 5]] {
            let line = next(m, 8, 8, 0x10);
            rgb(m, line, [0, 0xff, 0]);
            for (i, &at) in pair.iter().enumerate() {
                m.sh(line + 8 + 2 * i as u32, points[at]);
            }
            count(m, 8);
        }
        0
    }

    /// 80040740 (`status`, `x`, `y`): status sprite `status` (1..; 0 draws
    /// nothing) from the 800B10CC rectangles, right-aligned at x - 2;
    /// statuses 1, 2, 3 are red, yellow and blue.
    pub fn status_3ff34(m: &mut Machine, status: u32, x: u32, y: u32) {
        let status = status & 0xff;
        if status == 0 {
            return;
        }
        let rect = STATUSES + status * 4;
        let sprite = next(m, 0x10, 0x1008, 0x14);
        let width = u32::from(m.lbu(rect + 2));
        m.sh(sprite + 0xa, y as u16);
        m.sh(sprite + 8, x.wrapping_sub(width).wrapping_sub(2) as u16);
        sprite_rect(m, sprite, rect);
        match status {
            1 => rgb(m, sprite, [0x83, 0, 0]),
            2 => rgb(m, sprite, [0x83, 0x83, 0]),
            3 => {
                m.sb(sprite + 5, 0x23);
                m.sb(sprite + 4, 0);
                m.sb(sprite + 6, 0x83);
            }
            _ => {}
        }
        count(m, 0x10);
    }

    /// u, v, w, h of a sprite from a 4-byte rectangle.
    fn sprite_rect(m: &mut Machine, sprite: u32, rect: u32) {
        let u = m.lbu(rect);
        m.sb(sprite + 0xc, u);
        let v = m.lbu(rect + 1);
        m.sb(sprite + 0xd, v);
        let w = m.lbu(rect + 2);
        m.sh(sprite + 0x10, u16::from(w));
        let h = m.lbu(rect + 3);
        m.sh(sprite + 0x12, u16::from(h));
    }

    /// 8004084C (`label`, `x`, `y`): green label sprite `label` from the
    /// 800B10DC rectangles (skipped when its width is 0), right-aligned at
    /// x - 2.
    pub fn label_40040(m: &mut Machine, label: u32, x: u32, y: u32) {
        let rect = (label << 2).wrapping_add(LABELS);
        if m.lbu(rect + 2) == 0 {
            return;
        }
        let sprite = next(m, 0x10, 0x1008, 0x14);
        rgb(m, sprite, [0, 0x83, 0]);
        let width = u32::from(m.lbu(rect + 2));
        m.sh(sprite + 0xa, y as u16);
        m.sh(sprite + 8, x.wrapping_sub(width).wrapping_sub(2) as u16);
        sprite_rect(m, sprite, rect);
        count(m, 0x10);
    }

    // JP-REVIEW(R0116): near function: PAL 8004049C -> JP 80040E70, imm:1 ins:1 op:1, 3 instructions differ (whole routine)
    /// 80040E70: lock-on reticle at (`[gp+0x358]`, `[gp+0x35A]`) of size
    /// `[gp+0x356]` when `gp+0x354` is set: a red diamond (two three-point
    /// lines) while the timer byte `gp+0x355` is below 32, then a crosshair
    /// of eight red lines.
    pub fn reticle_4049c(m: &mut Machine) {
        let gp = crate::GP;
        if m.lbu(gp + 0x354) == 0 {
            return;
        }
        let size_bits = u32::from(m.lhu(gp + 0x356)) << 16;
        let x = i32::from(m.lh(gp + 0x358));
        let y = i32::from(m.lh(gp + 0x35a));
        let size = (size_bits as i32) >> 16;
        if m.lbu(gp + 0x355) < 0x20 {
            let reach = size.wrapping_add((size_bits as i32) >> 19);
            for dx in [reach, reach.wrapping_neg()] {
                let line = next(m, 0xc, 0x408, 0x18);
                rgb(m, line, [0xff, 0, 0]);
                m.sh(line + 8, x as u16);
                m.sh(line + 0xa, y.wrapping_sub(reach) as u16);
                m.sh(line + 0xc, x.wrapping_add(dx) as u16);
                m.sh(line + 0xe, y as u16);
                m.sh(line + 0x10, x as u16);
                m.sh(line + 0x12, y.wrapping_add(reach) as u16);
                count(m, 0xc);
            }
            return;
        }
        let quarter = (size_bits as i32) >> 18;
        let top = y.wrapping_sub(size);
        let inner_top = y.wrapping_sub(quarter);
        let bottom = y.wrapping_add(size);
        let inner_bottom = y.wrapping_add(quarter);
        let left = x.wrapping_sub(size);
        let inner_left = x.wrapping_sub(quarter);
        let right = x.wrapping_add(size);
        let inner_right = x.wrapping_add(quarter);
        // (x0, y0, x1, y1) of the eight lines, in the original order.
        let lines = [
            (x, top, x, inner_top),
            (x, bottom, x, inner_bottom),
            (left, y, inner_left, y),
            (right, y, inner_right, y),
            (inner_left, top, inner_right, top),
            (inner_left, bottom, inner_right, bottom),
            (left, inner_top, left, inner_bottom),
            (right, inner_top, right, inner_bottom),
        ];
        let mut line = next(m, 8, 8, 0x10);
        for (i, (x0, y0, x1, y1)) in lines.into_iter().enumerate() {
            if i > 0 {
                line += 0x10;
            }
            rgb(m, line, [0xff, 0, 0]);
            m.sh(line + 8, x0 as u16);
            m.sh(line + 0xa, y0 as u16);
            m.sh(line + 0xc, x1 as u16);
            m.sh(line + 0xe, y1 as u16);
            count(m, 8);
        }
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        Some(match target {
            0x8003_efe0 => {
                markers_3e828(m);
                0
            }
            0x8003_f018 => {
                begin_3e860(m);
                0
            }
            0x8003_f054 => {
                link_3e89c(m);
                0
            }
            0x8003_f520 => {
                targets_3ed64(m);
                0
            }
            0x8003_fa80 => {
                box_3f28c(m, a0, a1);
                0
            }
            0x8004_0530 => arrow_3fd24(m, a0),
            0x8004_0740 => {
                status_3ff34(m, a0, a1, a2);
                0
            }
            0x8004_084c => {
                label_40040(m, a0, a1, a2);
                0
            }
            0x8004_0e70 => {
                reticle_4049c(m);
                0
            }
            _ => return None,
        })
    }
}

pub mod tree_3bdf8_a {
    //! 8003C650..8003D5C0: the set-up and ordering-table half of the 8003C5B0
    //! HUD composition. 8003C650 picks this frame's HUD primitive area and
    //! clears its group counters; 8003D08C stores the radar basis (player
    //! position and heading) in the scratchpad and 8003D138 the radar
    //! distance of every entity; 8003D280 eases the radar range toward the
    //! nearest (or locked) target; 8003D458 adds two frame tiles and colors
    //! the range-scale sprites; 8003C690 and 8003CB90 link the filled
    //! primitive groups of the area into the gameplay ordering table.
    use super::*;
    use crate::sdk;

    /// `[gp+0x434]` (800BCFA8): the HUD primitive block. +0/+4 the primitive
    /// area of each display buffer; +8 glyph sprite count (area+0x18, 0x14
    /// bytes each); +0xC count of 0x10-byte packets at area+0x518; +0x10
    /// tile count (area+0x918, 0x10 bytes each); +0x14 count of 0x18-byte
    /// packets at area+0x998; +0x18 optional group flags (bit 0, bit 1).
    const HUD_BLOCK: u32 = 0x434;
    /// `[gp+0x438]` (800BCFAC): this frame's HUD primitive area.
    const HUD_AREA: u32 = 0x438;
    /// `[gp+0x43C]` (800BCFB0): pointer to the radar basis.
    const RADAR_BASIS_POINTER: u32 = 0x43c;
    /// 1F800100: radar basis in the scratchpad: +0 x and +4 z (position
    /// >> 12), +8 sin(yaw) / 4, +0xC cos(yaw) / 4, +0x10 yaw.
    const RADAR_BASIS: u32 = 0x1f80_0100;
    /// 800104AC: sine table (halfwords; index angle & 0x3FFF, +0x1000 for
    /// the cosine).
    const SINE_TABLE: u32 = 0x8001_04ac;
    /// 800C6FD0: radar flag word of each of the 64 entities (0x2000 on the
    /// radar, 0x100 marked as a target).
    const RADAR_FLAGS: u32 = 0x800c_6fd0;
    /// 800C6A60: radar record of each of the 64 entities (0xC bytes; +8 the
    /// distance, -1 when not on the radar).
    const RADAR_RECORDS: u32 = 0x800c_6a60;
    /// 800C6D60: HUD state: +8..+0xA frame tile color, +0xB..+0xD range-scale
    /// sprite color, +0xE locked target entity (0xFF none), +0x1C radar range.
    const HUD_STATE: u32 = 0x800c_6d60;
    /// Length byte of a two-word mode packet.
    const MODE_LENGTH: u32 = 0x0200_0000;
    /// Distance of an entity that is not on the radar.
    const NO_DISTANCE: u32 = 0xffff_ffff;

    /// libgpu `addPrims(slot, head, tail)` as compiled inline: the tail tag
    /// keeps its length and takes the slot's link, then the slot (reloaded)
    /// links to the head. Where the original reuses the slot word it just
    /// stored instead of reloading it, nothing is stored in between, so the
    /// reload reads the same word.
    fn add_prims(m: &mut Machine, slot: u32, head: u32, tail: u32) {
        let tag = m.lw(tail);
        let link = m.lw(slot);
        m.sw(tail, (tag & LENGTH) | (link & LINK));
        let link = m.lw(slot);
        m.sw(slot, (link & LENGTH) | (head & LINK));
    }

    /// The short insertion the original uses for its two-word mode packets:
    /// the tag becomes the slot word with length 2 ORed in, and the slot
    /// takes the packet address with a zero length byte.
    fn add_mode(m: &mut Machine, slot: u32, packet: u32) {
        let link = m.lw(slot);
        m.sw(packet, link | MODE_LENGTH);
        m.sw(slot, packet & LINK);
    }

    /// `add_prim` for `count` consecutive packets of `stride` bytes from
    /// `first` (a counted original loop, at least one iteration).
    fn add_run(m: &mut Machine, slot: u32, first: u32, stride: u32, count: u32) {
        for i in 0..count {
            add_prim(m, slot, first.wrapping_add(i.wrapping_mul(stride)));
        }
    }

    /// `add_prim` for the packets of a counted group (`do { .. } while (++i <
    /// [hud+field])`, the count reloaded after every insertion, signed).
    fn add_counted(m: &mut Machine, slot: u32, first: u32, stride: u32, hud: u32, field: u32) {
        let mut i: u32 = 0;
        loop {
            add_prim(m, slot, first.wrapping_add(i.wrapping_mul(stride)));
            i = i.wrapping_add(1);
            if (i as i32) >= (m.lw(hud.wrapping_add(field)) as i32) {
                break;
            }
        }
    }

    /// 8003C650: start a HUD frame: clear the five group counters/flags of
    /// the HUD block (+0x14, +0x10, +0xC, +8, +0x18) and select the primitive
    /// area of the current display buffer (`[context+4]`) as `[gp+0x438]`.
    pub fn begin_frame_3be98(m: &mut Machine) {
        let hud = m.gw(HUD_BLOCK);
        let context = context(m);
        for offset in [0x14, 0x10, 0xc, 8, 0x18] {
            m.sw(hud.wrapping_add(offset), 0);
        }
        let buffer = m.lw(context.wrapping_add(4));
        let area = m.lw(hud.wrapping_add(buffer << 2));
        m.set_gw(HUD_AREA, area);
    }

    /// 8003C690: link the HUD area's primitive groups into the first slot of
    /// the gameplay ordering table (`[gameplay+0x10]`): the optional groups
    /// of flags bit 1 (three packets at +0xC74) and bit 0 (two at +0xC54),
    /// the counted tiles, 0x18-byte packets and glyph sprites, and the fixed
    /// packets in between, each set closed by a mode packet.
    pub fn link_hud_3bed8(m: &mut Machine) {
        let hud = m.gw(HUD_BLOCK);
        let gameplay = gameplay(m);
        let flags = m.lw(hud.wrapping_add(0x18));
        let ot = m.lw(gameplay.wrapping_add(0x10));
        if flags & 2 != 0 {
            let area = m.gw(HUD_AREA);
            add_run(m, ot, area.wrapping_add(0xc74), 0x14, 3);
        }
        let hud = m.gw(HUD_BLOCK);
        if (m.lw(hud.wrapping_add(0x10)) as i32) > 0 {
            let area = m.gw(HUD_AREA);
            add_counted(m, ot, area.wrapping_add(0x918), 0x10, hud, 0x10);
        }
        let area = m.gw(HUD_AREA);
        add_prim(m, ot, area.wrapping_add(0xc0c));
        add_prim(m, ot, area.wrapping_add(0xc20));
        add_prim(m, ot, area.wrapping_add(0xc));
        add_mode(m, ot, area.wrapping_add(0xa88));
        let hud = m.gw(HUD_BLOCK);
        if m.lw(hud.wrapping_add(0x18)) & 1 != 0 {
            let area = m.gw(HUD_AREA);
            add_run(m, ot, area.wrapping_add(0xc54), 0x10, 2);
        }
        let hud = m.gw(HUD_BLOCK);
        let area = m.gw(HUD_AREA);
        if (m.lw(hud.wrapping_add(0x14)) as i32) > 0 {
            add_counted(m, ot, area.wrapping_add(0x998), 0x18, hud, 0x14);
        }
        let hud = m.gw(HUD_BLOCK);
        if (m.lw(hud.wrapping_add(8)) as i32) > 0 {
            let area = m.gw(HUD_AREA);
            add_counted(m, ot, area.wrapping_add(0x18), 0x14, hud, 8);
        }
        let area = m.gw(HUD_AREA);
        add_mode(m, ot, area.wrapping_add(0xa7c));
        let area = m.gw(HUD_AREA);
        add_prims(m, ot, area.wrapping_add(0xb6c), area.wrapping_add(0xbe4));
        add_prim(m, ot, area.wrapping_add(0xc44));
        add_mode(m, ot, area.wrapping_add(0xa70));
        let area = m.gw(HUD_AREA);
        add_prim(m, ot, area.wrapping_add(0xa94));
        add_prim(m, ot, area.wrapping_add(0xaa4));
        add_mode(m, ot, area.wrapping_add(0xa64));
        let area = m.gw(HUD_AREA);
        add_prims(m, ot, area.wrapping_add(0xacc), area.wrapping_add(0xb44));
        add_prim(m, ot, area.wrapping_add(0xc34));
        add_mode(m, ot, area.wrapping_add(0xa58));
        let area = m.gw(HUD_AREA);
        add_prim(m, ot, area);
        add_prim(m, ot, area.wrapping_add(0xa9c));
    }

    /// 8003CB90: link the second HUD layer (drawn after 8003E69C while
    /// `[context+0x128]` bit 0x8000 is set) into the first ordering-table
    /// slot: the packet at area+0xFFC when `[gameplay+0xC]` bit 3 is set,
    /// the counted 0x10-byte packets at +0x518, then fixed packet runs from
    /// +0xCD4 to +0x1078 closed by three mode packets.
    pub fn link_layer_3c3d8(m: &mut Machine) {
        let gameplay = gameplay(m);
        let flags = m.lw(gameplay.wrapping_add(0xc));
        let ot = m.lw(gameplay.wrapping_add(0x10));
        if flags & 8 != 0 {
            let area = m.gw(HUD_AREA);
            add_prim(m, ot, area.wrapping_add(0xffc));
        }
        let area = m.gw(HUD_AREA);
        let hud = m.gw(HUD_BLOCK);
        add_prim(m, ot, area.wrapping_add(0x1078));
        if (m.lw(hud.wrapping_add(0xc)) as i32) > 0 {
            add_counted(m, ot, area.wrapping_add(0x518), 0x10, hud, 0xc);
        }
        let area = m.gw(HUD_AREA);
        add_prim(m, ot, area.wrapping_add(0x1070));
        add_run(m, ot, area.wrapping_add(0xfcc), 0x18, 2);
        let area = m.gw(HUD_AREA);
        add_run(m, ot, area.wrapping_add(0x1010), 0x14, 4);
        let area = m.gw(HUD_AREA);
        add_prim(m, ot, area.wrapping_add(0x1068));
        add_run(m, ot, area.wrapping_add(0xf2c), 0x10, 10);
        let area = m.gw(HUD_AREA);
        add_run(m, ot, area.wrapping_add(0xecc), 0x18, 4);
        let area = m.gw(HUD_AREA);
        add_run(m, ot, area.wrapping_add(0xcd4), 0x28, 3);
        let area = m.gw(HUD_AREA);
        add_mode(m, ot, area.wrapping_add(0xcc8));
        let area = m.gw(HUD_AREA);
        add_run(m, ot, area.wrapping_add(0xd4c), 0x28, 4);
        let area = m.gw(HUD_AREA);
        add_mode(m, ot, area.wrapping_add(0xcbc));
        let area = m.gw(HUD_AREA);
        add_run(m, ot, area.wrapping_add(0xdec), 0x28, 4);
        let area = m.gw(HUD_AREA);
        add_run(m, ot, area.wrapping_add(0xe8c), 0x10, 4);
        let area = m.gw(HUD_AREA);
        add_mode(m, ot, area.wrapping_add(0xcb0));
    }

    /// Sine table entry `index` (14-bit wrapped angle plus offset) as the
    /// original reads it: `lhu`, then `(x << 16) >> 18` (signed, / 4).
    fn quarter_sine(m: &mut Machine, index: u32) -> u32 {
        let value = m.lhu(SINE_TABLE.wrapping_add(index << 1));
        (i32::from(value as i16) >> 2) as u32
    }

    /// 8003D08C: store the radar basis at 1F800100 from the view block
    /// `view`: the position (`[view+4]`) x and z words >> 12, a quarter of
    /// the sine and cosine of the yaw halfword (`[view+8]`+2) and the yaw
    /// itself; `[gp+0x43C]` points to it.
    pub fn radar_basis_3c8d4(m: &mut Machine, view: u32) {
        let position = m.lw(view.wrapping_add(4));
        let x = m.lw(position);
        m.sw(RADAR_BASIS, ((x as i32) >> 12) as u32);
        let position = m.lw(view.wrapping_add(4));
        let z = m.lw(position.wrapping_add(8));
        m.sw(RADAR_BASIS + 4, ((z as i32) >> 12) as u32);
        let angles = m.lw(view.wrapping_add(8));
        let yaw = u32::from(m.lhu(angles.wrapping_add(2)));
        let sine = quarter_sine(m, yaw & 0x3fff);
        m.sw(RADAR_BASIS + 8, sine);
        let angles = m.lw(view.wrapping_add(8));
        let yaw = u32::from(m.lhu(angles.wrapping_add(2)));
        let cosine = quarter_sine(m, (yaw & 0x3fff) + 0x1000);
        m.sw(RADAR_BASIS + 0xc, cosine);
        let angles = m.lw(view.wrapping_add(8));
        let yaw = m.lh(angles.wrapping_add(2));
        m.set_gw(RADAR_BASIS_POINTER, RADAR_BASIS);
        m.sw(RADAR_BASIS + 0x10, i32::from(yaw) as u32);
    }

    /// `|value| < 0x4000` as the original tests it (`subu` negation, signed
    /// compare, so -0x80000000 passes).
    fn within_near(value: i32) -> bool {
        let magnitude = if value >= 0 {
            value
        } else {
            value.wrapping_neg()
        };
        magnitude < 0x4000
    }

    /// Low word of `value * value` (`mult`, `mflo`).
    fn square(value: i32) -> u32 {
        value.wrapping_mul(value) as u32
    }

    /// 8003D138: radar distance of entities 2..63 into their records
    /// (800C6A60 + 0xC*i + 8): -1 unless the entity's radar flag 0x2000 is
    /// set and its own flag 0x40 is clear; otherwise the planar distance
    /// (x, z >> 12) to the radar basis by `SquareRoot0`, on coordinates
    /// divided by 16 (result times 16) when either offset reaches 0x4000.
    pub fn radar_distances_3c980(m: &mut Machine) {
        for index in 2..0x40u32 {
            let record = RADAR_RECORDS + index * 0xc;
            let entities = entities(m);
            let flags = m.lw(RADAR_FLAGS + index * 4);
            let entity = m.lw(entities.wrapping_add(index * 4));
            let distance = if flags & 0x2000 == 0 || m.lw(entity) & 0x40 != 0 {
                NO_DISTANCE
            } else {
                let basis = m.gw(RADAR_BASIS_POINTER);
                let x = (m.lw(entity.wrapping_add(8)) as i32) >> 12;
                let dx = x.wrapping_sub(m.lw(basis) as i32);
                let z = (m.lw(entity.wrapping_add(0x10)) as i32) >> 12;
                let dz = (m.lw(basis.wrapping_add(4)) as i32).wrapping_sub(z);
                if within_near(dx) && within_near(dz) {
                    sdk::square_root0(m, square(dx).wrapping_add(square(dz)))
                } else {
                    let sum = square(dx >> 4).wrapping_add(square(dz >> 4));
                    sdk::square_root0(m, sum) << 4
                }
            };
            m.sw(record + 8, distance);
        }
    }

    /// 8003D280: ease the radar range (`[800C6D7C]`) toward a target range.
    /// With `[context+0x128]` bit 0x8000 the target is 1.25 times the
    /// distance of the locked entity (`[800C6D6E]`, when marked 0x100 and on
    /// the radar) or else of the nearest marked entity below 0x40000; without
    /// it, the distance of the locked entity or else the nearest of entities
    /// 2..31 selects the step 0x4000 (< 0x3000), 0x8000 (< 0x6000) or
    /// 0x10000. The range becomes the mean of old and target, rounded odd
    /// values away from the old range, clamped to 0x2000..0x40000.
    pub fn radar_range_3cac8(m: &mut Machine) {
        let context = context(m);
        let mode = m.lw(context.wrapping_add(0x128)) & 0x8000;
        let locked = u32::from(m.lbu(HUD_STATE + 0xe));
        let mut target: i32 = 0x4_0000;
        if mode != 0 {
            let mut chosen = None;
            if locked != 0xff && m.lw(RADAR_FLAGS + locked * 4) & 0x100 != 0 {
                let distance = m.lw(RADAR_RECORDS + locked * 0xc + 8);
                if distance != NO_DISTANCE {
                    chosen = Some(distance as i32);
                }
            }
            if let Some(distance) = chosen {
                target = distance;
            } else {
                for index in 2..0x40u32 {
                    if m.lw(RADAR_FLAGS + index * 4) & 0x100 == 0 {
                        continue;
                    }
                    let distance = m.lw(RADAR_RECORDS + index * 0xc + 8);
                    if distance != NO_DISTANCE && (distance as i32) < target {
                        target = distance as i32;
                    }
                }
            }
            target = target.wrapping_add(target >> 2);
        } else {
            if locked != 0xff {
                // A locked entity off the radar keeps 0x40000 (step 0x10000).
                let distance = m.lw(RADAR_RECORDS + locked * 0xc + 8);
                if distance != NO_DISTANCE {
                    target = distance as i32;
                }
            } else {
                for index in 2..0x20u32 {
                    let distance = m.lw(RADAR_RECORDS + index * 0xc + 8);
                    if distance != NO_DISTANCE && (distance as i32) < target {
                        target = distance as i32;
                    }
                }
            }
            target = if target < 0x3000 {
                0x4000
            } else if target < 0x6000 {
                0x8000
            } else {
                0x1_0000
            };
        }
        let old = m.lw(HUD_STATE + 0x1c) as i32;
        let mut range = old.wrapping_add(target) >> 1;
        if range & 1 != 0 {
            range = if old < range { range + 1 } else { range - 1 };
        }
        let range = range.clamp(0x2000, 0x4_0000);
        m.sw(HUD_STATE + 0x1c, range as u32);
    }

    // JP-REVIEW(R0117): near function: PAL 8003CCA0 -> JP 8003D458, imm:3 ins:1 reg:1, 5 instructions differ (whole routine)
    /// 8003D458: append two frame tiles (`TILE`, color `[800C6D68..32]` / 2)
    /// at (0, 0x2B) 5x0x2B and (0x56, 0xC) 0x140x0xC to the HUD area's tile
    /// group, color the range-scale sprites at area+0xC0C (full) and +0xC20
    /// (half) from `[800C6D6B..35]`, and select the latter's u (0x18, 0xC, 0)
    /// by the radar range (<= 0x4000, <= 0x8000, larger).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_radar::frame_tiles_3cca0` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn frame_tiles_3cca0(m: &mut Machine) {
        crate::jp::res_p3_hud_radar::frame_tiles_3cca0(m)
    }

    /// Native targets of this group by original address (none reads a
    /// register argument except 8003D08C's `a0`; none returns a value).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        match target {
            0x8003_c650 => begin_frame_3be98(m),
            0x8003_c690 => link_hud_3bed8(m),
            0x8003_cb90 => link_layer_3c3d8(m),
            0x8003_d08c => radar_basis_3c8d4(m, args[0]),
            0x8003_d138 => radar_distances_3c980(m),
            0x8003_d280 => radar_range_3cac8(m),
            0x8003_d458 => frame_tiles_3cca0(m),
            _ => return None,
        }
        Some(0)
    }
}

pub mod tree_3bdf8_b {
    //! 8003D5C0..8003ECB8: the radar and map parts of the HUD composition
    //! 8003C5B0. 8003D5C0 maps the terrain texture under the rotating radar
    //! (one POLY_FT4); 8003D9C8 refreshes the 12-byte radar blip records of
    //! the entities (8003DA08) and, at short range, of the ground objects
    //! (8003DCF0); 8003DE5C turns those records into sprites (8003DFF8); and
    //! 8003E69C draws the 128x128 map window (seven textured tiles, the
    //! player arrow and the entity dots placed by 8003EC0C). None of these
    //! routines reads its incoming `a0`.
    use super::*;
    use crate::GP;

    /// 800C6D7C: radar range (`RADAR + 0x1C`) in view position units; it
    /// maps to the 32-pixel radar radius.
    const RADAR_RANGE: u32 = 0x800c_6d7c;
    /// 800104AC: sine halfwords, 0x1000 = 1.0, 0x4000 steps per turn; the
    /// cosine of step `a` is entry `a + 0x1000`.
    const SINE: u32 = 0x8001_04ac;
    /// `[gp+0x434]`: HUD primitive counters (+8 sprites, +0xC tiles used).
    const COUNTERS: u32 = 0x434;
    /// `[gp+0x438]`: HUD primitive block.
    const PRIMITIVES: u32 = 0x438;

    /// Map cell 0..=0xFF of a world coordinate (`(v >> 23) + 0x80`).
    fn map_cell(coordinate: i32) -> i32 {
        ((coordinate >> 23) + 0x80).clamp(0, 0xff)
    }

    // JP-REVIEW(R0121): near function: PAL 8003CE04 -> JP 8003D5C0, imm:4, 4 instructions differ (whole routine)
    /// 8003D5C0: map the radar terrain texture. The square of
    /// [`RADAR_RANGE`] around the view, widened to whole 0x1000 cells and
    /// clamped to the texture, becomes the POLY_FT4 at `[gp+0x438]+0xAA4`:
    /// corners rotated into radar pixels around (0x2C, 0x2C), texels
    /// `(corner >> 12) + 0x40`.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_radar::radar_map_3ce04` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn radar_map_3ce04(m: &mut Machine) {
        crate::jp::res_p3_hud_radar::radar_map_3ce04(m)
    }

    /// 8003D9C8: refresh the radar blip records: entities always, ground
    /// objects only while the radar range is at most 0x8000.
    pub fn radar_blips_3d20c(m: &mut Machine) {
        entity_blips_3d24c(m);
        if m.lw(RADAR_RANGE) as i32 <= 0x8000 {
            ground_blips_3d534(m);
        }
    }

    // JP-REVIEW(R0122): near function: PAL 8003D24C -> JP 8003DA08, imm:1, 1 instructions differ (whole routine)
    /// 8003DA08: entity blip records of slots 2..63. Each record keeps its
    /// high flag bits; a live entity (+0 bit 0x4000) shown on the radar
    /// ([`SLOT_FLAGS`] bits 1 and 0x2000) within its range word and inside
    /// the radar square gets its radar position, the visible flag (plus 8
    /// above range 0x7FFF, 4 when highlighted), altitude class against the
    /// player (0 below, 1 level, 2 above, by 0x1000), heading sector and,
    /// with flag 0x10 and a source object, the brightness 0x13..=0x83.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_radar::entity_blips_3d24c` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn entity_blips_3d24c(m: &mut Machine) {
        crate::jp::res_p3_hud_radar::entity_blips_3d24c(m)
    }

    // JP-REVIEW(R0123): near function: PAL 8003D534 -> JP 8003DCF0, imm:1, 1 instructions differ (whole routine)
    /// 8003DCF0: ground-object blip records 0..31: cleared, then for each
    /// live object (mask bit `i`) inside the radar square the radar
    /// position, shape 2, color 0 and the visible flag.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_radar::ground_blips_3d534` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn ground_blips_3d534(m: &mut Machine) {
        crate::jp::res_p3_hud_radar::ground_blips_3d534(m)
    }

    // JP-REVIEW(R0124): near function: PAL 8003D6A0 -> JP 8003DE5C, ins:1 op:2, 3 instructions differ (whole routine)
    /// 8003DE5C: draw the radar blips: entity records with color 0, then
    /// the other entity records (slots shown by [`SLOT_FLAGS`] bit 0x2000),
    /// then at range 0x8000 or less all ground records; in missions 0x12
    /// and 0x25 with gameplay +8 == 1 also the map marker 8003E274
    /// ([`super::extras::radar_marker_3dab4`]).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_radar::draw_blips_3d6a0` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn draw_blips_3d6a0(m: &mut Machine) {
        crate::jp::res_p3_hud_radar::draw_blips_3d6a0(m)
    }

    /// 8003DFF8: fill the next radar sprite (`SPRT` number `[gp+0x434]+8`
    /// at `[gp+0x438]+0x18`) from the blip `record` if it is shown: clut
    /// 0x7F98, the texture cell and size of its shape (+1, +4, +5, flag 8),
    /// the color (+2) at brightness 0x83 or +3 (an eighth while highlighted
    /// and gameplay +0xC bit 2 blinks), centred on (+6, +7); then counts the
    /// sprite. Returns 0.
    pub fn blip_sprite_3d838(m: &mut Machine, record: u32) -> u32 {
        if m.lbu(record) & 1 == 0 {
            return 0;
        }
        let counters = m.gw(COUNTERS);
        let used = m.lw(counters + 8);
        let sprite = m
            .gw(PRIMITIVES)
            .wrapping_add(used.wrapping_mul(20))
            .wrapping_add(0x18);
        m.sh(sprite + 0xe, 0x7f98);
        let square = |m: &mut Machine, size: u16| {
            m.sh(sprite + 0x10, size);
            m.sh(sprite + 0x12, size);
        };
        if m.lbu(record) & 8 != 0 {
            m.sb(sprite + 0xc, 0x62);
            m.sb(sprite + 0xd, 6);
            let size = if m.lbu(record + 2) == 1 { 3 } else { 2 };
            square(m, size);
        } else {
            match m.lbu(record + 1) {
                0 => {
                    // Texture row and size by altitude class, column by
                    // heading sector.
                    let heading = m.lbu(record + 4);
                    let cell = match m.lbu(record + 5) {
                        0 => Some((0x33, 4, (heading << 2).wrapping_add(0x3e))),
                        1 => Some((0xb, 6, heading.wrapping_mul(6))),
                        2 => Some((0x11, 8, heading << 3)),
                        _ => None,
                    };
                    if let Some((v, size, u)) = cell {
                        m.sb(sprite + 0xd, v);
                        square(m, size);
                        m.sb(sprite + 0xc, u);
                    }
                }
                1 => {
                    let size = m.lbu(record + 5);
                    m.sb(sprite + 0xc, 0x62);
                    m.sb(sprite + 0xd, 6);
                    square(m, u16::from(size) + 2);
                }
                2 => {
                    m.sb(sprite + 0xc, 0x62);
                    m.sb(sprite + 0xd, 6);
                    square(m, 1);
                }
                _ => {}
            }
        }
        let flags = m.lbu(record);
        let mut level = 0x83u32;
        if flags & 0x10 != 0 {
            level = u32::from(m.lbu(record + 3));
        }
        if flags & 4 != 0 {
            let state = gameplay(m);
            if m.lw(state + 0xc) & 2 != 0 {
                level >>= 3;
            }
        }
        let level = level as u8;
        let rgb = match m.lbu(record + 2) {
            0 => Some([level >> 1; 3]),
            1 => Some([level, 0, 0]),
            2 => Some([0, 0, level]),
            3 => Some([level, level, 0]),
            _ => None,
        };
        if let Some(rgb) = rgb {
            for (offset, component) in (4u32..).zip(rgb) {
                m.sb(sprite + offset, component);
            }
        }
        let width = m.lhu(sprite + 0x10) as i16;
        let x = m.lbu(record + 6);
        let height = m.lhu(sprite + 0x12) as i16;
        m.sh(sprite + 8, (i32::from(x) - (i32::from(width) >> 1)) as u16);
        let y = m.lbu(record + 7);
        let counters = m.gw(COUNTERS);
        m.sh(
            sprite + 0xa,
            (i32::from(y) - (i32::from(height) >> 1)) as u16,
        );
        let used = m.lw(counters + 8);
        m.sw(counters + 8, used.wrapping_add(1));
        0
    }

    // JP-REVIEW(R0125): near function: PAL 8003DEDC -> JP 8003E69C, imm:4, 4 instructions differ (whole routine)
    /// 8003E69C: draw the 128x128 map window at (0x60, 0x40). The scroll
    /// origin follows the player's map cell; the seven map tiles get their
    /// texels, the player arrow (sprite at `[gp+0x438]+0xFFC`, cell by
    /// heading) and two heading wedges (POLY_F4 points at +0xFD4 and
    /// +0xFEC) are placed, then one 2x2 tile per shown entity in the map
    /// window: first the [`SLOT_FLAGS`] 0x100 slots (red, drawn at 1,1
    /// less), then the others (blue for entity flag 0x40, yellow for 0x100,
    /// else grey, semi-transparent).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_radar::map_window_3dedc` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn map_window_3dedc(m: &mut Machine) {
        crate::jp::res_p3_hud_radar::map_window_3dedc(m)
    }

    /// 8003EC0C: map window position of the world vector at `position`
    /// (x at +0, z at +8): its map cell minus the scroll `origin` halfwords,
    /// stored as two halfwords at `out`. Returns 0 inside the window
    /// (both 0..0x80), else -1.
    pub fn map_position_3e44c(m: &mut Machine, position: u32, out: u32, origin: u32) -> u32 {
        let x = map_cell(m.lw(position) as i32) as u32;
        let z = map_cell((m.lw(position + 8) as i32).wrapping_neg()) as u32;
        let dx = x.wrapping_sub(u32::from(m.lhu(origin)));
        m.sh(out, dx as u16);
        let dz = z.wrapping_sub(u32::from(m.lhu(origin + 2)));
        let wx = m.lh(out);
        m.sh(out + 2, dz as u16);
        let wz = dz as i16;
        if (0..0x80).contains(&wx) && (0..0x80).contains(&wz) {
            0
        } else {
            0xffff_ffff
        }
    }

    /// Targets owned by this group: a0.. as in the original, v0 out.
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        match target {
            0x8003_d5c0 => radar_map_3ce04(m),
            0x8003_d9c8 => radar_blips_3d20c(m),
            0x8003_da08 => entity_blips_3d24c(m),
            0x8003_dcf0 => ground_blips_3d534(m),
            0x8003_de5c => draw_blips_3d6a0(m),
            0x8003_dff8 => return Some(blip_sprite_3d838(m, a0)),
            0x8003_e69c => map_window_3dedc(m),
            0x8003_ec0c => return Some(map_position_3e44c(m, a0, a1, a2)),
            _ => return None,
        }
        Some(0)
    }
}

pub mod tree_39d84 {
    //! 8003B34C..8008FC9C: the per-frame HUD state called by 8003A2B8. Target
    //! markers are projected to the screen (8003B34C, sized by depth in
    //! 8003B758) and fire their one-shot sighting scripts; the lock-on
    //! states of the 62 target slots and the lock cursor animation are
    //! stepped (8003EF40 -> 8003F2CC, 80040A24, with 800409D4/80040A18
    //! starting/stopping the cursor); two small panel animations advance
    //! (80041698, 800419C0); 80042524 resets a message block, 8005650C reads
    //! a flag word, and 8008FC9C queues the full-screen fade tile with its
    //! four textured quads.
    use super::*;
    use crate::{sdk, shared};

    /// 800C67A0: 64 pointers to the per-slot screen records (+0 lock state
    /// byte, +1 cursor-owner byte, +2 lockable byte, +6 marker size
    /// halfword, +8 screen x/y halfwords).
    pub const SCREEN_RECORDS: u32 = 0x800c_67a0;
    /// 800C6ED0: 64 pointers to the per-slot target records (+0 status word,
    /// +0x20 visibility flags, +0x24 frames on screen).
    pub const TARGET_RECORDS: u32 = 0x800c_6ed0;
    /// 800C6FD0: 64 per-slot event flag words.
    pub const EVENT_FLAGS: u32 = 0x800c_6fd0;
    /// 800C6D60: lock-on block (+0xE selected slot, +0x10 lock-suppress
    /// byte, +0x12 locked slot).
    pub const LOCK_BLOCK: u32 = 0x800c_6d60;
    /// 800C7170: message animation block (+0 counter halfword, +2 state,
    /// +6/+7 glyph bytes).
    pub const MESSAGE_BLOCK: u32 = 0x800c_7170;
    /// 800C7180: message slot reset by [`reset_41a48`].
    pub const MESSAGE_SLOT: u32 = 0x800c_7180;
    /// 800BD02C: mission mode byte.
    pub const MISSION_MODE: u32 = 0x800b_d02c;
    /// `gp+0x354` (800BCEC8): lock cursor block (+0 state, +1 slot, +2 size,
    /// +4 x, +6 y halfwords).
    pub const CURSOR: u32 = crate::GP + 0x354;
    /// `gp+0x460` (800BCFD4): panel animation state byte (+2 extent
    /// halfword at `gp+0x462`, word `gp+0x45C` cleared when opened).
    pub const PANEL: u32 = crate::GP + 0x460;
    /// `gp+0x340`: pointer to the weapon/HUD mode block (word 0 bit 2
    /// suppresses lock starts).
    const HUD_MODE: u32 = 0x340;
    /// `gp+0x4B4`: the word returned by [`flag_55038`].
    const FLAG_4BC: u32 = 0x4b4;

    /// The camera-relative coordinate `(position - camera) >> 12` when it
    /// fits a signed halfword (the original's `sltu 0xFFFF, rel + 0x8000`).
    fn relative(position: u32, camera: u32) -> Option<u16> {
        let rel = (position.wrapping_sub(camera) as i32) >> 12;
        ((rel as u32).wrapping_add(0x8000) <= 0xffff).then_some(rel as u16)
    }

    // JP-REVIEW(R0132): near function: PAL 8003ABAC -> JP 8003B34C, imm:5 ins:1 op:2, 8 instructions differ (whole routine)
    /// 8003B34C: project the target markers of slots 2..63 and run their
    /// sighting scripts. Pass 1: for each live entity (word 0 bit 0x4000)
    /// with both records, the camera-relative position (`>> 12`, each
    /// component a signed halfword) goes through RotTransPers into the
    /// screen record (+8 x/y, +6 size from [`marker_size_3afb4`]); the
    /// target record's +0x20 flags get 0x10000 (in range), 0x40000 (valid
    /// projection) and 0x20000 (inside 320x256). Pass 2: on-screen targets
    /// count frames in +0x24 while HUD bit 4 is on (reset otherwise); after
    /// 90 frames (0 in mission mode 6) the slot's first script runs once
    /// (event bit 0x1000000) and, while `[gameplay+8] > 0`, the second one
    /// (event bit 0x2000000). The original returns 0.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_lock::screen_targets_3abac` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn screen_targets_3abac(m: &mut Machine) {
        crate::jp::res_p3_hud_lock::screen_targets_3abac(m)
    }

    /// 8003B758: marker size for the projected depth `depth` (SZ3 / 4):
    /// 2 beyond 0x1000, else `3 * (0x1000 - depth)^2 >> 21` clamped to
    /// 2..=24.
    pub fn marker_size_3afb4(depth: u32) -> u32 {
        let depth = depth as i32;
        if depth >= 0x1001 {
            return 2;
        }
        let near = 0x1000i32.wrapping_sub(depth);
        let square = near.wrapping_mul(near) >> 12;
        let size = (square << 1).wrapping_add(square) >> 9;
        size.clamp(2, 0x18) as u32
    }

    // JP-REVIEW(R0133): changed function: PAL 8003E780 -> JP 8003EF40, call-:1 del:12 imm:1 ins:10 op:4 reg:1, 28 instructions differ (whole routine)
    /// 8003EF40: lock-on update. The lock-suppress byte (`LOCK_BLOCK +
    /// 0x10`) is set for weapon modes 5, 6 and 9 (from 8003A62C, or when it
    /// returns -1, context byte +0x3AE / +0x3AF chosen by 8003A5C0), then
    /// the slot lock states (8003F2CC) and the lock cursor (80040A24) step.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_lock::lockon_3e780` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn lockon_3e780(m: &mut Machine) {
        crate::jp::res_p3_hud_lock::lockon_3e780(m)
    }

    // JP-REVIEW(R0134): near function: PAL 8003EB14 -> JP 8003F2CC, imm:16 ins:1, 17 instructions differ (whole routine)
    /// 8003F2CC: lock-on state of slots 2..63 in their screen records
    /// (+0: 0 none, 1 visible lockable target, 2 selected, 3 locked; +1:
    /// owns the lock cursor). The selected slot (`LOCK_BLOCK + 0xE`) starts
    /// the cursor (800409D4) when lockable and not suppressed; the locked
    /// slot goes to `LOCK_BLOCK + 0x12` (0xFF when none). An unselected
    /// slot owning the cursor stops it (80040A18).
    pub fn lock_states_3eb14(m: &mut Machine) {
        m.sb(LOCK_BLOCK + 0x12, 0xff);
        for index in 2..64u32 {
            let record = m.lw(SCREEN_RECORDS + index * 4);
            let target = m.lw(TARGET_RECORDS + index * 4);
            if record == 0 {
                continue;
            }
            m.sb(record, 0);
            if target == 0 {
                continue;
            }
            let entities = super::entities(m);
            let entity = m.lw(entities.wrapping_add(index * 4));
            if m.lw(entity) & 0x4000 == 0 {
                continue;
            }
            let event = EVENT_FLAGS + index * 4;
            if m.lw(event) & 0x1000 == 0
                || m.lbu(record.wrapping_add(2)) == 0
                || m.lw(target.wrapping_add(0x20)) & 0x2_0000 == 0
                || m.lw(target) & 1 == 0
            {
                continue;
            }
            m.sb(record, 1);
            if u32::from(m.lbu(LOCK_BLOCK + 0xe)) != index {
                if m.lbu(record.wrapping_add(1)) != 0 && u32::from(m.lbu(CURSOR + 1)) == index {
                    let owner = lock_clear_40160(m);
                    m.sb(record.wrapping_add(1), owner as u8);
                }
                continue;
            }
            m.sb(record, 2);
            let entities = super::entities(m);
            let entity = m.lw(entities.wrapping_add(index * 4));
            if m.lw(entity) & 0x40 != 0 || m.lw(target) & 8 == 0 || m.lw(event) & 0x4000 == 0 {
                continue;
            }
            let hud_mode = m.gw(HUD_MODE);
            if m.lw(hud_mode) & 4 != 0 || m.lbu(LOCK_BLOCK + 0x10) != 0 {
                continue;
            }
            if m.lbu(record.wrapping_add(1)) != 0 && u32::from(m.lbu(CURSOR + 1)) == index {
                match m.lbu(CURSOR) {
                    2 => {
                        m.sb(record, 3);
                        m.sb(LOCK_BLOCK + 0x12, index as u8);
                    }
                    0 => m.sb(record.wrapping_add(1), 0),
                    _ => {}
                }
            } else {
                let started = lock_start_4011c(m, index);
                m.sb(record.wrapping_add(1), started as u8);
            }
        }
    }

    // JP-REVIEW(R0135): near function: PAL 8004011C -> JP 800409D4, imm:1, 1 instructions differ (whole routine)
    /// 800409D4: start the lock cursor on slot `index` unless it is already
    /// running: state 1, size 2 at the screen centre (160, 128). Returns 1
    /// when started, else 0.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_lock::lock_start_4011c` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn lock_start_4011c(m: &mut Machine, index: u32) -> u32 {
        crate::jp::res_p3_hud_lock::lock_start_4011c(m, index)
    }

    /// 80040A18: stop the lock cursor (state 0); returns 0xFF.
    pub fn lock_clear_40160(m: &mut Machine) -> u32 {
        m.sb(CURSOR, 0);
        0xff
    }

    // JP-REVIEW(R0138): changed function: PAL 8004016C -> JP 80040A24, call+:3 del:3 imm:11 ins:74 op:7 reg:2, 97 instructions differ (whole routine)
    /// 80040A24: lock cursor animation. While running (state 1) the cursor
    /// moves toward the owner slot's marker (x/y by at most its size, size
    /// by at most 4 per frame); on arrival it becomes locked (state 2,
    /// following the marker exactly). It stops when the owner leaves the
    /// lockable state; sounds 0x2D/0x32 play every 8th frame (8002DBE0) and
    /// 8004CEB0(0xE) runs on arrival in mission modes 4, 8, 15, 30 for an
    /// entity with bit 0x100.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_lock::lock_cursor_4016c` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn lock_cursor_4016c(m: &mut Machine) {
        crate::jp::res_p3_hud_lock::lock_cursor_4016c(m)
    }

    /// 80041698: panel open/close animation (unless the context is paused,
    /// bit 0x20000000). State 1 grows the extent `gp+0x462` by 16 up to 200
    /// (sound 0x3E at the start) and then opens (state 2, `gp+0x45C` = 0);
    /// state 3 shrinks it by 32 (sound 0x36 at the start) down to closed
    /// (state 0). The original returns 0.
    pub fn panel_40ce8(m: &mut Machine) {
        let context = super::context(m);
        if m.lw(context) & 0x2000_0000 != 0 {
            return;
        }
        let extent = PANEL + 2;
        match m.lbu(PANEL) {
            1 => {
                if m.lh(extent) == 0 {
                    super::sound_2d724(m, 0x3e);
                }
                let value = m.lhu(extent).wrapping_add(0x10);
                m.sh(extent, value);
                if value as i16 >= 0xc8 {
                    m.sb(PANEL, 2);
                    m.sh(extent, 0xc8);
                    m.sw(PANEL - 4, 0);
                }
            }
            3 => {
                if m.lh(extent) == 0xc8 {
                    super::sound_2d724(m, 0x36);
                }
                let value = m.lhu(extent).wrapping_sub(0x20);
                m.sh(extent, value);
                if value as i16 <= 0 {
                    m.sh(extent, 0);
                    m.sb(PANEL, 0);
                }
            }
            _ => {}
        }
    }

    /// 800419C0: message glyph animation (unless the context is paused).
    /// State 1 steps the glyph pair (+6/+7) through four frames by the
    /// counter (+0) and moves to state 2 at frame 3; state 3 fades +7 by
    /// `>> 2` and ends (state 0) when it reaches 0. The original returns 0.
    pub fn message_41010(m: &mut Machine) {
        let context = super::context(m);
        if m.lw(context) & 0x2000_0000 != 0 {
            return;
        }
        let block = MESSAGE_BLOCK;
        match m.lbu(block + 2) {
            1 => {
                let glyphs = match m.lh(block) {
                    0 => Some((0x2b, 6, None)),
                    1 => Some((0xc, 0x29, None)),
                    2 => Some((0x15, 0x14, None)),
                    3 => Some((0x2b, 0x29, Some(2))),
                    _ => None,
                };
                if let Some((first, second, state)) = glyphs {
                    m.sb(block + 6, first);
                    m.sb(block + 7, second);
                    if let Some(state) = state {
                        m.sb(block + 2, state);
                    }
                }
                let counter = m.lhu(block).wrapping_add(1);
                m.sh(block, counter);
            }
            3 => {
                let value = m.lbu(block + 7);
                m.sb(block + 6, 0x2b);
                let value = value >> 2;
                m.sb(block + 7, value);
                if value == 0 {
                    m.sb(block + 2, 0);
                }
            }
            _ => {}
        }
    }

    /// 80042524: reset the message slot (word -1, word 0, two bytes 0).
    pub fn reset_41a48(m: &mut Machine) {
        m.sw(MESSAGE_SLOT, 0xffff_ffff);
        m.sw(MESSAGE_SLOT + 4, 0);
        m.sb(MESSAGE_SLOT + 8, 0);
        m.sb(MESSAGE_SLOT + 0xc, 0);
    }

    /// 8005650C: the word at `gp+0x4B4`.
    pub fn flag_55038(m: &mut Machine) -> u32 {
        m.gw(FLAG_4BC)
    }

    // JP-REVIEW(R0139): near function: PAL 8008E42C -> JP 8008FC9C, imm:15, 15 instructions differ (whole routine)
    /// 8008FC9C: queue the full-screen fade at `level` (0..0x1000; negative
    /// draws nothing) into ordering-table slot `ot`, building primitives at
    /// `prim`: a DR_MODE, a 320x256 TILE (DPCS-faded semi-transparent white,
    /// or opaque black at full level), four textured 160x128 quads with a
    /// DPCS-faded color when the level is partial, and a closing DR_MODE.
    /// Returns the next free primitive address (`prim` when nothing is
    /// drawn).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_lock::fade_8e42c` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn fade_8e42c(m: &mut Machine, ot: u32, prim: u32, level: u32) -> u32 {
        crate::jp::res_p3_hud_lock::fade_8e42c(m, ot, prim, level)
    }

    /// Native targets of this group, by original address.
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        match target {
            0x8003_b34c => {
                screen_targets_3abac(m);
                Some(0)
            }
            0x8003_b758 => Some(marker_size_3afb4(a0)),
            0x8003_ef40 => {
                lockon_3e780(m);
                Some(0)
            }
            0x8003_f2cc => {
                lock_states_3eb14(m);
                Some(0)
            }
            0x8004_09d4 => Some(lock_start_4011c(m, a0)),
            0x8004_0a18 => Some(lock_clear_40160(m)),
            0x8004_0a24 => {
                lock_cursor_4016c(m);
                Some(0)
            }
            0x8004_1698 => {
                panel_40ce8(m);
                Some(0)
            }
            0x8004_19c0 => {
                message_41010(m);
                Some(0)
            }
            0x8004_2524 => {
                reset_41a48(m);
                Some(0)
            }
            0x8005_650c => Some(flag_55038(m)),
            0x8008_fc9c => Some(fade_8e42c(m, a0, a1, a2)),
            _ => None,
        }
    }
}

pub mod tree_40a24 {
    //! 8003A12C, PAL 8003A158 and 800413FC..: the HUD color state (green, or a
    //! blinking red alert), the mission-mode-3 counter text, and the overlay
    //! packet set `[gp+0x450]`: the upper-right frame, the target box with
    //! its scrolling scan lines, the warning labels chosen by 80042544 (alert
    //! flags, leaving the area, altitude limits, mission conditions), and the
    //! radio message text line. All are called by the HUD composition
    //! 8003A0B4 with the 80033814 view block in a0 (only 8003A12C and
    //! 800413FC read it).
    use super::*;
    use crate::machine::GP;

    /// 800C6D60: HUD color state. +0 word blink counter, +6 alert timer
    /// (halfword), +8..+A highlight RGB, +B..+D base RGB, +F mode (0 green,
    /// 1 alert, 2 blinking alert), +0x11 alert flag.
    pub const HUD_COLOR: u32 = 0x800c_6d60;
    /// 800C6ECC: alert flags (bit 1 forces the alert colors and warning 1,
    /// bit 0 warning 2).
    const ALERT_FLAGS: u32 = 0x800c_6ecc;
    /// `[gp+0x450]`: overlay packet set (+0/+4 the two buffers, +8 sprite
    /// count, +0xC draw-mode count); `[gp+0x454]`: this frame's buffer.
    const SET_440_TABLE: u32 = 0x450;
    const SET_440_BUFFER: u32 = 0x454;
    /// gp+0x462: halfword left shift of the upper-right frame.
    const FRAME_SHIFT: u32 = GP + 0x462;
    /// gp+0x464: label slot drawn by [`label_41dc8`].
    const LABEL_SLOT: u32 = GP + 0x464;
    /// `[gp+0x478]`: voice/stream state (+0x11C current id, 0xFF idle).
    const VOICE: u32 = 0x478;
    /// 800C7170: target box (+2 enable, +3 texture cell, +6 width, +7
    /// height, +8/+A x/y halfwords).
    const BOX: u32 = 0x800c_7170;
    /// 800C7180: active warning (+0 code or -1, +4 remaining frames, +8 and
    /// +0xC two label slots: active byte, label id, frame halfword).
    const WARNING: u32 = 0x800c_7180;
    /// 800B149C: per warning code (halfword frames, label A, label B).
    const WARNINGS: u32 = 0x800b_149c;
    /// 800B1254: 16-byte labels (+0 blink kind in the low nibble, +1 color,
    /// +2 frames, +4/+6 x/y, +8/+9 and +A/+B glyph and x offset of two
    /// sprites (+A 0xFF: none), +C sound id, +E sound frame).
    const LABELS: u32 = 0x800b_1254;

    /// Signed remainder of `div` (HI), with the documented divide-by-zero
    /// result (the dividend).
    fn remainder(dividend: u32, divisor: i32) -> u32 {
        if divisor == 0 {
            dividend
        } else {
            (dividend as i32).wrapping_rem(divisor) as u32
        }
    }

    /// 8003A12C: update the HUD color state. A view flag (`view` +0 & 7)
    /// restarts the 15-frame alert timer; alert flag bit 1 selects mode 1,
    /// a running timer mode 2 (counting down), otherwise mode 0. Mode 1 is
    /// red, mode 2 blinks dark red/green with the counter (period 6), mode 0
    /// is green.
    pub fn hud_color_39bf8(m: &mut Machine, view: u32) {
        let state = HUD_COLOR;
        if m.lw(view) & 7 != 0 {
            m.sh(state + 6, 0xf);
        }
        if m.lw(ALERT_FLAGS) & 2 != 0 {
            m.sb(state + 0xf, 1);
        } else {
            let timer = m.lh(state + 6);
            let raw = m.lhu(state + 6);
            if timer > 0 {
                m.sb(state + 0xf, 2);
                m.sh(state + 6, raw.wrapping_sub(1));
            } else {
                m.sb(state + 0xf, 0);
            }
        }
        // (base red, highlight red) of the alert colors.
        let alert = match m.lbu(state + 0xf) {
            1 => Some((0x83, 0xff)),
            // mult by 0x2AAAAAAB: signed remainder by 6.
            2 if (m.lw(state) as i32) % 6 < 4 => Some((0x43, 0x83)),
            _ => None,
        };
        if let Some((base, highlight)) = alert {
            m.sb(state + 0xb, base);
            m.sb(state + 0xc, 0);
            m.sb(state + 0xd, 0);
            m.sb(state + 8, highlight);
            m.sb(state + 9, 0);
            m.sb(state + 0xa, 0);
            m.sb(state + 0x11, 1);
        } else {
            m.sb(state + 0xc, 0x83);
            m.sb(state + 0xb, 0);
            m.sb(state + 0xd, 0);
            m.sb(state + 8, 0);
            m.sb(state + 9, 0xff);
            m.sb(state + 0xa, 0);
            m.sb(state + 0x11, 0);
        }
    }

    /// 800413FC: overlay set `[gp+0x450]`: select this frame's buffer, run
    /// the warnings (gameplay +4 bit 2, context +0 without bit 29), the
    /// gp+0x464 label (gameplay +4 & 0x2C00), the upper-right frame, the
    /// target box, then link the frame and draw the message line.
    pub fn overlay_40a24(m: &mut Machine, view: u32) {
        select_buffer_40acc(m);
        let game = gameplay(m);
        if m.lw(game + 4) & 4 != 0 {
            let ctx = context(m);
            if m.lw(ctx) & 0x2000_0000 == 0 {
                warnings_41a68(m, view);
            }
        }
        let game = gameplay(m);
        if m.lw(game + 4) & 0x2c00 != 0 {
            label_41dc8(m);
        }
        let shift = m.lh(FRAME_SHIFT);
        frame_40de4(m, shift as i32 as u32);
        target_box_4112c(m);
        link_frame_40b00(m);
    }

    /// 800414A4: reset the sprite and draw-mode counts of set `[gp+0x450]`
    /// and select the buffer of context +4 into `[gp+0x454]`.
    pub fn select_buffer_40acc(m: &mut Machine) {
        let table = m.gw(SET_440_TABLE);
        let ctx = context(m);
        m.sw(table + 0xc, 0);
        m.sw(table + 8, 0);
        let index = m.lw(ctx + 4);
        let buffer = m.lw(table.wrapping_add(index << 2));
        m.set_gw(SET_440_BUFFER, buffer);
    }

    // JP-REVIEW(R0143): changed function: PAL 80040B00 -> JP 800414D8, call-:1 del:11 ins:1 op:3 reg:29, 44 instructions differ (whole routine)
    /// 800414D8: draw the message line (PAL 80054D18) into gameplay +0x14, then
    /// link the buffer's first primitive and, when gp+0x460 is set, the
    /// upper-right frame primitives (+0x14, +0x28, +0x48, +0x58, +0x38,
    /// +0x60) into the ordering-table slot gameplay +0x10.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_warn::link_frame_40b00` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn link_frame_40b00(m: &mut Machine) {
        crate::jp::res_p3_hud_warn::link_frame_40b00(m)
    }

    // JP-REVIEW(R0144): near function: PAL 80040DE4 -> JP 80041794, imm:4, 4 instructions differ (whole routine)
    /// 80041794: fill the upper-right frame primitives of this frame's
    /// buffer, shifted left by `shift` (the sign-extended gp+0x462): two
    /// base-color sprites (+0, +0x14), highlight bars (+0x28, +0x48) and a
    /// black backing tile (+0x38), whose widths grow with the shift.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_warn::frame_40de4` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn frame_40de4(m: &mut Machine, shift: u32) {
        crate::jp::res_p3_hud_warn::frame_40de4(m, shift)
    }

    /// 80041ADC: when the target box is enabled (800C7170 +2), fill and link
    /// its primitives: the base-color frame quad (+0x210), the textured
    /// inner quad (+0x1E8, cell 800C7170 +3), the fill tile (+0x238) and
    /// two scan lines (+0x248, +0x258) scrolling with the frame counter
    /// gameplay +0xC; also links the fixed +0x268 and +0x270 packets.
    /// Returns 0 (the original clears v0 on both paths).
    pub fn target_box_4112c(m: &mut Machine) -> u32 {
        if m.lbu(BOX + 2) == 0 {
            return 0;
        }
        let width = u32::from(m.lbu(BOX + 6));
        let height = u32::from(m.lbu(BOX + 7));
        let inner_width = width.wrapping_sub(0xb);
        let inner_height = height.wrapping_sub(5);
        // srl 31/addu/sra 1: signed halves.
        let x = u32::from(m.lhu(BOX + 8)).wrapping_add(((0x2b - width as i32) / 2) as u32);
        let y = u32::from(m.lhu(BOX + 0xa)).wrapping_add(((0x29 - height as i32) / 2) as u32);
        let inner_x = x.wrapping_add(6);
        let inner_y = y.wrapping_add(3);
        let fill_width = if (inner_width as i32) > 0 {
            inner_width
        } else {
            1
        };
        let fill_height = if (inner_height as i32) > 0 {
            inner_height
        } else {
            1
        };

        let quad = m.gw(SET_440_BUFFER) + 0x210;
        let r = m.lbu(HUD_COLOR + 0xb);
        m.sb(quad + 4, r);
        let g = m.lbu(HUD_COLOR + 0xc);
        m.sb(quad + 5, g);
        let b = m.lbu(HUD_COLOR + 0xd);
        m.sh(quad + 8, x as u16);
        m.sh(quad + 0xa, y as u16);
        m.sb(quad + 6, b);
        let w = u32::from(m.lbu(BOX + 6));
        m.sh(quad + 0x12, y as u16);
        m.sh(quad + 0x18, x as u16);
        m.sh(quad + 0x10, x.wrapping_add(w) as u16);
        let h = u32::from(m.lbu(BOX + 7));
        m.sh(quad + 0x1a, y.wrapping_add(h) as u16);
        let w = u32::from(m.lbu(BOX + 6));
        m.sh(quad + 0x20, x.wrapping_add(w) as u16);
        let h = u32::from(m.lbu(BOX + 7));
        m.sh(quad + 0x22, y.wrapping_add(h) as u16);

        let quad = m.gw(SET_440_BUFFER) + 0x1e8;
        let cell = m.lbu(BOX + 3);
        let right = fill_width.wrapping_add(x.wrapping_add(5)) as u16;
        let bottom = fill_height.wrapping_add(y.wrapping_add(2)) as u16;
        m.sh(quad + 8, inner_x as u16);
        m.sh(quad + 0xa, inner_y as u16);
        m.sh(quad + 0x10, right);
        m.sh(quad + 0x12, inner_y as u16);
        m.sh(quad + 0x18, inner_x as u16);
        m.sh(quad + 0x1a, bottom);
        m.sh(quad + 0x20, right);
        m.sh(quad + 0x22, bottom);
        m.sh(
            quad + 0xe,
            (u32::from(cell) << 6).wrapping_add(0x3216) as u16,
        );
        // Cell: u = (cell & 7) * 32, v = (cell >> 3) * 36.
        let u = u32::from(cell & 7) << 5;
        let v = u32::from(cell >> 3) * 36;
        for (offset, du, dv) in [
            (0xc, 0, 0x80u32),
            (0x14, 0x1f, 0x80),
            (0x1c, 0, 0x5d),
            (0x24, 0x1f, 0x5d),
        ] {
            m.sb(quad + offset, (u + du) as u8);
            m.sb(quad + offset + 1, v.wrapping_sub(dv) as u8);
        }

        // The tile and scan-line sizes clamp as halfwords.
        let tile_width = fill_width.wrapping_sub(2);
        let tile_width = if ((tile_width << 16) as i32) > 0 {
            tile_width
        } else {
            1
        };
        let tile_height = fill_height.wrapping_sub(3);
        let tile_height = if ((tile_height << 16) as i32) > 0 {
            tile_height
        } else {
            1
        };
        let divisor = i32::from(tile_height as u16 as i16);
        let tile_x = x.wrapping_add(7) as u16;
        let buffer = m.gw(SET_440_BUFFER);
        let game = gameplay(m);
        let tile = buffer + 0x238;
        m.sh(tile + 8, tile_x);
        m.sh(tile + 0xa, y.wrapping_add(4) as u16);
        m.sh(tile + 0xc, tile_width as u16);
        m.sh(tile + 0xe, tile_height as u16);
        let line = buffer + 0x248;
        m.sh(line + 8, tile_x);
        let time = m.lw(game + 0xc);
        let scroll = remainder(time << 2, divisor);
        m.sh(line + 0xc, tile_width as u16);
        m.sh(line + 0xe, 1);
        m.sh(line + 0xa, inner_y.wrapping_add(scroll) as u16);
        let line = buffer + 0x258;
        m.sh(line + 8, tile_x);
        let time = m.lw(game + 0xc);
        let scroll = remainder((time << 2).wrapping_add(time), divisor);
        m.sh(line + 0xe, 1);
        m.sh(line + 0xc, tile_width as u16);
        m.sh(line + 0xa, inner_y.wrapping_add(scroll) as u16);

        for offset in [0x210, 0x1e8, 0x248, 0x258, 0x268, 0x238, 0x270] {
            let ot = m.lw(game + 0x10);
            add_prim(m, ot, buffer + offset);
        }
        0
    }

    // JP-REVIEW(R0148): near function: PAL 80041A68 -> JP 80042544, call+:3 del:7 imm:8 ins:28 op:2 reg:5, 50 instructions differ (whole routine)
    /// 80042544: pick the highest-priority warning (codes 0..0xA: view
    /// flags, alert flags, leaving the area (800476F0), minimum altitude,
    /// altitude limit after 30 frames, context/mission conditions) and raise
    /// it (8004287C); keep mission flag 0x20 equal to "view +0x1C is zero";
    /// then draw both labels of the active warning and expire it.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_warn::warnings_41a68` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn warnings_41a68(m: &mut Machine, view: u32) {
        crate::jp::res_p3_hud_warn::warnings_41a68(m, view)
    }

    /// 8004287C: make `code` the active warning unless one with a lower or
    /// equal code is active: its frame count and two label ids come from
    /// 800B149C, both label slots restart at frame 0.
    pub fn raise_warning_41d4c(m: &mut Machine, code: u32) {
        let active = m.lw(WARNING);
        if active != 0xffff_ffff && (code as i32) >= (active as i32) {
            return;
        }
        let entry = (code << 2).wrapping_add(WARNINGS);
        m.sw(WARNING, code);
        let frames = m.lh(entry);
        m.sb(WARNING + 8, 1);
        m.sw(WARNING + 4, frames as i32 as u32);
        let first = m.lbu(entry + 2);
        m.sh(WARNING + 0xa, 0);
        m.sb(WARNING + 9, first);
        m.sb(WARNING + 0xc, 1);
        let second = m.lbu(entry + 3);
        m.sh(WARNING + 0xe, 0);
        m.sb(WARNING + 0xd, second);
    }

    // JP-REVIEW(R0152): changed function: PAL 800415D8 -> JP 80041F88, del:4 imm:2 ins:79 op:11 reg:74, 170 instructions differ (whole routine)
    /// 80041F88: run the label slot `slot` (+0 active, +1 label id, +2 frame
    /// halfword): advance its frame (starting the label's sound at its sound
    /// frame when no voice plays) unless context +0 has bit 29, deactivate
    /// it after the label's frames, and, in its blink phase, draw its one or
    /// two glyph sprites and the colored marker sprite with their draw
    /// modes. Returns the slot's active byte (0 for an empty slot).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_warn::warning_label_415d8` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn warning_label_415d8(m: &mut Machine, slot: u32) -> u32 {
        crate::jp::res_p3_hud_warn::warning_label_415d8(m, slot)
    }

    /// 800428F8: run the label slot at gp+0x464 (80041F88); returns its v0.
    pub fn label_41dc8(m: &mut Machine) -> u32 {
        warning_label_415d8(m, LABEL_SLOT)
    }

    /// 8004C858: the current voice id (`[gp+0x478]` +0x11C; 0xFF when idle).
    pub fn voice_4b108(m: &mut Machine) -> u32 {
        let voice = m.gw(VOICE);
        m.lw(voice + 0x11c)
    }

    // JP-REVIEW(R0153): changed function: PAL 80046AA8 -> JP 800476F0, del:7 imm:2 ins:7 op:4 reg:4, 24 instructions differ (whole routine)
    /// 800476F0: area status of entity `index`: 2 when its x/z position is
    /// outside the `[gp+0x33C]` box, 1 when the position 256 frames ahead
    /// (velocity +0x38..+0x40) is outside it, or (mission modes 0x2A, 0x3A)
    /// its predicted altitude passes the mode's limits, else 0. Also called
    /// from 80046B4C.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud::area_status_46aa8` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn area_status_46aa8(m: &mut Machine, index: u32) -> u32 {
        crate::jp::res_p3_hud::area_status_46aa8(m, index)
    }

    /// Native targets of this group by original address.
    #[allow(unused_variables)]
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        let v0 = match target {
            0x8003_a12c => {
                hud_color_39bf8(m, a0);
                0
            }
            0x8004_13fc => {
                overlay_40a24(m, a0);
                0
            }
            0x8004_14a4 => {
                select_buffer_40acc(m);
                0
            }
            0x8004_14d8 => {
                link_frame_40b00(m);
                0
            }
            0x8004_1794 => {
                frame_40de4(m, a0);
                0
            }
            0x8004_1adc => target_box_4112c(m),
            0x8004_2544 => {
                warnings_41a68(m, a0);
                0
            }
            0x8004_287c => {
                raise_warning_41d4c(m, a0);
                0
            }
            0x8004_1f88 => warning_label_415d8(m, a0),
            0x8004_28f8 => label_41dc8(m),
            0x8004_c858 => voice_4b108(m),
            0x8004_76f0 => area_status_46aa8(m, a0),
            _ => return None,
        };
        Some(v0)
    }
}

pub mod tree_62680_a {
    //! 80063F80..80065D8C (first half of the callees of 80063E4C) and
    //! 80044F94: the HUD packet pool and the gun-sight overlay. `[gp+0x500]`
    //! describes a double-buffered pool of prebuilt packets (+0x00/+0x04 the
    //! two buffers, +0x08..+0x20 the packets of each class used this frame,
    //! +0x24 a flag for the packet at buffer +0x940); `[gp+0x504]` is the
    //! current buffer. 80063F80 starts a frame, the drawers fill packets
    //! (80064CB4 three centre lines, 8006447C the boresight projection and
    //! its marker, 800660F8 the four corner brackets around the boresight,
    //! 80065D8C the target direction arrow) and 80063FCC links every used
    //! packet into ordering-table slot `[gameplay+0x10]`. 80044F94 (shared
    //! with four other engine routines) rotates a world offset into an
    //! object's frame.
    use super::*;
    use crate::{sdk, shared};

    /// `[gp+0x500]`: the HUD packet pool descriptor.
    const POOL: u32 = 0x500;
    /// `[gp+0x504]`: the pool buffer of this frame.
    const BUFFER: u32 = 0x504;
    /// 800C6D60: HUD state block (+0x08..+0x0A color, +0x0E target slot or
    /// 0xFF, +0x14 boresight screen xy, +0x18 boresight depth).
    const STATE: u32 = 0x800c_6d60;
    /// 800104AC: 16-bit sine table (0x1000 entries per quarter turn, cosine
    /// at +0x1000 entries).
    const SINE: u32 = 0x8001_04ac;
    /// Line class of the pool: count word +0x14, 0x10-byte `LINE_F2`
    /// packets from buffer +0x3D8.
    const LINE_COUNT: u32 = 0x14;
    const LINE_BASE: u32 = 0x3d8;
    /// Quad class of the pool: count word +0x1C, 0x28-byte packets
    /// (`POLY_G4`) from buffer +0x878.
    const QUAD_COUNT: u32 = 0x1c;
    const QUAD_BASE: u32 = 0x878;
    /// Packet classes in the order 80063FCC links them: (count word in the
    /// descriptor, first packet in the buffer, packet stride).
    const CLASSES: [(u32, u32, u32); 7] = [
        (0x08, 0x008, 0x18),
        (0x0c, 0x188, 0x1c),
        (0x10, 0x1f8, 0x28),
        (LINE_COUNT, LINE_BASE, 0x10),
        (0x18, 0x7d8, 0x14),
        (QUAD_COUNT, QUAD_BASE, 0x28),
        (0x20, 0x8a0, 0x14),
    ];
    /// 800ADFB8 printf (no `sdk.rs` function yet): the first class holds at
    /// most 15 packets, more print the 800245BC format `"AVIPKT %d\n"`.
    const PRINTF: u32 = 0x800a_c3e8;

    /// Signed `div` quotient (LO). The compiled code guards a zero divisor
    /// with `break 7` and `0x80000000 / -1` with `break 6`, so the original
    /// traps there; the translation keeps the R3000 quotient (-1, or 1 for a
    /// negative dividend; `i32::MIN` for the overflow).
    fn quotient(numerator: i32, divisor: i32) -> i32 {
        if divisor == 0 {
            if numerator < 0 { 1 } else { -1 }
        } else {
            numerator.wrapping_div(divisor)
        }
    }

    /// Pool count word `offset` plus one.
    fn bump(m: &mut Machine, pool: u32, offset: u32) {
        let count = m.lw(pool + offset);
        m.sw(pool + offset, count.wrapping_add(1));
    }

    /// The next free packet of the line class.
    fn next_line(m: &mut Machine) -> u32 {
        let pool = m.gw(POOL);
        let count = m.lw(pool + LINE_COUNT);
        let buffer = m.gw(BUFFER);
        buffer.wrapping_add((count << 4).wrapping_add(LINE_BASE))
    }

    /// Copy the HUD color (state +0x08..+0x0A) into the packet's RGB bytes
    /// (byte by byte, as the original).
    fn copy_color(m: &mut Machine, prim: u32) {
        for i in 0..3 {
            let value = m.lbu(STATE + 8 + i);
            m.sb(prim + 4 + i, value);
        }
    }

    /// 80063F80: start the frame's pool: clear the class counts (+0x24 down
    /// to +0x08) and select buffer `[pool + 4 * [context+4]]`. Reads no
    /// argument register.
    pub fn reset_pool_627b4(m: &mut Machine) {
        let pool = m.gw(POOL);
        let context = context(m);
        for offset in (0x08..=0x24).rev().step_by(4) {
            m.sw(pool + offset, 0);
        }
        let index = m.lw(context + 4);
        let buffer = m.lw(pool.wrapping_add(index << 2));
        m.set_gw(BUFFER, buffer);
    }

    /// 80063FCC: link every packet used this frame into ordering-table slot
    /// `[gameplay+0x10]` (libgpu `addPrim` each): the classes in
    /// [`CLASSES`] order, packets in ascending order (the count word is
    /// reread after each one), then the packet at buffer +0x940 when pool
    /// +0x24 is 1, and last the buffer's first packet (drawn first). With 16
    /// or more packets in the first class it prints the overflow warning
    /// (printf 800ADFB8, debug output as a host call) and skips that class. Reads no
    /// argument register (a0 is loaded first).
    pub fn link_pool_62800(m: &mut Machine) {
        let gameplay = gameplay(m);
        let slot = m.lw(gameplay + 0x10);
        for (class, (count, base, stride)) in CLASSES.into_iter().enumerate() {
            let pool = m.gw(POOL);
            let used = m.lw(pool + count);
            if class == 0 && used >= 0x10 {
                // printf(0x800245BC "AVIPKT %d\n", used)
                m.host_call(PRINTF, [0x8002_45bc, used, 0, 0]);
                continue;
            }
            if used as i32 <= 0 {
                continue;
            }
            let buffer = m.gw(BUFFER);
            let mut index = 0u32;
            loop {
                let prim = buffer.wrapping_add(base.wrapping_add(index.wrapping_mul(stride)));
                index = index.wrapping_add(1);
                add_prim(m, slot, prim);
                if index as i32 >= m.lw(pool + count) as i32 {
                    break;
                }
            }
        }
        let pool = m.gw(POOL);
        if m.lw(pool + 0x24) == 1 {
            let buffer = m.gw(BUFFER);
            add_prim(m, slot, buffer.wrapping_add(0x940));
        }
        let buffer = m.gw(BUFFER);
        add_prim(m, slot, buffer);
    }

    /// 80064400 (`view`): call 8006466C (`view`, 0) when any of the three
    /// halfwords at `[view+0x10]` lies outside -2..=2. Returns the original
    /// v0: 8006466C's result, or 1 (the last compare) when it is not called
    /// (80063E4C ignores it).
    pub fn rotation_check_62c34(m: &mut Machine, view: u32) -> u32 {
        let angles = m.lw(view + 0x10);
        for offset in [0, 2, 4] {
            let value = m.lhu(angles + offset);
            if value.wrapping_add(2) >= 5 {
                // 8006466C (a0 = view, a1 = 0, both unread). The original
                // 0x18 frame keeps its nested frames at their addresses.
                const FRAME: u32 = 0x18;
                m.push_frame(FRAME);
                let v0 = super::tree_62ea0::attitude_marker_62ea0(m);
                m.pop_frame(FRAME);
                return v0;
            }
        }
        1
    }

    /// 8006447C (`view`): project the boresight point (0, 0, 0x100) with
    /// zero translation and the rotation 800308F0 builds from the angles at
    /// `[view+0x10]`, and store its screen xy and depth at state
    /// +0x14/+0x18. Then rotate it further by the negated x/y angles at
    /// `[view+0xC]` (800308F0, MulMatrix2) and, when that point is in front,
    /// append a two-line marker (x-2..x+2 at y; x at y-3..y) to the line
    /// class. Returns 0.
    pub fn boresight_62cb0(m: &mut Machine, view: u32) -> u32 {
        const FRAME: u32 = 0x78;
        let frame = m.push_frame(FRAME);
        let rotation = frame + 0x10; // MATRIX
        let offset = frame + 0x30; // MATRIX
        let angles = frame + 0x50; // SVECTOR
        let point = frame + 0x58; // SVECTOR
        let screen = frame + 0x60;
        let depth_cue = frame + 0x68;
        let flag = frame + 0x6c;
        m.sw(rotation + 0x1c, 0);
        m.sw(rotation + 0x18, 0);
        m.sw(rotation + 0x14, 0);
        sdk::set_trans_matrix(m, rotation);
        m.sh(point, 0);
        m.sh(point + 2, 0);
        m.sh(point + 4, 0x100);
        let view_angles = m.lw(view + 0x10);
        shared::f_800306ac(m, [view_angles, rotation, 0, 0]);
        sdk::set_rot_matrix(m, rotation);
        let depth = sdk::rot_trans_pers(m, point, screen, depth_cue, flag);
        // lwl/lwr + swl/swr: a word copy (both addresses are aligned).
        let xy = m.lw(screen);
        m.sw(STATE + 0x14, xy);
        m.sw(STATE + 0x18, depth);
        let vector = m.lw(view + 0xc);
        let x = m.lhu(vector).wrapping_neg();
        m.sh(angles, x);
        let vector = m.lw(view + 0xc);
        let y = m.lhu(vector + 2).wrapping_neg();
        m.sh(angles + 4, 0);
        m.sh(angles + 2, y);
        if x != 0 || y != 0 {
            shared::f_800306ac(m, [angles, offset, 0, 0]);
            sdk::mul_matrix2(m, offset, rotation);
            sdk::set_rot_matrix(m, rotation);
            let depth = sdk::rot_trans_pers(m, point, screen, depth_cue, flag);
            if depth as i32 > 0 {
                let prim = next_line(m);
                copy_color(m, prim);
                let x = m.lhu(screen);
                m.sh(prim + 8, x.wrapping_sub(2));
                let x = m.lhu(screen);
                let pool = m.gw(POOL);
                m.sh(prim + 0xc, x.wrapping_add(2));
                let y = m.lhu(screen + 2);
                m.sh(prim + 0xe, y);
                m.sh(prim + 0xa, y);
                bump(m, pool, LINE_COUNT);
                let prim = prim + 0x10;
                copy_color(m, prim);
                let x = m.lhu(screen);
                m.sh(prim + 0xc, x);
                m.sh(prim + 8, x);
                let y = m.lhu(screen + 2);
                let pool = m.gw(POOL);
                m.sh(prim + 0xa, y.wrapping_sub(3));
                let y = m.lhu(screen + 2);
                m.sh(prim + 0xe, y);
                bump(m, pool, LINE_COUNT);
            }
        }
        m.pop_frame(FRAME);
        0
    }

    // JP-REVIEW(R0159): near function: PAL 800634E8 -> JP 80064CB4, imm:1, 1 instructions differ (whole routine)
    /// 80064CB4: append three half-intensity lines at y = 0x80 (x 0..0x3C,
    /// 0x104..0x122 and 0x13A..0x140) to the line class. Reads no argument
    /// register.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_sight::center_lines_634e8` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn center_lines_634e8(m: &mut Machine) {
        crate::jp::res_p3_hud_sight::center_lines_634e8(m)
    }

    /// 800660F8 (`view`): when the boresight depth (state +0x18) is
    /// positive, place the four corner brackets (prebuilt, already chained
    /// three-point lines at buffer +0x954, +0x974, +0x994, +0x9B4) around
    /// the boresight point at 3/4 of `H * sin / cos` of the angle
    /// `[view+0x20]` (H from ReadGeomScreen) and link the chain into
    /// ordering-table slot `[gameplay+0x10]`.
    pub fn brackets_6492c(m: &mut Machine, view: u32) {
        if m.lw(STATE + 0x18) as i32 <= 0 {
            return;
        }
        let h = sdk::read_geom_screen(m) as i32;
        let angle = m.lw(view + 0x20) & 0x3fff;
        let sin = i32::from(m.lh(SINE + angle * 2));
        let product = h.wrapping_mul(sin);
        let cos = i32::from(m.lh(SINE + (angle + 0x1000) * 2));
        let half = quotient(product, cos).wrapping_mul(3) >> 2;
        let buffer = m.gw(BUFFER);

        // Top left.
        let corner = buffer.wrapping_add(0x954);
        let y = i32::from(m.lh(STATE + 0x16));
        let x = i32::from(m.lh(STATE + 0x14));
        let top = y.wrapping_sub(half);
        m.sh(corner + 0xa, top as u16);
        m.sh(corner + 0x12, top as u16);
        let left = x.wrapping_sub(half);
        m.sh(corner + 8, left.wrapping_add(0x13) as u16);
        let top_end = top.wrapping_add(0x14);
        m.sh(corner + 0x10, left as u16);
        m.sh(corner + 0x18, left as u16);
        m.sh(corner + 0x1a, top_end as u16);
        // Top right.
        let x = i32::from(m.lh(STATE + 0x14));
        let corner = buffer.wrapping_add(0x974);
        m.sh(corner + 0xa, top as u16);
        m.sh(corner + 0x12, top as u16);
        m.sh(corner + 0x1a, top_end as u16);
        let right = x.wrapping_add(half);
        m.sh(corner + 8, right.wrapping_sub(0x13) as u16);
        m.sh(corner + 0x10, right as u16);
        m.sh(corner + 0x18, right as u16);
        // Bottom left.
        let corner = buffer.wrapping_add(0x994);
        let x = i32::from(m.lh(STATE + 0x14));
        let y = i32::from(m.lh(STATE + 0x16));
        let left = x.wrapping_sub(half);
        let bottom = y.wrapping_add(half);
        let bottom_end = bottom.wrapping_sub(0x14);
        m.sh(corner + 8, left.wrapping_add(0x13) as u16);
        m.sh(corner + 0xa, bottom as u16);
        m.sh(corner + 0x10, left as u16);
        m.sh(corner + 0x12, bottom as u16);
        m.sh(corner + 0x18, left as u16);
        m.sh(corner + 0x1a, bottom_end as u16);
        // Bottom right.
        let x = i32::from(m.lh(STATE + 0x14));
        let corner = buffer.wrapping_add(0x9b4);
        m.sh(corner + 0xa, bottom as u16);
        m.sh(corner + 0x12, bottom as u16);
        let right = x.wrapping_add(half);
        m.sh(corner + 8, right.wrapping_sub(0x13) as u16);
        m.sh(corner + 0x10, right as u16);
        m.sh(corner + 0x18, right as u16);
        m.sh(corner + 0x1a, bottom_end as u16);

        // Link the chain: the last bracket takes the slot's link, the slot
        // (pointer reloaded) points at the first.
        let gameplay = gameplay(m);
        let slot = m.lw(gameplay + 0x10);
        let last = buffer.wrapping_add(0x9b4);
        let tag = m.lw(last);
        let link = m.lw(slot);
        m.sw(last, (tag & LENGTH) | (link & LINK));
        let slot = m.lw(gameplay + 0x10);
        let link = m.lw(slot);
        let first = buffer.wrapping_add(0x954);
        m.sw(slot, (link & LENGTH) | (first & LINK));
    }

    // JP-REVIEW(R0160): near function: PAL 800645C0 -> JP 80065D8C, imm:2, 2 instructions differ (whole routine)
    /// 80065D8C: the direction arrow of the current target (slot byte state
    /// +0x0E, 0xFF for none; object `[800C6ED0 + 4 * slot]`). It is drawn
    /// for a live object (`[object] & 1`) whose halfword +0x18 is at least
    /// 0x400 or whose word +0x20 has bit 0x20000; with that bit the screen
    /// record (`[800C67A0 + 4 * slot]`) must exist and lie at least 0x40
    /// pixels (SquareRoot0) from the screen centre (0xA0, 0x80). The target
    /// position in the player's frame (80044F94) gives the arrow direction
    /// (80030D54 of x, y) and length (sine of 80030D54 of the planar
    /// distance and z, >> 9, at least 6; base half-width 11 - length / 2, at
    /// least 3). The triangle is rotated by the direction about a centre
    /// 12 pixels from (0xA0, 0x80) along it (`3 * sin >> 10`, `3 * cos >>
    /// 10`) and appended as a Gouraud quad (bright tip, quarter-intensity
    /// base) to the quad class.
    /// Returns 0 on every path. Reads no argument register.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_sight::target_arrow_645c0` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn target_arrow_645c0(m: &mut Machine) -> u32 {
        crate::jp::res_p3_hud_sight::target_arrow_645c0(m)
    }

    /// 80044F94 (`object`, `point`, `out`): `out` (VECTOR) = the rotation
    /// 800308F0 builds from the angles at `object + 0x10`, applied
    /// (ApplyMatrixLV) to `(point - object) >> 12` per word (arithmetic
    /// shift; both are VECTORs). Returns `out` (ApplyMatrixLV's v0; none of
    /// the five callers 8003AF04, 80045650, 800456B8, 80049770, 80065EB4
    /// uses it).
    pub fn relative_view_4444c(m: &mut Machine, object: u32, point: u32, out: u32) -> u32 {
        const FRAME: u32 = 0x50;
        let frame = m.push_frame(FRAME);
        let rotation = frame + 0x10; // MATRIX
        let offset = frame + 0x30; // VECTOR
        shared::f_800306ac(m, [object.wrapping_add(0x10), rotation, 0, 0]);
        for i in 0..3 {
            let to = m.lw(point.wrapping_add(i * 4));
            let from = m.lw(object.wrapping_add(i * 4));
            m.sw(
                offset + i * 4,
                ((to.wrapping_sub(from) as i32) >> 12) as u32,
            );
        }
        let result = sdk::apply_matrix_lv(m, rotation, offset, out);
        m.pop_frame(FRAME);
        result
    }

    /// Native targets of this group by original address (a0..a2 in, v0
    /// out; `Some(0)` for routines without a result).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        Some(match target {
            0x8006_3f80 => {
                reset_pool_627b4(m);
                0
            }
            0x8006_3fcc => {
                link_pool_62800(m);
                0
            }
            0x8006_4400 => rotation_check_62c34(m, a0),
            0x8006_447c => boresight_62cb0(m, a0),
            0x8006_4cb4 => {
                center_lines_634e8(m);
                0
            }
            0x8006_60f8 => {
                brackets_6492c(m, a0);
                0
            }
            0x8006_5d8c => target_arrow_645c0(m),
            0x8004_4f94 => relative_view_4444c(m, a0, a1, a2),
            _ => return None,
        })
    }
}

pub mod tree_62680_b {
    //! 80064D98..8006658C: the attitude overlays among the callees of the
    //! HUD routine 80063E4C (second half): the 3D pitch ladder with its
    //! two-digit pitch readout (80064D98), the horizon heading ring
    //! (80065598) with its compass labels (800657B0), and the gun pipper of
    //! the locked target (800662B4). Each routine loads a GTE rotation built
    //! by the shared angle-to-matrix routines (zero translation), projects
    //! points with RotTransPers/RotTransPers3 and appends LINE_F2, POLY_FT4
    //! and sprite packets to the HUD packet pool `[gp+0x504]`, counting them
    //! in the block at `[gp+0x500]`.
    //!
    //! RotTransPers3 stack arguments and the glyph stack arguments are also
    //! stored into the guest frame at `frame + 0x10..`, as the original
    //! does: later routines read stale stack words there (the SVECTOR pad
    //! halfwords of 800657B0 overlay 80064D98's argument words), and those
    //! pads reach the raw GTE VZ registers.
    use super::*;
    use crate::{GP, sdk, shared};

    /// 800104AC: sine table, 0x4000 steps per turn with 1.0 = 0x4000; the
    /// cosine of step `a` is entry `a + 0x1000`.
    const SINE: u32 = 0x8001_04ac;
    /// 800C6D60: HUD state: +8..+0xA ladder/ring RGB, +0xB..+0xD label and
    /// pipper RGB, +0xE locked target slot, +0x11 readout glyph color.
    const HUD: u32 = 0x800c_6d60;
    /// 800B2C38: two letter indices per compass direction (N, NE, E, ...).
    const LABEL_LETTERS: u32 = 0x800b_2c38;
    /// `gp+0x260` (800BCDD4): per letter index the texture u and width bytes.
    const LETTER_GLYPHS: u32 = GP + 0x260;
    /// 800C6ED0: 64 pointers to the per-slot target records (+0 status word,
    /// bit 0x20 shows the pipper; +4 distance).
    const TARGET_RECORDS: u32 = 0x800c_6ed0;
    /// 800C6EC8: the divisor turning `distance << 5` into the pipper's frame
    /// delay (bullet speed).
    const BULLET_SPEED: u32 = 0x800c_6ec8;
    /// 800C79C8: ring of 32 predicted bullet positions (x, y, z words,
    /// 16-byte stride).
    const BULLET_TRAIL: u32 = 0x800c_79c8;
    /// `gp+0x4FC`: newest bullet trail entry (byte).
    const TRAIL_HEAD: u32 = GP + 0x4fc;
    /// `gp+0x4FD`: the trail entry the pipper shows (byte).
    const TRAIL_SHOWN: u32 = GP + 0x4fd;
    /// `gp+0x500`: pointer to the HUD packet counters.
    const COUNTS: u32 = 0x500;
    /// `gp+0x504`: the HUD packet pool (+0x1F8 POLY_FT4, 40 bytes each;
    /// +0x3D8 LINE_F2, 16; +0x8A0 glyph sprites, 20; +0x940 the pipper
    /// sprite).
    const POOL: u32 = 0x504;
    /// Counter offsets in `[gp+0x500]`.
    const POLY_COUNT: u32 = 0x10;
    const LINE_COUNT: u32 = 0x14;
    const GLYPH_COUNT: u32 = 0x20;
    const PIPPER_COUNT: u32 = 0x24;

    /// Scratchpad SVECTORs of a ladder rung half: inner end, outer end and
    /// end tick. 1F800018 holds the attitude angles, then the current rung
    /// angle; 1F800020 receives the projected end tick.
    const RUNG: [u32; 3] = [0x1f80_0000, 0x1f80_0008, 0x1f80_0010];
    const ANGLES: u32 = 0x1f80_0018;
    const RUNG_TICK: u32 = 0x1f80_0020;
    /// Scratchpad SVECTOR of the heading ring point, and its current and
    /// previous screen positions.
    const RING_POINT: u32 = 0x1f80_0000;
    const RING_CURRENT: u32 = 0x1f80_0018;
    const RING_PREVIOUS: u32 = 0x1f80_0024;

    /// Sine table entry of step `angle` (one turn = 0x4000).
    fn sin(m: &mut Machine, angle: u32) -> i32 {
        i32::from(m.lh(SINE + ((angle & 0x3fff) << 1)))
    }

    /// Cosine of step `angle` (sine table entry `angle + 0x1000`).
    fn cos(m: &mut Machine, angle: u32) -> i32 {
        i32::from(m.lh(SINE + (((angle & 0x3fff) + 0x1000) << 1)))
    }

    /// Next free packet of a pool array: `[gp+0x504] + base + size *
    /// [[gp+0x500] + counter]`.
    fn next_packet(m: &mut Machine, counter: u32, base: u32, size: u32) -> u32 {
        let counts = m.gw(COUNTS);
        let used = m.lw(counts.wrapping_add(counter));
        let pool = m.gw(POOL);
        pool.wrapping_add(used.wrapping_mul(size).wrapping_add(base))
    }

    /// `[[gp+0x500] + counter] += 1`; returns the new count.
    fn count_packet(m: &mut Machine, counter: u32) -> u32 {
        let counts = m.gw(COUNTS);
        let used = m.lw(counts.wrapping_add(counter)).wrapping_add(1);
        m.sw(counts.wrapping_add(counter), used);
        used
    }

    /// The caller's outgoing stack-argument words at `frame + 0x10..`.
    fn stack_args(m: &mut Machine, frame: u32, words: &[u32]) {
        for (i, &word) in words.iter().enumerate() {
            m.sw(frame + 0x10 + i as u32 * 4, word);
        }
    }

    /// RotTransPers3 as called with stack arguments sxy1, sxy2, p, flag.
    fn project3(m: &mut Machine, frame: u32, vectors: [u32; 3], sxy: [u32; 3], p: u32, flag: u32) {
        stack_args(m, frame, &[sxy[1], sxy[2], p, flag]);
        sdk::rot_trans_pers3(m, vectors, sxy, p, flag);
    }

    /// Copy the three color bytes at `rgb` into packet bytes +4..+6.
    fn copy_rgb(m: &mut Machine, rgb: u32, packet: u32) {
        for i in 0..3 {
            let c = m.lbu(rgb + i);
            m.sb(packet.wrapping_add(4 + i), c);
        }
    }

    /// One rung half of [`pitch_ladder_635cc`]: RotTransPers3 of the three
    /// [`RUNG`] vectors into the line's xy0/xy1 and the end tick into
    /// 1F800020 (p and flag in the frame at +0x70/+0x74). True when it
    /// projected (FLAG bit 31 clear).
    fn project_rung(m: &mut Machine, frame: u32, line: u32) -> bool {
        let sxy = [line.wrapping_add(8), line.wrapping_add(0xc), RUNG_TICK];
        project3(m, frame, RUNG, sxy, frame + 0x70, frame + 0x74);
        m.lw(frame + 0x74) & 0x8000_0000 == 0
    }

    /// 80064D98: the pitch ladder. The rotation is the camera matrix
    /// (800308F0 of the angles at `[view+0x10]`) times the attitude matrix
    /// (800308F0 of pitch `[[view+8]]`, 0 and roll `[[view+8]+4]`), with no
    /// translation. Nine rungs k = -4..4 every 227 steps (5 degrees) from
    /// the multiple of 227 at the pitch: each a left and a mirrored right
    /// LINE_F2 from (inner, -sin >> 4, cos >> 4) to (outer, ...) of the rung
    /// angle, colored `[HUD+8..]` * min(16 * (5 - |k|)^2, 0xFF) / 256. Inner
    /// and outer x are -100 - t/4 and -100 - t/2 for the horizon rung (angle
    /// 0; t = max(|pitch|, 0x1BD)), -200 - d/16 and -200 - d/8 for the
    /// others (d = |rung - pitch|); an end tick 0x14 inside the outer end is
    /// projected too. Two marks at the current pitch follow in full color,
    /// and when both project the pitch readout [`pitch_readout_635cc`] sits
    /// at their end ticks. `a1` (0 from 80063E4C) is not read; the original
    /// returns 0.
    pub fn pitch_ladder_635cc(m: &mut Machine, view: u32) {
        let frame = m.push_frame(0xb8);
        let attitude = m.lw(view.wrapping_add(8));
        let pitch = i32::from(m.lh(attitude));
        m.sh(ANGLES + 2, 0);
        m.sh(ANGLES, pitch as u16);
        let attitude = m.lw(view.wrapping_add(8));
        let roll = m.lhu(attitude.wrapping_add(4));
        m.sh(ANGLES + 4, roll);
        shared::f_800306ac(m, [ANGLES, frame + 0x20, 0, 0]);
        let camera = m.lw(view.wrapping_add(0x10));
        shared::f_800306ac(m, [camera, frame + 0x50, 0, 0]);
        sdk::mul_matrix2(m, frame + 0x50, frame + 0x20);
        sdk::set_rot_matrix(m, frame + 0x20);
        m.sw(frame + 0x3c, 0);
        m.sw(frame + 0x38, 0);
        m.sw(frame + 0x34, 0);
        sdk::set_trans_matrix(m, frame + 0x20);

        let mut line = next_packet(m, LINE_COUNT, 0x3d8, 16);
        // The original divides by 227 with the multiply 0x24168E19 >> 37,
        // truncating toward zero like `/`.
        m.sh(ANGLES, ((pitch / 227 - 4) * 227) as u16);
        for k in -4i32..5 {
            let fade = 5 - k.abs();
            let level = (16 * fade * fade).min(0xff) as u32;
            let rgb: [u8; 3] = std::array::from_fn(|i| {
                ((u32::from(m.lbu(HUD + 8 + i as u32)) * level) >> 8) as u8
            });
            let rung = u32::from(m.lhu(ANGLES));
            let y = (sin(m, rung).wrapping_neg() >> 4) as u16;
            let z = (cos(m, rung) >> 4) as u16;
            for vector in [RUNG[2], RUNG[1], RUNG[0]] {
                m.sh(vector + 2, y);
            }
            for vector in [RUNG[2], RUNG[1], RUNG[0]] {
                m.sh(vector + 4, z);
            }
            let rung = i32::from(m.lh(ANGLES));
            let (inner, outer) = if rung == 0 {
                let tilt = pitch.abs().max(0x1bd);
                (-100 - (tilt >> 2), -100 - (tilt >> 1))
            } else {
                let offset = (rung - pitch).abs();
                (-200 - (offset >> 4), -200 - (offset >> 3))
            };
            m.sh(RUNG[0], inner as u16);
            m.sh(RUNG[1], outer as u16);
            let tick = m.lhu(RUNG[1]).wrapping_add(0x14);
            m.sh(RUNG[2], tick);
            for half in 0..2 {
                if half == 1 {
                    // The right half mirrors x (the end tick again 0x14
                    // toward the center).
                    let inner = m.lhu(RUNG[0]).wrapping_neg();
                    let outer = m.lhu(RUNG[1]).wrapping_neg();
                    m.sh(RUNG[1], outer);
                    m.sh(RUNG[0], inner);
                    m.sh(RUNG[2], outer.wrapping_sub(0x14));
                }
                if project_rung(m, frame, line) {
                    for (i, c) in rgb.into_iter().enumerate() {
                        m.sb(line.wrapping_add(4 + i as u32), c);
                    }
                    count_packet(m, LINE_COUNT);
                    line = line.wrapping_add(16);
                }
            }
            let next = m.lhu(ANGLES).wrapping_add(227);
            m.sh(ANGLES, next);
        }

        // The current-pitch marks: x -0xD0..-0xC0, end tick at -0x98.
        m.sh(ANGLES, pitch as u16);
        m.sh(RUNG[0], (-0xd0i16) as u16);
        m.sh(RUNG[1], (-0xc0i16) as u16);
        m.sh(RUNG[2], (-0x98i16) as u16);
        let rung = u32::from(m.lhu(ANGLES));
        let y = (sin(m, rung).wrapping_neg() >> 4) as u16;
        m.sh(RUNG[0] + 2, y);
        let z = (cos(m, rung) >> 4) as u16;
        m.sh(RUNG[0] + 4, z);
        let z = m.lhu(RUNG[0] + 4);
        m.sh(RUNG[1] + 2, y);
        m.sh(RUNG[2] + 2, y);
        m.sh(RUNG[1] + 4, z);
        m.sh(RUNG[2] + 4, z);
        if project_rung(m, frame, line) {
            copy_rgb(m, HUD + 8, line);
            count_packet(m, LINE_COUNT);
            line = line.wrapping_add(16);
        }
        let left_flag = m.lw(frame + 0x74);
        let left = m.lw(RUNG_TICK);
        for vector in RUNG {
            let x = m.lhu(vector).wrapping_neg();
            m.sh(vector, x);
        }
        if project_rung(m, frame, line) {
            copy_rgb(m, HUD + 8, line);
            count_packet(m, LINE_COUNT);
        }
        let flags = left_flag | m.lw(frame + 0x74);
        let right = m.lw(RUNG_TICK);
        if flags & 0x8000_0000 == 0 {
            pitch_readout_635cc(m, frame, pitch, left, right);
        }
        m.pop_frame(0xb8);
    }

    /// One readout glyph of [`pitch_readout_635cc`] at screen word `at`
    /// (x, y halfwords) moved by (`dx`, -3), into `sprite`.
    fn readout_glyph(m: &mut Machine, frame: u32, code: u32, at: u32, dx: i32, sprite: u32) {
        let x = i32::from(at as u16 as i16).wrapping_add(dx);
        let y = i32::from((at >> 16) as u16 as i16).wrapping_sub(3);
        m.sw(frame + 0x10, sprite);
        let color = u32::from(m.lbu(HUD + 0x11));
        m.sw(frame + 0x14, color);
        super::glyph_3b00c(m, code, x as u32, y as u32, 3, sprite, color);
        count_packet(m, GLYPH_COUNT);
    }

    /// The pitch readout of [`pitch_ladder_635cc`] (its tail from
    /// 80065348): degrees = `pitch` * 360 / 0x4000 (truncated) as font-2
    /// glyph sprites ([`super::glyph_3b00c`], color `[HUD+0x11]`) beside
    /// both end ticks `left` and `right` (screen words): when negative a
    /// minus glyph at x - 9, then the tens digit (modulo 10) at x - 4 and
    /// the units at x + 1, all at y - 3, each pair left then right. `frame`
    /// is the 80064D98 frame whose stack-argument words carry the sprite
    /// and color.
    pub fn pitch_readout_635cc(m: &mut Machine, frame: u32, pitch: i32, left: u32, right: u32) {
        let mut sprite = next_packet(m, GLYPH_COUNT, 0x8a0, 20);
        let mut pair = |m: &mut Machine, code: u32, dx: i32| {
            for at in [left, right] {
                readout_glyph(m, frame, code, at, dx, sprite);
                sprite = sprite.wrapping_add(0x14);
            }
        };
        // The original adds 0x3FFF to negative products before `>> 14`.
        let mut degrees = pitch.wrapping_mul(360) / 0x4000;
        if degrees < 0 {
            pair(m, u32::MAX, -9);
            degrees = -degrees;
        }
        let tens = degrees / 10;
        pair(m, (tens % 10) as u32, -4);
        pair(m, (degrees - tens * 10) as u32, 1);
    }

    /// 80065598: the horizon heading ring. The rotation is 800308F0 of the
    /// camera angles `[[gameplay+0x28]+0x14]`, with no translation. Points
    /// (sin >> 3, h, cos >> 3) every 0x200 steps around the turn, at height
    /// h = 3 * (sin(-pitch) >> 2) / 4 (pitch `[[view+8]]`, truncated), are
    /// projected in turn (the first, step 0, as (0, h, 0x800)); each that
    /// projects (FLAG bit 31 clear) is joined to the previous screen point by
    /// a LINE_F2 in `[HUD+8..]` and gets its compass label from
    /// [`ring_label_63fe4`]. `a1` (0 from 80063E4C) is not read; the
    /// original returns 0.
    pub fn horizon_ring_63dcc(m: &mut Machine, view: u32) {
        let frame = m.push_frame(0x58);
        let gameplay = super::gameplay(m);
        let camera = m.lw(gameplay.wrapping_add(0x28));
        shared::f_800306ac(m, [camera.wrapping_add(0x14), frame + 0x10, 0, 0]);
        sdk::set_rot_matrix(m, frame + 0x10);
        m.sw(frame + 0x2c, 0);
        m.sw(frame + 0x28, 0);
        m.sw(frame + 0x24, 0);
        sdk::set_trans_matrix(m, frame + 0x10);
        let attitude = m.lw(view.wrapping_add(8));
        let pitch = i32::from(m.lh(attitude));
        let height = ((sin(m, pitch.wrapping_neg() as u32) >> 2) * 3) / 4;
        m.sh(RING_POINT + 2, height as u16);
        m.sh(RING_POINT, 0);
        m.sh(RING_POINT + 4, 0x800);
        sdk::rot_trans_pers(m, RING_POINT, RING_PREVIOUS, frame + 0x30, frame + 0x34);
        for step in (0x200..=0x4000u32).step_by(0x200) {
            let x = sin(m, step) >> 3;
            let z = cos(m, step) >> 3;
            m.sh(RING_POINT, x as u16);
            m.sh(RING_POINT + 4, z as u16);
            sdk::rot_trans_pers(m, RING_POINT, RING_CURRENT, frame + 0x30, frame + 0x34);
            let shown = (m.lw(frame + 0x34) as i32) >= 0;
            if shown {
                let line = next_packet(m, LINE_COUNT, 0x3d8, 16);
                copy_rgb(m, HUD + 8, line);
                let previous = m.lw(RING_PREVIOUS);
                m.sw(line.wrapping_add(8), previous);
                let current = m.lw(RING_CURRENT);
                m.sw(line.wrapping_add(0xc), current);
                count_packet(m, LINE_COUNT);
            }
            let current = m.lw(RING_CURRENT);
            m.sw(RING_PREVIOUS, current);
            if shown {
                ring_label_63fe4(m, view, step);
            }
        }
        m.pop_frame(0x58);
    }

    /// One compass label POLY_FT4 of [`ring_label_63fe4`]: corners from the
    /// screen words at `frame + xy[..]`, tpage 0x36, clut 0x7F98, color
    /// `[HUD+0xB..]` / 4, letter `[letter]`'s glyph columns (u, u + width)
    /// over rows 0x28..0x32.
    fn label_poly(m: &mut Machine, poly: u32, frame: u32, xy: [u32; 4], letter: u32) {
        m.sh(poly.wrapping_add(0x16), 0x36);
        m.sh(poly.wrapping_add(0xe), 0x7f98);
        for i in 0..3 {
            let c = m.lbu(HUD + 0xb + i) >> 2;
            m.sb(poly.wrapping_add(4 + i), c);
        }
        for (corner, offset) in xy.into_iter().enumerate() {
            let screen = m.lw(frame + offset);
            m.sw(poly.wrapping_add(8 + corner as u32 * 8), screen);
        }
        let glyph = LETTER_GLYPHS.wrapping_add(u32::from(m.lbu(letter)) << 1);
        for (corner, v) in [(0u32, 0x28u8), (1, 0x28), (2, 0x32), (3, 0x32)] {
            let mut u = m.lbu(glyph);
            if corner & 1 != 0 {
                u = u.wrapping_add(m.lbu(glyph + 1));
            }
            let at = poly.wrapping_add(0xc + corner * 8);
            if corner == 3 {
                m.sb(at, u);
                m.sb(at + 1, v);
            } else {
                m.sb(at + 1, v);
                m.sb(at, u);
            }
        }
    }

    /// 800657B0: the compass label of the heading ring at step `angle`
    /// (masked to 0x3FFF; steps that are not multiples of 0x800 return -1).
    /// The letters of direction `angle >> 11` (`[0x800B2C38 + 2 * dir]`)
    /// stand on the ring between the ring height `3 * (sin(-pitch) >> 2) >>
    /// 2` (pitch `[[view+8]]`) and 0x90 above it over three columns at
    /// `angle` - 0xC0, `angle`, + 0xC0 for the diagonal directions (two
    /// POLY_FT4), or 0xC4 above over `angle` -/+ 0x80 for the cardinal ones
    /// (one POLY_FT4); points are (sin >> 3, y, cos >> 3) through
    /// RotTransPers3/RotTransPers with the current GTE matrices. Returns the
    /// new POLY_FT4 count.
    pub fn ring_label_63fe4(m: &mut Machine, view: u32, angle: u32) -> u32 {
        let angle = angle & 0x3fff;
        if angle & 0x7ff != 0 {
            return u32::MAX;
        }
        let frame = m.push_frame(0x80);
        let letters = LABEL_LETTERS + (angle >> 11) * 2;
        let attitude = m.lw(view.wrapping_add(8));
        let pitch = i32::from(m.lh(attitude));
        let base = ((sin(m, pitch.wrapping_neg() as u32) >> 2) * 3) >> 2;
        let (p, flag) = (frame + 0x68, frame + 0x6c);
        if angle & 0x800 != 0 {
            let top = base - 0x90;
            for (k, column) in [angle.wrapping_sub(0xc0), angle, angle + 0xc0]
                .into_iter()
                .enumerate()
            {
                let x = (sin(m, column) >> 3) as u16;
                let z = (cos(m, column) >> 3) as u16;
                let upper = frame + 0x20 + k as u32 * 8;
                let lower = upper + 0x18;
                m.sh(upper, x);
                m.sh(upper + 2, top as u16);
                m.sh(upper + 4, z);
                m.sh(lower, x);
                m.sh(lower + 2, base as u16);
                m.sh(lower + 4, z);
            }
            let upper = [frame + 0x20, frame + 0x28, frame + 0x30];
            let lower = [frame + 0x38, frame + 0x40, frame + 0x48];
            project3(
                m,
                frame,
                upper,
                [frame + 0x50, frame + 0x54, frame + 0x58],
                p,
                flag,
            );
            project3(
                m,
                frame,
                lower,
                [frame + 0x5c, frame + 0x60, frame + 0x64],
                p,
                flag,
            );
            let poly = next_packet(m, POLY_COUNT, 0x1f8, 40);
            label_poly(m, poly, frame, [0x50, 0x54, 0x5c, 0x60], letters);
            count_packet(m, POLY_COUNT);
            label_poly(
                m,
                poly.wrapping_add(0x28),
                frame,
                [0x54, 0x58, 0x60, 0x64],
                letters + 1,
            );
        } else {
            let top = base - 0xc4;
            for (k, column) in [angle.wrapping_sub(0x80), angle + 0x80]
                .into_iter()
                .enumerate()
            {
                let x = (sin(m, column) >> 3) as u16;
                let z = (cos(m, column) >> 3) as u16;
                let upper = frame + 0x20 + k as u32 * 8;
                let lower = upper + 0x10;
                m.sh(upper, x);
                m.sh(upper + 2, top as u16);
                m.sh(upper + 4, z);
                m.sh(lower, x);
                m.sh(lower + 2, base as u16);
                m.sh(lower + 4, z);
            }
            let vectors = [frame + 0x20, frame + 0x28, frame + 0x30];
            project3(
                m,
                frame,
                vectors,
                [frame + 0x50, frame + 0x54, frame + 0x58],
                p,
                flag,
            );
            sdk::rot_trans_pers(m, frame + 0x38, frame + 0x5c, p, flag);
            let poly = next_packet(m, POLY_COUNT, 0x1f8, 40);
            label_poly(m, poly, frame, [0x50, 0x54, 0x58, 0x5c], letters);
        }
        let count = count_packet(m, POLY_COUNT);
        m.pop_frame(0x80);
        count
    }

    /// 800662B4: the gun pipper. Takes no arguments (80063E4C calls it with
    /// stale a0..a3; none is read). The player entity's angle matrix
    /// (80030778 of entity+0x18) turns (0, 0, 0x1580000) into a forward
    /// offset (ApplyRotMatrixLV); while context word 0 bit 29 is clear the
    /// trail head `[gp+0x4FC]` advances (mod 32) and its entry receives
    /// position + offset + (velocity << 5) (entity +8.. and +0x38..). With a
    /// locked target record (slot `[HUD+0xE]`; none returns 1) the delay
    /// `min((distance << 5) / [0x800C6EC8], 31)` selects the trail entry
    /// shown (`[gp+0x4FD]`), which is projected camera-relative (`>> 12`)
    /// through 800308F0 of the camera angles; [`pipper_sprite_64ae8`] then
    /// draws it. Returns the original v0 (1, 0, or the new pipper count).
    pub fn gun_pipper_64ae8(m: &mut Machine) -> u32 {
        let entities = super::entities(m);
        let frame = m.push_frame(0x80);
        let player = m.lw(entities);
        shared::f_80030534(m, [player.wrapping_add(0x18), frame + 0x10, 0, 0]);
        sdk::set_rot_matrix(m, frame + 0x10);
        m.sw(frame + 0x30, 0);
        m.sw(frame + 0x34, 0);
        m.sw(frame + 0x38, 0x158_0000);
        sdk::apply_rot_matrix_lv(m, frame + 0x30, frame + 0x40);
        let context = super::context(m);
        if !m.lw(context) & 0x2000_0000 != 0 {
            let head = m.lbu(TRAIL_HEAD).wrapping_add(1);
            m.sb(TRAIL_HEAD, head);
        }
        if m.lbu(TRAIL_HEAD) >= 0x20 {
            m.sb(TRAIL_HEAD, 0);
        }
        let head = u32::from(m.lbu(TRAIL_HEAD));
        let entry = BULLET_TRAIL + head * 16;
        for k in 0..3 {
            let position = m.lw(player.wrapping_add(8 + k * 4));
            let offset = m.lw(frame + 0x40 + k * 4);
            let velocity = m.lw(player.wrapping_add(0x38 + k * 4));
            m.sw(
                entry + k * 4,
                position.wrapping_add(offset).wrapping_add(velocity << 5),
            );
        }
        let slot = u32::from(m.lbu(HUD + 0xe));
        let record = m.lw(TARGET_RECORDS + slot * 4);
        if record == 0 {
            m.pop_frame(0x80);
            return 1;
        }
        let distance = m.lw(record.wrapping_add(4)) << 5;
        let speed = m.lw(BULLET_SPEED);
        // `break 7`/`break 6` guard the division in the original (unobserved).
        let mut delay = super::mips_div(distance as i32, speed as i32);
        if delay >= 0x20 {
            delay = 0x1f;
        }
        let half = delay >> 1;
        let clut_step = ((half & 3) + ((half & 0xc) << 4)) as u32;
        let gameplay = super::gameplay(m);
        m.sb(TRAIL_SHOWN, (head.wrapping_sub(delay as u32) & 0x1f) as u8);
        let camera = m.lw(gameplay.wrapping_add(0x28));
        shared::f_800306ac(m, [camera.wrapping_add(0x14), frame + 0x10, 0, 0]);
        let shown = u32::from(m.lbu(TRAIL_SHOWN));
        let gameplay = super::gameplay(m);
        m.sw(frame + 0x2c, 0);
        m.sw(frame + 0x28, 0);
        m.sw(frame + 0x24, 0);
        let entry = BULLET_TRAIL + shown * 16;
        for k in 0..3 {
            let camera = m.lw(gameplay.wrapping_add(0x28));
            let position = m.lw(entry + k * 4);
            let eye = m.lw(camera.wrapping_add(4 + k * 4));
            let relative = (position.wrapping_sub(eye) as i32) >> 12;
            m.sh(frame + 0x50 + k * 2, relative as u16);
        }
        sdk::set_rot_matrix(m, frame + 0x10);
        sdk::set_trans_matrix(m, frame + 0x10);
        sdk::rot_trans_pers(m, frame + 0x50, frame + 0x58, frame + 0x60, frame + 0x64);
        let flag = m.lw(frame + 0x64);
        let screen = m.lw(frame + 0x58);
        let v0 = pipper_sprite_64ae8(m, record, clut_step, screen, flag);
        m.pop_frame(0x80);
        v0
    }

    /// The pipper sprite of [`gun_pipper_64ae8`] (its tail from 800664F4):
    /// nothing (v0 0) when the projection failed (`flag` bit 31) or target
    /// `record` +0 lacks bit 0x20; otherwise the sprite at pool +0x940 gets
    /// color `[HUD+0xB..]`, clut 0x7E18 + `clut_step` and position
    /// `screen` - (10, 10), and the pipper count is incremented and
    /// returned.
    pub fn pipper_sprite_64ae8(
        m: &mut Machine,
        record: u32,
        clut_step: u32,
        screen: u32,
        flag: u32,
    ) -> u32 {
        if (flag as i32) < 0 || m.lw(record) & 0x20 == 0 {
            return 0;
        }
        let sprite = m.gw(POOL).wrapping_add(0x940);
        let red = m.lbu(HUD + 0xb);
        m.sb(sprite.wrapping_add(4), red);
        let green = m.lbu(HUD + 0xc);
        m.sb(sprite.wrapping_add(5), green);
        let blue = m.lbu(HUD + 0xd);
        m.sh(
            sprite.wrapping_add(0xe),
            clut_step.wrapping_add(0x7e18) as u16,
        );
        m.sb(sprite.wrapping_add(6), blue);
        m.sh(sprite.wrapping_add(8), (screen as u16).wrapping_sub(10));
        m.sh(
            sprite.wrapping_add(0xa),
            ((screen >> 16) as u16).wrapping_sub(10),
        );
        count_packet(m, PIPPER_COUNT)
    }

    /// Native targets of this group by original address. 80064D98 and
    /// 80065598 take the view block in a0 (a1 unused) and return 0;
    /// 800657B0 takes (view, angle); 800662B4 reads no arguments.
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, _, _] = args;
        match target {
            0x8006_4d98 => {
                pitch_ladder_635cc(m, a0);
                Some(0)
            }
            0x8006_5598 => {
                horizon_ring_63dcc(m, a0);
                Some(0)
            }
            0x8006_57b0 => Some(ring_label_63fe4(m, a0, a1)),
            0x8006_62b4 => Some(gun_pipper_64ae8(m)),
            _ => None,
        }
    }
}

pub mod status {
    //! 8003A2B8: per-frame HUD state before drawing: screen projection of
    //! the target slots, lock-on, panel and message animation, and the
    //! mission fade (`[gp+0x340]` +0x30 active, +0x31 direction, +0x1A
    //! elapsed, +0x32 duration) drawn by 8008FC9C.
    use super::*;

    /// `[gp+0x340]`: mission presentation block.
    const PRESENTATION: u32 = 0x340;

    /// 8003A2B8: run the HUD state routines, then advance an active fade.
    /// The original 0x18-byte frame is allocated so that the frames of its
    /// callees (8003B34C passes stack addresses to the SDK) keep their
    /// original addresses: stale stack words reach GTE registers through
    /// `lwc2`.
    pub fn status_39d84(m: &mut Machine) {
        const FRAME: u32 = 0x18;
        m.push_frame(FRAME);
        state(m);
        m.pop_frame(FRAME);
    }

    fn state(m: &mut Machine) {
        tree_39d84::screen_targets_3abac(m);
        tree_39d84::lockon_3e780(m);
        tree_39d84::panel_40ce8(m);
        tree_39d84::message_41010(m);
        let gameplay = super::gameplay(m);
        if m.lw(gameplay) & 1 != 0 {
            tree_39d84::reset_41a48(m);
        }
        let block = m.gw(PRESENTATION);
        if m.lbu(block + 0x30) != 1 {
            return;
        }
        if tree_39d84::flag_55038(m) != 0 {
            return;
        }
        let block = m.gw(PRESENTATION);
        let level = match m.lbu(block + 0x31) {
            1 => {
                // Fading in: 0x555 * (duration - elapsed) / duration.
                let duration = i32::from(m.lh(block + 0x32));
                let elapsed = i32::from(m.lh(block + 0x1a));
                Some(mips_div(
                    duration.wrapping_sub(elapsed).wrapping_mul(1365),
                    duration,
                ))
            }
            2 => {
                // Fading out: elapsed * 0x1000 / duration.
                let elapsed = i32::from(m.lh(block + 0x1a));
                let duration = i32::from(m.lh(block + 0x32));
                Some(mips_div(elapsed << 12, duration))
            }
            _ => None,
        };
        if let Some(level) = level {
            let gameplay = super::gameplay(m);
            let ot = m.lw(gameplay + 0x10);
            let primitives = m.lw(gameplay + 0x14);
            let next = tree_39d84::fade_8e42c(m, ot, primitives, level as u32);
            let gameplay = super::gameplay(m);
            m.sw(gameplay + 0x14, next);
        }
        let block = m.gw(PRESENTATION);
        let elapsed = m.lhu(block + 0x1a).wrapping_add(1);
        let duration = m.lh(block + 0x32);
        m.sh(block + 0x1a, elapsed);
        if elapsed as i16 >= duration {
            m.sb(block + 0x30, 0);
        }
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, _args: [u32; 4]) -> Option<u32> {
        match target {
            0x8003_a2b8 => status_39d84(m),
            _ => return None,
        }
        Some(0)
    }
}

pub mod compose {
    //! 80033814 -> 8003A0B4: the HUD composition. 80033814 builds the view
    //! block in its stack frame (see [`composition_3357c`]) and 8003A0B4
    //! runs the drawers on it. Every routine here allocates its original
    //! frame so that the frames of the callees that pass stack addresses
    //! keep their original addresses.
    use super::*;
    use crate::sdk;

    /// `[0x800BD098]`: flight state block.
    const FLIGHT_STATE: u32 = 0x800b_d098;
    /// `[0x800BD094]`: camera block (+0x38 vector, +0x48 word).
    const CAMERA: u32 = 0x800b_d094;
    /// 800BD0A0: word copied to view +0x38.
    const VIEW_EXTRA: u32 = 0x800b_d0a0;
    /// 800C6D6E: locked target slot (0xFF: none).
    const LOCKED_SLOT: u32 = 0x800c_6d6e;
    /// 800C6D70: lock-suppress byte set by 8003EF40.
    const LOCK_SUPPRESS: u32 = 0x800c_6d70;

    /// 80033814: build the view block at frame +0x10 and draw the HUD.
    ///
    /// View block: +0x00 `[state+0x24]`; +0x04/+0x08 position and
    /// orientation of the tracked object (the entity selected by
    /// `[gameplay+0x28]` bit 0x100 and its low six bits, else the player
    /// block itself); +0x0C its 8-byte vector (camera +0x38 for entities 0
    /// and 1, else a zeroed local at frame +0x50); +0x10 player block +0xB4;
    /// +0x14 a word (camera +0x48, entity +0x28 or 0); +0x18 speed ratio
    /// `(lh(player+0x4E) << 12) / [state+0x14]` clamped to 0..0x1000;
    /// +0x1C, +0x20, +0x2C, +0x30 state words +0x1C, +0x08, +0x28, +0x2C;
    /// +0x24, +0x28 state +0x04 and +0x20 capped at 6; +0x34 the player
    /// block `[gameplay+0x28]`; +0x38 `[0x800BD0A0]`.
    pub fn composition_3357c(m: &mut Machine) {
        const FRAME: u32 = 0x68;
        let frame = m.push_frame(FRAME);
        let zero = frame + 0x50;
        sdk::memset(m, zero, 0, 8);
        let table = entities(m);
        let player = m.lw(table);
        let state = m.lw(FLIGHT_STATE);
        let speed = i32::from(m.lh(player + 0x4e)) << 12;
        let scale = m.lw(state + 0x14) as i32;
        let ratio = mips_div(speed, scale);
        let gameplay = gameplay(m);
        let own = m.lw(gameplay + 0x28);
        let view = frame + 0x10;
        m.sw(view + 0x18, ratio as u32);
        if ratio >= 0x1001 {
            m.sw(view + 0x18, 0x1000);
        }
        if (m.lw(view + 0x18) as i32) < 0 {
            m.sw(view + 0x18, 0);
        }
        let word = m.lw(state + 0x24);
        m.sw(view, word);
        let a = m.lw(state + 4);
        m.sw(view + 0x24, a);
        let b = m.lw(state + 0x20);
        m.sw(view + 0x28, b);
        if (a as i32) >= 7 {
            m.sw(view + 0x24, 6);
        }
        if (b as i32) >= 7 {
            m.sw(view + 0x28, 6);
        }
        let word = m.lw(state + 0x1c);
        let extra = m.lw(VIEW_EXTRA);
        m.sw(view + 0x1c, word);
        let word = m.lw(state + 8);
        m.sw(view + 0x34, own);
        m.sw(view + 0x10, own.wrapping_add(0xb4));
        m.sw(view + 0x38, extra);
        m.sw(view + 0x20, word);
        let word = m.lw(state + 0x28);
        m.sw(view + 0x2c, word);
        let word = m.lw(state + 0x2c);
        m.sw(view + 0x30, word);
        let tracked = m.lw(own);
        if tracked & 0x100 != 0 {
            let slot = tracked & 0x3f;
            let at = (slot << 2).wrapping_add(table);
            let entity = m.lw(at);
            m.sw(view + 4, entity.wrapping_add(8));
            m.sw(view + 8, entity.wrapping_add(0x18));
            if slot < 2 {
                let camera = m.lw(CAMERA);
                let word = m.lw(camera + 0x48);
                m.sw(view + 0xc, camera.wrapping_add(0x38));
                m.sw(view + 0x14, word);
            } else {
                let entity = m.lw(at);
                let word = m.lw(entity + 0x28);
                m.sw(view + 0xc, zero);
                m.sw(view + 0x14, word);
            }
        } else {
            m.sw(view + 4, own.wrapping_add(4));
            m.sw(view + 8, own.wrapping_add(0x14));
            m.sw(view + 0xc, zero);
            m.sw(view + 0x14, 0);
        }
        compose_39b80(m, view);
        m.pop_frame(FRAME);
    }

    // JP-REVIEW(R0162): exact function, calls differ: PAL 80039B80 -> JP 8003A0B4: instructions equal once addresses are masked, but only 7/8 calls go to the PAL callee's counterpart
    /// 8003A0B4 (`view`): the HUD drawers in their original order; with
    /// gameplay +4 flag 0x800, also the training guidance 80042B20
    /// ([`tree_41ff0`]).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_warn::compose_39b80` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn compose_39b80(m: &mut Machine, view: u32) {
        crate::jp::res_p3_hud_warn::compose_39b80(m, view)
    }

    // JP-REVIEW(R0163): near function: PAL 8003BDF8 -> JP 8003C5B0, imm:1 op:1, 2 instructions differ (whole routine)
    /// 8003C5B0 (`view`): radar, gauges and their packet linking; with
    /// `[context+0x95E]` clear and `[context+0x128]` flag 0x8000, also the
    /// second layer (8003E69C, 8003CB90).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_radar::gauges_3bdf8` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn gauges_3bdf8(m: &mut Machine, view: u32) {
        crate::jp::res_p3_hud_radar::gauges_3bdf8(m, view)
    }

    /// 80063E4C (`view`): the flight instruments. With player flags
    /// (`[[view+0x34]+0x50]`) 0x401: boresight checks, center lines,
    /// boresight marker, pitch ladder (projection distance 0x78), horizon
    /// ring and corner brackets; with gameplay +4 flag 4 the target arrow
    /// and, for a locked live target (800C6D6E), the gun pipper; the impact
    /// marker 80066590 when 800C6D70 is 1; finally the packet pool is linked.
    pub fn overlay_62680(m: &mut Machine, view: u32) {
        const FRAME: u32 = 0x20;
        m.push_frame(FRAME);
        tree_62680_a::reset_pool_627b4(m);
        let player = m.lw(view + 0x34);
        if m.lw(player + 0x50) & 0x401 != 0 {
            tree_62680_a::rotation_check_62c34(m, view);
            tree_62680_a::center_lines_634e8(m);
            let screen = sdk::read_geom_screen(m);
            tree_62680_a::boresight_62cb0(m, view);
            tree_62680_b::pitch_ladder_635cc(m, view);
            sdk::set_geom_screen(m, 0x78);
            tree_62680_b::horizon_ring_63dcc(m, view);
            sdk::set_geom_screen(m, screen);
            tree_62680_a::brackets_6492c(m, view);
        }
        let gameplay = gameplay(m);
        if m.lw(gameplay + 4) & 4 != 0 {
            let slot = m.lbu(LOCKED_SLOT);
            tree_62680_a::target_arrow_645c0(m);
            if slot != 0xff {
                let table = entities(m);
                let entity = m.lw(table.wrapping_add(u32::from(slot) << 2));
                if !m.lw(entity) & 0x40 != 0 {
                    tree_62680_b::gun_pipper_64ae8(m);
                }
            }
            if m.lbu(LOCK_SUPPRESS) == 1 {
                super::extras::impact_marker_64dc4(m, view);
            }
        }
        tree_62680_a::link_pool_62800(m);
        m.pop_frame(FRAME);
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, ..] = args;
        match target {
            0x8003_3814 => composition_3357c(m),
            0x8003_a0b4 => compose_39b80(m, a0),
            0x8003_c5b0 => gauges_3bdf8(m, a0),
            0x8006_3e4c => overlay_62680(m, a0),
            _ => return None,
        }
        Some(0)
    }
}

pub mod stream_draw {
    //! 8002D330, 8002D10C, 8002CE70 (with 8002D560): the three drawing modes
    //! of the stream overlay 8002D3F4 (`stream`). Each takes the ordering
    //! table slot `ot` and the next free primitive and returns the new next
    //! free primitive. The overlay block `[gp+0x314]` holds the mode word
    //! (+0) and up to three 1-based line numbers (+4..+6, 0: none); the
    //! mode's 20-byte record at 800B03AC + 20 * mode describes the line
    //! images: +0 width, +1 height, +2 lines per texture page, +3..+5 RGB, +6
    //! semi-transparency flag, +7 x, +8/+0xA/+0xC y of rows 0..2, +0xE hold
    //! time (8002CD24), +0xF flags (0x10: icon sprite). Line n lies on page
    //! q = (n - 1) / lines-per-page at row r = (n - 1) % lines-per-page:
    //! v = r * height, CLUT 0x4A16 + 0x40 * (q & 7) (one CLUT row per page),
    //! texture page `[block+0x1C]` + (q >> 1). Mode 9 copies line +4 as a VRAM
    //! rectangle; mode 10 draws each line as a POLY_FT4 of double height;
    //! the other modes draw SPRT + DR_TPAGE pairs and, with record flag
    //! 0x10, the icon `gp+0x31A` (from table 800B0488) through 8002D560.
    use super::*;

    /// `[gp+0x314]`: the overlay block (see the `stream` module).
    const BLOCK: u32 = 0x314;
    /// 800B03AC: 20-byte records per block mode (layout in the module doc).
    const MODES: u32 = 0x800b_03ac;
    /// 800B0488: 4-byte icon records (u, v, width, height), right after the
    /// eleven mode records.
    const ICONS: u32 = 0x800b_0488;
    /// 800BCD90 (`gp+0x21C`): index of the frame buffer being drawn (0/1).
    const BUFFER: u32 = 0x800b_cd90;
    /// CLUT of page 0 of the line images.
    const LINE_CLUT: u16 = 0x4a16;
    /// CLUT of the icon sprites.
    const ICON_CLUT: u16 = 0x7fd8;
    /// Draw mode (DR_TPAGE word) of the icon sprites.
    const ICON_TPAGE: u32 = 0xe100_0236;

    /// 800B03AC + 20 * mode: the record of the block's current mode.
    fn record(m: &mut Machine) -> u32 {
        let block = m.gw(BLOCK);
        let mode = m.lw(block);
        mode.wrapping_mul(20).wrapping_add(MODES)
    }

    /// Signed `div` of the 0-based line index by the lines per page:
    /// (page, row). The original guards a zero divisor with `break 7`
    /// (unobserved); the translation keeps the R3000 results there
    /// (quotient as [`mips_div`], remainder = dividend).
    fn page_row(index: i32, per_page: i32) -> (i32, i32) {
        let page = mips_div(index, per_page);
        let row = if per_page == 0 {
            index
        } else {
            index.wrapping_rem(per_page)
        };
        (page, row)
    }

    /// The (page, v) of line `line` (1-based) in `record`: `div` by +2, then
    /// row * height (+1), low word of the `mult`.
    fn line_texel(m: &mut Machine, record: u32, line: u8) -> (i32, u32) {
        let per_page = m.lbu(record + 2);
        let (page, row) = page_row(i32::from(line) - 1, i32::from(per_page));
        let height = m.lbu(record + 1);
        (page, row.wrapping_mul(i32::from(height)) as u32)
    }

    /// Store the record's RGB (+3..+5) at `prim` +4..+6.
    fn record_rgb(m: &mut Machine, record: u32, prim: u32) {
        for i in 0..3 {
            let component = m.lbu(record + 3 + i);
            m.sb(prim + 4 + i, component);
        }
    }

    /// 8002D330 (`ot`, `primitives`) -> next free primitive: mode 9. When
    /// block +4 holds a line n, appends a 6-word VRAM-to-VRAM copy packet
    /// (0x01000000 cache flush, 0x80000000 copy) of the 0x180 x 10 rectangle
    /// at (0x240, 10 * (n - 1)) to (0x36, 0xD6), or (0x36, 0x1D6) when
    /// [`BUFFER`] is 1.
    pub fn copy_line_2cf08(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
        let block = m.gw(BLOCK);
        if m.lbu(block + 4) == 0 {
            return primitives;
        }
        let y: u32 = if m.lw(BUFFER) == 1 { 0x1d6 } else { 0xd6 };
        let prim = primitives;
        m.sw(prim, 0x0500_0000);
        m.sw(prim + 4, 0x0100_0000);
        m.sw(prim + 8, 0x8000_0000);
        let line = m.lbu(block + 4);
        m.sw(prim + 0x14, 0x000a_0180);
        m.sw(prim + 0x10, (y << 16) | 0x36);
        let row = u32::from(line).wrapping_sub(1).wrapping_mul(5) << 17;
        m.sw(prim + 0xc, row | 0x240);
        add_prim(m, ot, prim);
        prim + 0x18
    }

    /// 8002D10C (`ot`, `primitives`) -> next free primitive: mode 10. Each
    /// line of block +4..+6 becomes a textured quad (POLY_FT4, code 0x2C or
    /// 0x2E when record +6 is set) at (x, row y) of size width x
    /// (2 * height - 1), texture (0, v)..(width, v + height).
    pub fn quad_lines_2cce4(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
        let record = record(m);
        let mut prim = primitives;
        for i in 0..3u32 {
            let block = m.gw(BLOCK);
            let line = m.lbu(block.wrapping_add(i) + 4);
            if line == 0 {
                continue;
            }
            let rows = record + 2 * i;
            let (page, v) = line_texel(m, record, line);
            m.sb(prim + 3, 9);
            m.sb(prim + 7, 0x2c);
            let clut = ((page as u32) & 7) << 6;
            let tpage = (page >> 1) as u32;
            let code = if m.lbu(record + 6) != 0 { 0x2e } else { 0x2c };
            m.sb(prim + 7, code);
            record_rgb(m, record, prim);
            let x = u16::from(m.lbu(record + 7));
            m.sh(prim + 8, x);
            let y = m.lhu(rows + 8);
            m.sh(prim + 0xa, y);
            let x = u32::from(m.lbu(record + 7));
            let width = u32::from(m.lbu(record));
            m.sh(prim + 0x10, x.wrapping_add(width) as u16);
            let y = m.lhu(rows + 8);
            m.sh(prim + 0x12, y);
            let x = u16::from(m.lbu(record + 7));
            m.sh(prim + 0x18, x);
            let y = u32::from(m.lhu(rows + 8));
            let height = u32::from(m.lbu(record + 1));
            let bottom = y.wrapping_sub(1).wrapping_add(height << 1);
            m.sh(prim + 0x1a, bottom as u16);
            let x = u32::from(m.lbu(record + 7));
            let width = u32::from(m.lbu(record));
            m.sh(prim + 0x20, x.wrapping_add(width) as u16);
            let y = u32::from(m.lhu(rows + 8));
            let height = u32::from(m.lbu(record + 1));
            m.sb(prim + 0xc, 0);
            m.sb(prim + 0xd, v as u8);
            let bottom = y.wrapping_sub(1).wrapping_add(height << 1);
            m.sh(prim + 0x22, bottom as u16);
            let width = m.lbu(record);
            m.sb(prim + 0x15, v as u8);
            m.sb(prim + 0x1c, 0);
            m.sb(prim + 0x14, width);
            let height = u32::from(m.lbu(record + 1));
            m.sb(prim + 0x1d, height.wrapping_add(v) as u8);
            let width = m.lbu(record);
            m.sb(prim + 0x24, width);
            let height = u32::from(m.lbu(record + 1));
            m.sb(prim + 0x25, height.wrapping_add(v) as u8);
            let block = m.gw(BLOCK);
            m.sh(prim + 0xe, LINE_CLUT.wrapping_add(clut as u16));
            let base = u32::from(m.lhu(block + 0x1c));
            m.sh(prim + 0x16, base.wrapping_add(tpage) as u16);
            add_prim(m, ot, prim);
            prim += 0x28;
        }
        prim
    }

    /// 8002CE70 (`ot`, `primitives`) -> next free primitive: the modes other
    /// than 9 and 10. Mode 0 draws nothing while `[context+0x2C]` is 2 and
    /// gameplay word 0 has bit 0. Each line of block +4..+6 becomes a sprite
    /// (SPRT, code 0x64 or 0x66 when record +6 is set; height 0x8A for row 1
    /// in mode 6) followed by its DR_TPAGE (0xE1000200 | (page & 0x9FF));
    /// each stores 0x4A16 at block +0x1E. With record flag 0x10, at least one
    /// line drawn and icon `gp+0x31A` non-zero, 8002D560 appends icon
    /// `gp+0x31A` - 1.
    pub fn sprite_lines_2ca48(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
        let block = m.gw(BLOCK);
        let mode = m.lw(block);
        let record = mode.wrapping_mul(20).wrapping_add(MODES);
        if mode == 0 {
            let context = context(m);
            if m.lw(context + 0x2c) == 2 {
                let gameplay = gameplay(m);
                if m.lw(gameplay) & 1 != 0 {
                    return primitives;
                }
            }
        }
        let mut prim = primitives;
        let mut drawn: i32 = 0;
        for i in 0..3u32 {
            let block = m.gw(BLOCK);
            let line = m.lbu(block.wrapping_add(i) + 4);
            if line == 0 {
                continue;
            }
            let rows = record + 2 * i;
            let (page, v) = line_texel(m, record, line);
            drawn += 1;
            m.sb(prim + 3, 4);
            m.sb(prim + 7, 0x64);
            let clut = ((page as u32) & 7) << 6;
            let tpage = (page >> 1) as u32;
            let code = if m.lbu(record + 6) != 0 { 0x66 } else { 0x64 };
            m.sb(prim + 7, code);
            record_rgb(m, record, prim);
            let x = u16::from(m.lbu(record + 7));
            m.sh(prim + 8, x);
            let y = m.lhu(rows + 8);
            m.sb(prim + 0xc, 0);
            let block = m.gw(BLOCK);
            m.sh(prim + 0xa, y);
            let width = u16::from(m.lbu(record));
            m.sh(prim + 0x10, width);
            let height = u16::from(m.lbu(record + 1));
            m.sh(prim + 0x12, height);
            if m.lw(block) == 6 && i == 1 {
                m.sh(prim + 0x12, 0x8a);
            }
            let block = m.gw(BLOCK);
            m.sh(block + 0x1e, LINE_CLUT);
            m.sh(prim + 0xe, LINE_CLUT.wrapping_add(clut as u16));
            m.sb(prim + 0xd, v as u8);
            add_prim(m, ot, prim);
            let mode_packet = prim + 0x14;
            m.sb(mode_packet + 3, 1);
            let block = m.gw(BLOCK);
            let base = u32::from(m.lhu(block + 0x1c));
            m.sw(
                mode_packet + 4,
                (base.wrapping_add(tpage) & 0x9ff) | 0xe100_0200,
            );
            add_prim(m, ot, mode_packet);
            prim += 0x1c;
        }
        if m.lbu(record + 0xf) & 0x10 == 0 || drawn <= 0 {
            return prim;
        }
        let icon = m.lbu(crate::GP + 0x31a);
        if icon == 0 {
            return prim;
        }
        icon_sprite_2d138(m, ot, prim, u32::from(icon) - 1)
    }

    /// 8002D560 (`ot`, `primitives`, `icon`) -> next free primitive: icon
    /// `icon` of [`ICONS`] as a sprite (SPRT in the record's RGB and
    /// semi-transparency, CLUT 0x7FD8) at (0x84, 0x38) in mode 0, else at
    /// (record x - 4, row 0 y - 6), followed by DR_TPAGE 0xE1000236.
    pub fn icon_sprite_2d138(m: &mut Machine, ot: u32, primitives: u32, icon: u32) -> u32 {
        let prim = primitives;
        let record = record(m);
        m.sb(prim + 3, 4);
        m.sb(prim + 7, 0x64);
        let code = if m.lbu(record + 6) != 0 { 0x66 } else { 0x64 };
        m.sb(prim + 7, code);
        record_rgb(m, record, prim);
        let block = m.gw(BLOCK);
        let y = if m.lw(block) == 0 {
            m.sh(prim + 8, 0x84);
            0x38
        } else {
            let x = u16::from(m.lbu(record + 7));
            m.sh(prim + 8, x.wrapping_sub(4));
            m.lhu(record + 8).wrapping_sub(6)
        };
        m.sh(prim + 0xa, y);
        let entry = (icon << 2).wrapping_add(ICONS);
        let u = m.lbu(entry);
        m.sb(prim + 0xc, u);
        let v = m.lbu(entry + 1);
        m.sb(prim + 0xd, v);
        let width = u16::from(m.lbu(entry + 2));
        m.sh(prim + 0x10, width);
        let height = u16::from(m.lbu(entry + 3));
        m.sh(prim + 0xe, ICON_CLUT);
        m.sh(prim + 0x12, height);
        add_prim(m, ot, prim);
        let mode_packet = prim + 0x14;
        m.sb(mode_packet + 3, 1);
        m.sw(mode_packet + 4, ICON_TPAGE);
        add_prim(m, ot, mode_packet);
        prim + 0x1c
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        Some(match target {
            0x8002_d330 => copy_line_2cf08(m, a0, a1),
            0x8002_d10c => quad_lines_2cce4(m, a0, a1),
            0x8002_ce70 => sprite_lines_2ca48(m, a0, a1),
            0x8002_d560 => icon_sprite_2d138(m, a0, a1, a2),
            _ => return None,
        })
    }
}

pub mod marker_shapes {
    //! 8003FD48, 800401AC: the target-marker shapes 2 and 3 that 8003F520
    //! draws besides the 8003FA80 box, for entity slot `index` (0..63) in
    //! the box color `color`, from the screen record 800C67A0 (+6 half size
    //! s, +8 x, +A y) and the distance record 800C6ED0 (+0 flags), into this
    //! buffer's marker block `[gp+0x448]` (counts in `[gp+0x444]`). Shape 2
    //! is a pointy-top hexagon: its outline as three-point lines when
    //! distance flag 0x100 is set, otherwise six vertex brackets. Shape 3
    //! (8003F520 also uses it for every marker when `[[gp+0x340]] & 4`) is
    //! four two-point ticks from the box corners towards the centre plus,
    //! with distance flag 0x20, a cross tick on every corner and a centre
    //! dot. Both draw nothing for a slot without a screen or distance record.
    use super::*;

    /// `[gp+0x444]`: marker list (+8 two-point line count, +0xC
    /// three-point line count).
    const LIST: u32 = 0x444;
    /// `[gp+0x448]`: this frame's marker block (two-point lines of 0x10
    /// bytes from +8, three-point lines of 0x18 bytes from +0x408).
    const BLOCK: u32 = 0x448;
    /// 800C67A0: per-entity screen records (+6 half size, +8 x, +A y).
    const RECORDS: u32 = 0x800c_67a0;
    /// 800C6ED0: per-entity distance records (+0 flags).
    const DISTANCES: u32 = 0x800c_6ed0;
    /// Code byte of the three-point lines: GP0 0x4A, a semi-transparent
    /// monochrome polyline.
    const POLYLINE: u32 = 0x4a00_0000;
    /// Code byte of the two-point lines: GP0 0x42, a semi-transparent
    /// monochrome line.
    const LINE: u32 = 0x4200_0000;

    /// Code word `word` at `packet + 4`, then the halfword `stores`
    /// (offset, value) in the original order.
    fn packet(m: &mut Machine, packet: u32, word: u32, stores: &[(u32, u16)]) {
        m.sw(packet.wrapping_add(4), word);
        for &(offset, value) in stores {
            m.sh(packet.wrapping_add(offset), value);
        }
    }

    /// Increment the count word `list + at`.
    fn bump(m: &mut Machine, list: u32, at: u32) {
        let count = m.lw(list.wrapping_add(at));
        m.sw(list.wrapping_add(at), count.wrapping_add(1));
    }

    /// Bracket of 8003FD48 on the top or bottom vertex (`x`, `y`): from
    /// (x - `quarter`, y + `dy`) over the vertex to (x + `quarter`, y + `dy`).
    fn apex(m: &mut Machine, line: u32, word: u32, (x, y): (u16, u16), quarter: u16, dy: u16) {
        let stores = [
            (0x10, x),
            (0xc, x),
            (8, x.wrapping_sub(quarter)),
            (0x10, x.wrapping_add(quarter)),
            (0xe, y),
            (0x12, y.wrapping_add(dy)),
            (0xa, y.wrapping_add(dy)),
        ];
        packet(m, line, word, &stores);
    }

    /// Bracket of 8003FD48 on a side vertex (`x`, `y`): from (x, y +
    /// `along`) up or down the vertical side to the vertex, then along the
    /// slanted edge to (x + `dx`, y + `dy`).
    fn side(
        m: &mut Machine,
        line: u32,
        word: u32,
        (x, y): (u16, u16),
        along: u16,
        (dx, dy): (u16, u16),
    ) {
        let stores = [
            (0x10, x),
            (0xc, x),
            (8, x),
            (0x10, x.wrapping_add(dx)),
            (0x12, y),
            (0xe, y),
            (0xa, y.wrapping_add(along)),
            (0x12, y.wrapping_add(dy)),
        ];
        packet(m, line, word, &stores);
    }

    // JP-REVIEW(R0164): near function: PAL 8003F54C -> JP 8003FD48, imm:1 ins:2 op:1, 4 instructions differ (whole routine)
    /// 8003FD48 (`index`, `color`): marker shape 2, the hexagon with
    /// vertices (x, y - s), (x + s, y - s/2), (x + s, y + s/2), (x, y + s),
    /// (x - s, y + s/2), (x - s, y - s/2) (arithmetic shifts of s). With
    /// distance flag 0x100: its outline as three three-point lines (the
    /// roof, then each side down to the bottom vertex); otherwise six vertex
    /// brackets along the edges (arms s/4 across and s/8 up or down, 9(s/8)/4
    /// along a vertical side). Code word `color | 0x4A000000`.
    pub fn hexagon_3f54c(m: &mut Machine, index: u32, color: u32) {
        let word = color | POLYLINE;
        let slot = index << 2;
        let record = m.lw(RECORDS.wrapping_add(slot));
        if record == 0 {
            return;
        }
        let distance = m.lw(DISTANCES.wrapping_add(slot));
        if distance == 0 {
            return;
        }
        let size_bits = (u32::from(m.lhu(record.wrapping_add(6))) << 16) as i32;
        let x = i32::from(m.lh(record.wrapping_add(8)));
        let y = i32::from(m.lh(record.wrapping_add(0xa)));
        let size = size_bits >> 16;
        let half = size_bits >> 17;
        let quarter = (size_bits >> 18) as u16;
        let eighth = size_bits >> 19;
        let along = ((eighth << 3).wrapping_add(eighth) >> 2) as u16;
        let eighth = eighth as u16;
        // The original's seven stack words; only their low halves are used.
        let left = x.wrapping_sub(size) as u16;
        let centre = x as u16;
        let right = x.wrapping_add(size) as u16;
        let top = y.wrapping_sub(size) as u16;
        let upper = y.wrapping_sub(half) as u16;
        let lower = y.wrapping_add(half) as u16;
        let bottom = y.wrapping_add(size) as u16;
        let full = m.lw(distance) & 0x100 != 0;
        let list = m.gw(LIST);
        let count = m.lw(list.wrapping_add(0xc));
        let block = m.gw(BLOCK);
        let mut line = block.wrapping_add(count.wrapping_mul(0x18).wrapping_add(0x408));
        if full {
            let roof = [
                (8, left),
                (0xc, centre),
                (0x10, right),
                (0xa, upper),
                (0xe, top),
                (0x12, upper),
            ];
            packet(m, line, word, &roof);
            bump(m, list, 0xc);
            line = line.wrapping_add(0x18);
            // Left, then right side down to the bottom vertex.
            for (i, x) in [left, right].into_iter().enumerate() {
                if i > 0 {
                    bump(m, list, 0xc);
                    line = line.wrapping_add(0x18);
                }
                let stores = [
                    (0xc, x),
                    (8, x),
                    (0x10, centre),
                    (0xa, upper),
                    (0xe, lower),
                    (0x12, bottom),
                ];
                packet(m, line, word, &stores);
            }
        } else {
            let (minus_q, minus_e, minus_along) = (
                quarter.wrapping_neg(),
                eighth.wrapping_neg(),
                along.wrapping_neg(),
            );
            apex(m, line, word, (centre, top), quarter, eighth);
            bump(m, list, 0xc);
            line = line.wrapping_add(0x18);
            side(m, line, word, (left, upper), along, (quarter, minus_e));
            bump(m, list, 0xc);
            line = line.wrapping_add(0x18);
            side(m, line, word, (left, lower), minus_along, (quarter, eighth));
            // Here the original takes the next packet from the block and the
            // incremented count instead of advancing the pointer.
            let count = m.lw(list.wrapping_add(0xc)).wrapping_add(1);
            line = block.wrapping_add(count.wrapping_mul(0x18).wrapping_add(0x408));
            m.sw(list.wrapping_add(0xc), count);
            apex(m, line, word, (centre, bottom), quarter, minus_e);
            bump(m, list, 0xc);
            line = line.wrapping_add(0x18);
            side(m, line, word, (right, upper), along, (minus_q, minus_e));
            bump(m, list, 0xc);
            line = line.wrapping_add(0x18);
            side(
                m,
                line,
                word,
                (right, lower),
                minus_along,
                (minus_q, eighth),
            );
        }
        bump(m, list, 0xc);
    }

    // JP-REVIEW(R0165): near function: PAL 8003F9A8 -> JP 800401AC, imm:1 ins:2 op:1, 4 instructions differ (whole routine)
    /// 800401AC (`index`, `color`): marker shape 3 around the box
    /// (x ± s, y ± s): from each corner a two-point tick s/4 towards the
    /// centre; with distance flag 0x20 also a cross tick through each
    /// corner (s/8 each way, perpendicular to the first) and a zero-length
    /// line on the record's (x, y) word as a centre dot. Code word
    /// `color | 0x42000000`.
    pub fn corner_ticks_3f9a8(m: &mut Machine, index: u32, color: u32) {
        let word = color | LINE;
        let slot = index << 2;
        let record = m.lw(RECORDS.wrapping_add(slot));
        if record == 0 {
            return;
        }
        let distance = m.lw(DISTANCES.wrapping_add(slot));
        if distance == 0 {
            return;
        }
        let size_bits = (u32::from(m.lhu(record.wrapping_add(6))) << 16) as i32;
        let x = i32::from(m.lh(record.wrapping_add(8)));
        let y = i32::from(m.lh(record.wrapping_add(0xa)));
        let size = size_bits >> 16;
        // The original's four stack words; only their low halves are used.
        let left = x.wrapping_sub(size) as u16;
        let right = x.wrapping_add(size) as u16;
        let top = y.wrapping_sub(size) as u16;
        let bottom = y.wrapping_add(size) as u16;
        let list = m.gw(LIST);
        let count = m.lw(list.wrapping_add(8));
        let block = m.gw(BLOCK);
        let mut line = block.wrapping_add((count << 4).wrapping_add(8));
        let q = (size_bits >> 18) as u16;
        let minus_q = q.wrapping_neg();
        for (cx, cy, dx, dy) in [
            (left, top, q, q),
            (right, top, minus_q, q),
            (left, bottom, q, minus_q),
            (right, bottom, minus_q, minus_q),
        ] {
            let stores = [
                (8, cx),
                (0xc, cx.wrapping_add(dx)),
                (0xa, cy),
                (0xe, cy.wrapping_add(dy)),
            ];
            packet(m, line, word, &stores);
            bump(m, list, 8);
            line = line.wrapping_add(0x10);
        }
        if m.lw(distance) & 0x20 == 0 {
            return;
        }
        let size_bits = (u32::from(m.lhu(record.wrapping_add(6))) << 16) as i32;
        let e = (size_bits >> 19) as u16;
        let minus_e = e.wrapping_neg();
        for (cx, cy, dy) in [
            (left, top, e),
            (right, top, minus_e),
            (left, bottom, minus_e),
            (right, bottom, e),
        ] {
            let stores = [
                (8, cx.wrapping_sub(e)),
                (0xc, cx.wrapping_add(e)),
                (0xa, cy.wrapping_add(dy)),
                (0xe, cy.wrapping_sub(dy)),
            ];
            packet(m, line, word, &stores);
            bump(m, list, 8);
            line = line.wrapping_add(0x10);
        }
        m.sw(line.wrapping_add(4), word);
        let point = m.lw(record.wrapping_add(8));
        m.sw(line.wrapping_add(8), point);
        let point = m.lw(record.wrapping_add(8));
        m.sw(line.wrapping_add(0xc), point);
        bump(m, list, 8);
    }

    /// Native targets of this group by original address (a0 entity slot,
    /// a1 color; no result).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [index, color, ..] = args;
        match target {
            0x8003_fd48 => hexagon_3f54c(m, index, color),
            0x8004_01ac => corner_ticks_3f9a8(m, index, color),
            _ => return None,
        }
        Some(0)
    }
}

pub mod tree_41ff0 {
    //! 80042B20..80044034: the training guidance overlay, drawn by 8003A0B4
    //! with gameplay +4 flag 0x800. Byte 0 of the course record of mode
    //! `[gp+0x334]` (800B1FC4) selects it: 0/1 an approach path to the guide
    //! point `[view+0x38]` (16 projected gates, turn and pitch arrows and a
    //! deviation cross, 80042D98), 2/3 turn and pitch arrows with a deviation
    //! diamond (8004357C), 4 roll and pitch arrows with a level bar
    //! (80043BF8); other kinds draw nothing. The packets come from the set
    //! `[gp+0x46C]` built by 80042938: per buffer 128 LINE_F3 (0x18 bytes)
    //! from +0, 128 LINE_F2 (0x10 bytes) from +0xC00 and 4 POLY_F3 (0x14
    //! bytes) from +0x1400, counted at set +8, +0xC and +0x10, and linked
    //! into ordering-table slot `[gameplay+0x10]` by 80042C2C. Guidance
    //! messages go to 8004291C on the frames where the frame counter
    //! `[gameplay+0xC]` & 0x7F reaches the cue's phase.
    use super::*;
    use crate::{dispatch::call, sdk};

    /// `[gp+0x46C]`: the guidance packet set: +0/+4 the two buffers, +8
    /// LINE_F3, +0xC LINE_F2 and +0x10 POLY_F3 counts.
    pub const SET: u32 = 0x46c;
    /// `[gp+0x470]`: this frame's buffer of [`SET`].
    pub const BUFFER: u32 = 0x470;
    /// `[gp+0x334]`: training mode number, index into [`COURSES`].
    pub const MODE: u32 = 0x334;
    /// 800B1FC4: 4-byte course records by mode; byte 0 is the guidance kind.
    pub const COURSES: u32 = 0x800b_1fc4;
    /// 800BD02C: current mission number (mission 25 scales the camera offset
    /// of 80042D98 by 1/8 instead of 1/256).
    pub const MISSION: u32 = 0x800b_d02c;
    /// 800B14C8: x of the three points of a gate's left bracket (halfwords).
    pub const LEFT_X: u32 = 0x800b_14c8;
    /// 800B14D0: x of the three points of a gate's right bracket.
    pub const RIGHT_X: u32 = 0x800b_14d0;
    /// 800B14D8: y of the bracket points (before the gate's drop).
    pub const BRACKET_Y: u32 = 0x800b_14d8;
    /// 800B14E0: z of the bracket points (before the gate's depth).
    pub const BRACKET_Z: u32 = 0x800b_14e0;
    /// 800B14E8: x of the four chevron points (their z is the gate's depth).
    pub const CHEVRON_X: u32 = 0x800b_14e8;
    /// 800B14F0: y of the four chevron points.
    pub const CHEVRON_Y: u32 = 0x800b_14f0;
    /// 800B14F8: arrow shapes, 8 signed bytes each: the two base corners of
    /// the head (its tip is the arrow position) and the two ends of the bar,
    /// as (dx, dy) pairs. In the executable arrows 0..3 point left, right, up
    /// and down.
    pub const ARROWS: u32 = 0x800b_14f8;

    /// 8006CB30: pad buttons word (`[[gp+0x520]]`); reads no arguments.
    const PAD_BUTTONS: u32 = 0x8006_cb30;
    /// 8004291C (`id`): request guidance message `id` (gp+0x464 = 1,
    /// gp+0x465 = `id` + 0xC, gp+0x466 = 0); reads only a0.
    const MESSAGE: u32 = 0x8004_291c;
    /// 80030D54 (y, x): 14-bit angle of (x, y).
    const ATAN2: u32 = 0x8003_0d54;
    /// 800A6A08 SetMulMatrix(m0, m1): rotation registers = m0 * m1.
    const SET_MUL_MATRIX: u32 = 0x800a_6a08;

    /// Multiplier of the compiler's division by 100 (with shift 5) and by 50
    /// (shift 4), see [`div_by`].
    const BY_100: i32 = 0x51eb_851f;
    /// Multiplier of the division by 80 (shift 5).
    const BY_80: i32 = 0x6666_6667;
    /// Multiplier of the division by 150 (shift 4).
    const BY_150: i32 = 0x1b4e_81b5;

    fn sra(value: u32, shift: u32) -> u32 {
        ((value as i32) >> shift) as u32
    }

    /// `mult x, magic; mfhi; sra shift; subu (x >> 31)`: the compiler's
    /// truncating signed division by a constant.
    fn div_by(x: i32, magic: i32, shift: u32) -> i32 {
        let high = ((i64::from(x) * i64::from(magic)) >> 32) as i32;
        (high >> shift).wrapping_sub(x >> 31)
    }

    /// 8004291C(`id`) when the frame counter `[gameplay+0xC]` & 0x7F is
    /// `phase`.
    fn message_at(m: &mut Machine, id: u32, phase: u32) {
        let game = gameplay(m);
        if m.lw(game + 0xc) & 0x7f == phase {
            call(m, MESSAGE, [id, 0, 0, 0]);
        }
    }

    /// The next free LINE_F2: buffer + 0xC00 + 0x10 * count.
    fn next_line(m: &mut Machine) -> u32 {
        let set = m.gw(SET);
        let count = m.lw(set + 0xc);
        let buffer = m.gw(BUFFER);
        buffer.wrapping_add((count << 4).wrapping_add(0xc00))
    }

    /// Fill the LINE_F2 at `line` with color `rgb` and (x0, y0)-(x1, y1),
    /// then count it in set +0xC.
    fn line_f2(m: &mut Machine, line: u32, rgb: [u8; 3], xy: [u32; 4]) {
        m.sb(line + 4, rgb[0]);
        m.sb(line + 5, rgb[1]);
        m.sb(line + 6, rgb[2]);
        m.sh(line + 8, xy[0] as u16);
        m.sh(line + 0xa, xy[1] as u16);
        m.sh(line + 0xc, xy[2] as u16);
        m.sh(line + 0xe, xy[3] as u16);
        let set = m.gw(SET);
        let count = m.lw(set + 0xc);
        m.sw(set + 0xc, count.wrapping_add(1));
    }

    /// Fill consecutive LINE_F2 from `line` with (color, (x0, y0, x1, y1))
    /// each, as [`line_f2`].
    fn lines_f2(m: &mut Machine, line: u32, lines: &[([u8; 3], [u32; 4])]) {
        let mut at = line;
        for &(rgb, xy) in lines {
            line_f2(m, at, rgb, xy);
            at = at.wrapping_add(0x10);
        }
    }

    /// Bright and dim green line colors.
    const GREEN: [u8; 3] = [0, 0xff, 0];
    const DIM: [u8; 3] = [0, 0x40, 0];

    // JP-REVIEW(R0166): near function: PAL 80041FF0 -> JP 80042B20, call+:1 imm:1 ins:4 op:1, 6 instructions differ (whole routine)
    /// 80042B20 (`view`): select this frame's buffer (80042BF4), draw the
    /// guidance of the current course kind unless pad buttons 0x240 are
    /// held, and link the packets (80042C2C).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_guide::guidance_41ff0` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn guidance_41ff0(m: &mut Machine, view: u32) {
        crate::jp::res_p3_hud_guide::guidance_41ff0(m, view)
    }

    /// 80042BF4: reset the three packet counts and select the buffer of the
    /// context's current buffer index (`[context+4]`).
    pub fn begin_420b4(m: &mut Machine) {
        let set = m.gw(SET);
        let context = context(m);
        m.sw(set + 0x10, 0);
        m.sw(set + 0xc, 0);
        m.sw(set + 8, 0);
        let index = m.lw(context + 4);
        let buffer = m.lw(set.wrapping_add(index << 2));
        m.set_gw(BUFFER, buffer);
    }

    /// Link the counted packets of `size` bytes from buffer + `base` (count
    /// at set + `count`, reloaded per packet) into `slot`, in order.
    fn link_run(m: &mut Machine, slot: u32, count: u32, base: u32, size: u32) {
        let set = m.gw(SET);
        if (m.lw(set + count) as i32) > 0 {
            let buffer = m.gw(BUFFER);
            let mut prim = buffer.wrapping_add(base);
            let mut index = 0i32;
            loop {
                index += 1;
                add_prim(m, slot, prim);
                prim = prim.wrapping_add(size);
                if index >= m.lw(set + count) as i32 {
                    break;
                }
            }
        }
    }

    /// 80042C2C: link this frame's LINE_F3, LINE_F2 and POLY_F3 packets
    /// into ordering-table slot `[gameplay+0x10]`.
    pub fn link_420ec(m: &mut Machine) {
        let game = gameplay(m);
        let slot = m.lw(game + 0x10);
        link_run(m, slot, 8, 0, 0x18);
        link_run(m, slot, 0xc, 0xc00, 0x10);
        link_run(m, slot, 0x10, 0x1400, 0x14);
    }

    // JP-REVIEW(R0168): near function: PAL 80042258 -> JP 80042D98, del:1 ins:1 op:1, 3 instructions differ (whole routine)
    /// 80042D98 (`view`): approach guidance to the guide point `P =
    /// [view+0x38]` (+0/+4/+8 position, +0x10 angles, +0x18 range,
    /// +0x1C/+0x20/+0x24 offsets, +0x2E heading, +0x34 state).
    ///
    /// The camera sits at `(P - own) >> 12` (own = `[gameplay+0x28]` +4..,
    /// x/z moved by rsin/rcos of angle +0x12 `>> 8`, or `>> 3` in mission 25)
    /// rotated by `[gameplay+0x4C]`, under `[gameplay+0x4C] * rotation(P
    /// angles)`. 16 gates at depth `-0x400 * gate`, dropping `8 * gate^2`,
    /// are projected: left and right brackets (80043E0C) and a chevron
    /// (80043E74), each drawn only when all its points project without a
    /// FLAG error. Turn arrows follow `2 * atan2(+0x1C, -+0x24) + heading`
    /// (left for 0x201..0x1FFF with message 0xD from 0x801, right for
    /// 0x2000..0x3DFF with message 0xC below 0x3800). With state 1 message 3
    /// replaces the pitch cue; otherwise the player pitch (`+0x18`) against
    /// `atan2(+0x20, -+0x24)` gives a down arrow when over 0x200 inside range
    /// 0x801..0x3FFF (message 0xB from 0x801 with +0x24 negative), else an up
    /// arrow (message 0xA) when view +0x30 is at most 0xFFFFF or the error is
    /// below -0x200. Last, the player position in the point's frame (angle y
    /// mirrored to `0x4000 - y`) places a cross at `(-x / 100, (-y - 0x46) /
    /// 100)` (each clamped to ±0x40) from (0xA0, 0x78) over a dim center
    /// cross.
    ///
    /// Frame (0xA8): +0x10 MATRIX, +0x30 SVECTOR, +0x38.. screen words,
    /// +0x48 VECTOR, +0x58 its transform, +0x68 angles, +0x70/+0x74 depth
    /// cue and FLAG, +0x78 the player entity; `view` is saved at +0xA8 (the
    /// caller's argument slot).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_guide::approach_42258` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn approach_42258(m: &mut Machine, view: u32) {
        crate::jp::res_p3_hud_guide::approach_42258(m, view)
    }

    /// Color of a deviation line: blue when `offset` is 0, else green.
    fn deviation(offset: i32) -> [u8; 3] {
        match offset {
            0 => [0, 0, 0xff],
            _ => GREEN,
        }
    }

    /// 8004357C (`view`) -> 0: alignment guidance to the guide point `P =
    /// [view+0x38]`; nothing while pad button 0x80 is held.
    ///
    /// Beyond range 0x2000 (`P+0x18`): turn arrows from the player yaw
    /// (`+0x1A`) plus `atan2(+0x1C >> 12, -+0x24 >> 12)` as a signed 14-bit
    /// angle (left from 0x101, message 0xD from 0x401; right below -0x100,
    /// message 0xC below -0x400) and pitch arrows: up while P+0x20 > 0 and
    /// the player pitch (`+0x18`) is negative (message 0xA below -0x800),
    /// down while P+0x20 < -1 and the pitch is positive (message 0xB from
    /// 0x801). Always: a vertical line at x `0xA0 - (+0x1C >> 12) / 50`, a
    /// horizontal one at y `0xB4 - (+0x20 >> 12) / 80` (both offsets clamped
    /// to ±0x40) and a diamond around (0xA0, 0xB4) grown by `-(+0x24 >> 12)
    /// / 150` (±0x18) over the fixed one; each line is blue when its offset
    /// is 0, else green.
    pub fn alignment_42a3c(m: &mut Machine, view: u32) -> u32 {
        const FRAME: u32 = 0x20;
        let table = entities(m);
        m.push_frame(FRAME);
        let point = m.lw(view + 0x38);
        let player = m.lw(table);
        let buttons = call(m, PAD_BUTTONS, [view, 0, 0, 0]);
        if buttons & 0x80 != 0 {
            m.pop_frame(FRAME);
            return 0;
        }
        if (m.lw(point + 0x18) as i32) >= 0x2001 {
            let y = sra(m.lw(point + 0x1c), 12);
            let x = sra(m.lw(point + 0x24).wrapping_neg(), 12);
            let bearing = call(m, ATAN2, [y, x, 0, 0]);
            let sum = u32::from(m.lhu(player + 0x1a)).wrapping_add(bearing) & 0x3fff;
            let signed = if sum < 0x2000 { sum } else { sum | 0xc000 };
            let turn = i32::from(signed as u16 as i16);
            if turn >= 0x101 {
                if turn >= 0x401 {
                    message_at(m, 0xd, 0);
                }
                arrow_433b4(m, 0, 0x50, 0x78);
            } else if turn < -0x100 {
                if turn < -0x400 {
                    message_at(m, 0xc, 0);
                }
                arrow_433b4(m, 1, 0xf0, 0x78);
            }
            let mut vertical = m.lw(point + 0x20) as i32;
            if vertical > 0 {
                let pitch = m.lh(player + 0x18);
                if pitch < 0 {
                    if pitch < -0x800 {
                        message_at(m, 0xa, 0x40);
                    }
                    arrow_433b4(m, 2, 0xa0, 0x3c);
                }
                vertical = m.lw(point + 0x20) as i32;
            }
            if vertical < -1 {
                let pitch = m.lh(player + 0x18);
                if pitch > 0 {
                    if pitch >= 0x801 {
                        message_at(m, 0xb, 0x40);
                    }
                    arrow_433b4(m, 3, 0xa0, 0xb4);
                }
            }
        }

        let lateral = sra(m.lw(point + 0x1c), 12) as i32;
        let dx = div_by(lateral, BY_100, 4).wrapping_neg().clamp(-0x40, 0x40);
        let line = next_line(m);
        let x = dx.wrapping_add(0xa0) as u32;
        line_f2(m, line, deviation(dx), [x, 0x9c, x, 0xcc]);
        let vertical = sra(m.lw(point + 0x20), 12) as i32;
        let dy = div_by(vertical, BY_80, 5).wrapping_neg().clamp(-0x40, 0x40);
        let y = dy.wrapping_add(0xb4) as u32;
        let second = line.wrapping_add(0x10);
        line_f2(m, second, deviation(dy), [0x88, y, 0xb8, y]);
        let depth = sra(m.lw(point + 0x24), 12) as i32;
        let dz = div_by(depth, BY_150, 4).wrapping_neg().clamp(-0x18, 0x18);
        let rgb = deviation(dz);
        let top = 0x9c_i32.wrapping_sub(dz) as u32;
        let right = dz.wrapping_add(0xb8) as u32;
        let bottom = dz.wrapping_add(0xcc) as u32;
        let left = 0x88_i32.wrapping_sub(dz) as u32;
        let diamond = [
            [0xa0, top, right, 0xb4],
            [right, 0xb4, 0xa0, bottom],
            [0xa0, bottom, left, 0xb4],
            [left, 0xb4, 0xa0, top],
            [0xa0, 0x9c, 0xb8, 0xb4],
            [0xb8, 0xb4, 0xa0, 0xcc],
            [0xa0, 0xcc, 0x88, 0xb4],
            [0x88, 0xb4, 0xa0, 0x9c],
        ];
        lines_f2(m, line.wrapping_add(0x20), &diamond.map(|xy| (rgb, xy)));
        m.pop_frame(FRAME);
        0
    }

    /// 80043BF8: wing-level and pitch guidance for the player entity. A roll
    /// (`+0x1C`) of 0x1C8 or more shows an up arrow at the left and a down
    /// arrow at the right (mirrored below -0x1C7). The pitch error `0x71C -
    /// pitch` (`+0x18`) shows an up arrow from 0xE4 and a down arrow below
    /// -0xE3, then a bar at y `0x78 + (sin(-error) >> 8)` (sine table
    /// 800104AC, high byte), green within ±0xE3 and red otherwise (level
    /// 0x80), over a dim reference line at 0x78 and two fainter ones at 0x73
    /// and 0x7D.
    pub fn attitude_430b0(m: &mut Machine) {
        const FRAME: u32 = 0x20;
        let table = entities(m);
        m.push_frame(FRAME);
        let player = m.lw(table);
        let roll = m.lh(player + 0x1c);
        if roll >= 0x1c8 {
            arrow_433b4(m, 2, 0x50, 0x78);
            arrow_433b4(m, 3, 0xf0, 0x78);
        }
        if roll < -0x1c7 {
            arrow_433b4(m, 3, 0x50, 0x78);
            arrow_433b4(m, 2, 0xf0, 0x78);
        }
        let error = 0x71c_u32.wrapping_sub(u32::from(m.lhu(player + 0x18)));
        let pitch = i32::from(error as u16 as i16);
        if pitch >= 0xe4 {
            arrow_433b4(m, 2, 0xa0, 0x3c);
        }
        if pitch < -0xe3 {
            arrow_433b4(m, 3, 0xa0, 0xb4);
        }
        let index = (pitch.wrapping_neg() as u32 & 0x3fff) << 1;
        let sine = m.lhu(crate::shared::SINE_TABLE.wrapping_add(index));
        let offset = ((u32::from(sine) << 16) as i32 >> 24) as u32;
        let line = next_line(m);
        let rgb = if error.wrapping_add(0xe3) & 0xffff < 0x1c7 {
            [0, 0x80, 0]
        } else {
            [0x80, 0, 0]
        };
        let y = offset.wrapping_add(0x78);
        let faint = [0, 0x20, 0];
        let bar = [
            (rgb, [0x8c, y, 0xb4, y]),
            (DIM, [0x8c, 0x78, 0xb4, 0x78]),
            (faint, [0x8c, 0x73, 0xb4, 0x73]),
            (faint, [0x8c, 0x7d, 0xb4, 0x7d]),
        ];
        lines_f2(m, line, &bar);
        m.pop_frame(FRAME);
    }

    /// 80043E0C (`xy0`, `xy1`, `xy2`, `gate`): the next LINE_F3 through
    /// three screen points (SXY words) in green of level `((frame + gate) &
    /// 0xF) << 4` (frame = `[gameplay+0xC]`), so the gates pulse along the
    /// path.
    pub fn gate_432c4(m: &mut Machine, xy0: u32, xy1: u32, xy2: u32, gate: u32) {
        let set = m.gw(SET);
        let count = m.lw(set + 8);
        let buffer = m.gw(BUFFER);
        let game = gameplay(m);
        let frame = m.lw(game + 0xc);
        let line = buffer.wrapping_add(count.wrapping_mul(0x18));
        m.sb(line + 4, 0);
        m.sb(line + 6, 0);
        m.sw(line + 8, xy0);
        m.sw(line + 0xc, xy1);
        m.sw(line + 0x10, xy2);
        m.sb(line + 5, ((frame.wrapping_add(gate) & 0xf) << 4) as u8);
        let set = m.gw(SET);
        let count = m.lw(set + 8);
        m.sw(set + 8, count.wrapping_add(1));
    }

    /// 80043E74 (`xy0`, `xy1`, `xy2`, `xy3`): a chevron as the next two
    /// green LINE_F3, `xy0`-`xy1`-`xy3` and `xy0`-`xy2`-`xy3`.
    pub fn chevron_4332c(m: &mut Machine, xy0: u32, xy1: u32, xy2: u32, xy3: u32) {
        let set = m.gw(SET);
        let count = m.lw(set + 8);
        let buffer = m.gw(BUFFER);
        let mut line = buffer.wrapping_add(count.wrapping_mul(0x18));
        for middle in [xy1, xy2] {
            m.sb(line + 4, 0);
            m.sb(line + 5, 0xff);
            m.sb(line + 6, 0);
            m.sw(line + 8, xy0);
            m.sw(line + 0xc, middle);
            m.sw(line + 0x10, xy3);
            let set = m.gw(SET);
            let count = m.lw(set + 8);
            line = line.wrapping_add(0x18);
            m.sw(set + 8, count.wrapping_add(1));
        }
    }

    /// 80043EFC (`index`, `x`, `y`): arrow `index` of [`ARROWS`] at (`x`,
    /// `y`): the head as the next POLY_F3 (tip at (x, y); its color is
    /// preset by 80042938) and the bar as the next green LINE_F2.
    pub fn arrow_433b4(m: &mut Machine, index: u32, x: u32, y: u32) {
        let set = m.gw(SET);
        let shape = (index << 3).wrapping_add(ARROWS);
        let count = m.lw(set + 0x10);
        let buffer = m.gw(BUFFER);
        let poly = buffer.wrapping_add(count.wrapping_mul(0x14).wrapping_add(0x1400));
        let at = |m: &mut Machine, i: u32, base: u32| {
            base.wrapping_add(i32::from(m.lb(shape + i)) as u32) as u16
        };
        m.sh(poly + 8, x as u16);
        m.sh(poly + 0xa, y as u16);
        let value = at(m, 0, x);
        m.sh(poly + 0xc, value);
        let value = at(m, 1, y);
        m.sh(poly + 0xe, value);
        let value = at(m, 2, x);
        m.sh(poly + 0x10, value);
        let value = at(m, 3, y);
        m.sh(poly + 0x12, value);
        let polys = m.lw(set + 0x10);
        let lines = m.lw(set + 0xc);
        let line = buffer.wrapping_add((lines << 4).wrapping_add(0xc00));
        m.sw(set + 0x10, polys.wrapping_add(1));
        m.sb(line + 4, 0);
        m.sb(line + 5, 0xff);
        m.sb(line + 6, 0);
        let bar = [(8, x), (0xa, y), (0xc, x), (0xe, y)];
        for (i, (offset, base)) in (4..).zip(bar) {
            let value = at(m, i, base);
            m.sh(line + offset, value);
        }
        let set = m.gw(SET);
        let count = m.lw(set + 0xc);
        m.sw(set + 0xc, count.wrapping_add(1));
    }

    /// Indirect-call claims of this module (see [`super::dispatch`]).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, a3] = args;
        match target {
            0x8004_2b20 => guidance_41ff0(m, a0),
            0x8004_2bf4 => begin_420b4(m),
            0x8004_2c2c => link_420ec(m),
            0x8004_2d98 => approach_42258(m, a0),
            0x8004_357c => return Some(alignment_42a3c(m, a0)),
            0x8004_3bf8 => attitude_430b0(m),
            0x8004_3e0c => gate_432c4(m, a0, a1, a2, a3),
            0x8004_3e74 => chevron_4332c(m, a0, a1, a2, a3),
            0x8004_3efc => arrow_433b4(m, a0, a1, a2),
            _ => return None,
        }
        Some(0)
    }
}

pub mod extras {
    //! 8003E274, 8005E018, 80066590 and 80066A74: three HUD drawers on paths
    //! the captured frames do not reach. 8003E274 (from 8003DE5C in missions
    //! 0x12 and 0x25 while gameplay +8 is 1) marks a fixed map point: two
    //! short lines at its radar position (group flag bit 0 of the HUD block)
    //! and, when it is within 0x7FFF units and projects without a GTE error,
    //! three stacked triangles above its screen position whose size grows as
    //! it nears (bit 1). 8005E018 (panel flag 0x80 of 8005E178) colors a
    //! prebuilt panel frame of two sprites and two lines at half brightness
    //! and links it. 80066590 (from 80063E4C while the lock-suppress byte
    //! 800C6D70 is 1, the bomb weapon modes 5, 6 and 9) predicts where a bomb
    //! dropped now lands: the fall time over the height above the target (or
    //! the ground level of the view block) comes from 80066A74, the impact
    //! point is projected camera-relative and marked with a cross inside a
    //! diamond of pool lines.
    use super::*;
    use crate::{GP, sdk, shared};

    /// `[gp+0x434]`: the HUD primitive block (+0x18 optional group flags:
    /// bit 0 the two lines at area +0xC54, bit 1 the three triangles at area
    /// +0xC74, linked by 8003C690).
    const HUD_BLOCK: u32 = 0x434;
    /// `[gp+0x438]`: this frame's HUD primitive area.
    const HUD_AREA: u32 = 0x438;
    /// `[gp+0x43C]`: pointer to the radar basis (+0 x and +4 z of the
    /// player, position >> 12; +8 sin(yaw) / 4; +0xC cos(yaw) / 4; +0x10
    /// yaw).
    const RADAR_BASIS: u32 = 0x43c;
    /// 800C6D7C: radar range (position units that map to the 32-pixel
    /// radar radius).
    const RADAR_RANGE: u32 = 0x800c_6d7c;
    /// 800104AC: 1.14 sine halfwords, 0x4000 steps per turn (cosine of step
    /// `a` at entry `a + 0x1000`).
    const SINE: u32 = 0x8001_04ac;

    /// 800C6D60: HUD state block (+0x08..+0x0A line color).
    const STATE: u32 = 0x800c_6d60;
    /// `[gp+0x500]`: the HUD packet pool descriptor (+0x14 count of lines
    /// used this frame).
    const POOL: u32 = 0x500;
    /// `[gp+0x504]`: the pool buffer of this frame (`LINE_F2` packets of
    /// 0x10 bytes from +0x3D8).
    const BUFFER: u32 = 0x504;
    const LINE_COUNT: u32 = 0x14;
    const LINE_BASE: u32 = 0x3d8;
    /// Fall acceleration of 80066A74 per step.
    const GRAVITY: u32 = 0x29d;

    fn at(base: u32, offset: u32) -> u32 {
        base.wrapping_add(offset)
    }

    /// `sra`.
    fn sra(value: u32, shift: u32) -> u32 {
        ((value as i32) >> (shift & 31)) as u32
    }

    // JP-REVIEW(R0169): near function: PAL 8003DAB4 -> JP 8003E274, imm:1, 1 instructions differ (whole routine)
    /// 8003E274 (`marker`): draw the fixed map marker. `marker` points to
    /// six words (8003DE5C copies them from 80023DEC into its frame): a
    /// position VECTOR (+0 x, +4 y, +8 z) and a heading halfword at +0x12.
    ///
    /// Its radar offset from the player (`x >> 12 - basis.x`, `basis.z -
    /// z >> 12`) has length SquareRoot0(dx² + dz²) (with both components
    /// `>> 4` and the root `<< 4` when one is 0x4000 or more); beyond the
    /// radar range nothing is drawn. Rotated by the radar basis and scaled
    /// by `32 / range` it must lie within 31 pixels; then two red lines
    /// (`LINE_F2` at area +0xC54, +0xC64) start at the radar point, angled
    /// by `[gp+0xF4..]` + yaw - heading and shortened by `[gp+0xF8..]`,
    /// and group flag word `[gp+0x434]+0x18` becomes 1. Within distance
    /// 0x7FFF (80083DFC) and with a valid RotTransPers (camera-relative
    /// position `>> 12`, rotation gameplay +0x4C, no translation), three
    /// `POLY_F3` (area +0xC74..) stack above the screen point, `size =
    /// clamp((0x8000 - distance) >> 10, 2, 8)` wide, colored by gameplay
    /// +0xC (bits 2..3) plus the triangle index, and the flag word gains 2.
    /// Returns 0 on every path.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_extra::radar_marker_3dab4` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn radar_marker_3dab4(m: &mut Machine, marker: u32) -> u32 {
        crate::jp::res_p3_hud_extra::radar_marker_3dab4(m, marker)
    }

    /// 8005E018 (`panel`, `ot`, `block`): the half-brightness panel frame
    /// (panel flag 0x80; 8005E178 passes `chain + 0x3DC`). The two lines at
    /// `block + 0x28` and `+0x38` take half the first panel color (`panel +
    /// 0x1C..`), the two sprites at `block + 0` and `+0x14` half the second
    /// (`panel + 0x20..`); the prebuilt chain `+0x28 -> +0x38 -> +0 -> +0x14`
    /// is linked into slot `ot` (the last tag takes the slot's link, the
    /// slot then points at `block + 0x28`).
    pub fn dim_frame_5c8f0(m: &mut Machine, panel: u32, ot: u32, block: u32) {
        for (to, from) in [(0x2c, 0x1c), (0x3c, 0x1c), (4, 0x20), (0x18, 0x20)] {
            for c in 0..3 {
                let value = m.lbu(at(panel, from + c)) >> 1;
                m.sb(at(block, to + c), value);
            }
        }
        let last = at(block, 0x14);
        let tag = m.lw(last);
        let link = m.lw(ot);
        m.sw(last, (tag & LENGTH) | (link & LINK));
        let link = m.lw(ot);
        m.sw(ot, (link & LENGTH) | (at(block, 0x28) & LINK));
    }

    // JP-REVIEW(R0171): near function: PAL 80064DC4 -> JP 80066590, del:3 imm:3 ins:4, 10 instructions differ (whole routine)
    /// 80066590 (`view`): the bomb impact marker. The ground level is the y
    /// word (+0xC) of the locked target (800C6D6E; in missions other than
    /// 0xD only without slot flag 0x400) when it is not above the player
    /// (entity 0), else view +0x2C. The fall time [`fall_steps_652a4`] over
    /// the height above that level, starting at 1.5 times the player's
    /// vertical speed (+0x3C), scales the horizontal speed (+0x38, +0x40;
    /// 1.5 times unless weapon mode byte context +0x3AE is 9, or 8003A5C0
    /// returns 1 and context +0x3AF is 9) into the drop offset. The impact
    /// point (player x/z + offset, the ground level), relative to the camera
    /// `[gameplay+0x28]` (+4..) and `>> 12`, goes through RotTransPers with
    /// the camera rotation (800308F0 of camera +0x14) and no translation;
    /// the six `LINE_F2` of [`IMPACT_LINES`] at the screen point take the
    /// next pool lines, colored by HUD state +8..+0xA, whatever the GTE flag.
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_extra::impact_marker_64dc4` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn impact_marker_64dc4(m: &mut Machine, view: u32) {
        crate::jp::res_p3_hud_extra::impact_marker_64dc4(m, view)
    }

    /// 80066A74 (`distance`, `speed`) -> steps: the number of steps a fall
    /// starting at `speed` and accelerating by 0x29D per step needs to cover
    /// `distance` (signed; 0 for `distance <= 0`): the speed grows before
    /// each step is added, and at least one step is counted otherwise.
    pub fn fall_steps_652a4(distance: u32, speed: u32) -> u32 {
        if distance as i32 <= 0 {
            return 0;
        }
        let (mut speed, mut covered, mut steps) = (speed, 0u32, 0u32);
        loop {
            speed = speed.wrapping_add(GRAVITY);
            covered = covered.wrapping_add(speed);
            steps = steps.wrapping_add(1);
            if (covered as i32) >= (distance as i32) {
                return steps;
            }
        }
    }

    /// Native targets of this group by original address: 8003E274
    /// (`marker`) -> 0; 8005E018 (`panel`, `ot`, `block`); 80066590
    /// (`view`); 80066A74 (`distance`, `speed`) -> steps. 8005E018 and
    /// 80066590 leave no meaningful `v0` (no caller reads it): `Some(0)`.
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, a2, _] = args;
        match target {
            0x8003_e274 => Some(radar_marker_3dab4(m, a0)),
            0x8005_e018 => {
                dim_frame_5c8f0(m, a0, a1, a2);
                Some(0)
            }
            0x8006_6590 => {
                impact_marker_64dc4(m, a0);
                Some(0)
            }
            0x8006_6a74 => Some(fall_steps_652a4(a0, a1)),
            _ => None,
        }
    }
}

pub mod tree_62ea0 {
    //! 8006466C..80064CB0: the attitude marker that 80064400 draws while the
    //! view is turned away from straight ahead (a look angle at `[view+0x10]`
    //! outside -2..=2). 8006466C sets the GTE rotation to the attitude of
    //! the player entity (entity 0, angles +0x18..+0x1C) relative to the view
    //! object `[gameplay+0x28]` (angles +0x14..+0x18; its +0xB4 look angles
    //! are the ones 80064400 tests), with translation (0, 0, 0x400). 8006477C
    //! projects a four-vertex dart (SVECTORs at 800B2C18: top, nose, two wing
    //! tips) about the screen point (0xA0, 0xC8), culls its four faces with
    //! NormalClip and appends the front faces as green Gouraud triangles
    //! (`POLY_G3`, the 0x1C-byte class of the HUD packet pool described in
    //! [`super::tree_62680_a`]) and every edge of a face that is not a back
    //! face as a green Gouraud line (`LINE_G2`, the 0x14-byte class).
    use super::*;
    use crate::{sdk, shared};

    /// `[gp+0x500]`: the HUD packet pool descriptor.
    const POOL: u32 = 0x500;
    /// `[gp+0x504]`: the pool buffer of this frame.
    const BUFFER: u32 = 0x504;
    /// Triangle class of the pool: count word +0x0C, 0x1C-byte `POLY_G3`
    /// packets from buffer +0x188.
    const TRIANGLE_COUNT: u32 = 0x0c;
    const TRIANGLE_BASE: u32 = 0x188;
    const TRIANGLE_SIZE: u32 = 0x1c;
    /// Gouraud line class of the pool: count word +0x18, 0x14-byte
    /// `LINE_G2` packets from buffer +0x7D8.
    const EDGE_COUNT: u32 = 0x18;
    const EDGE_BASE: u32 = 0x7d8;
    const EDGE_SIZE: u32 = 0x14;
    /// 800A6A08 SetMulMatrix(m0, m1): rotation matrix = m0 * m1 (not in
    /// `sdk.rs`).
    const SET_MUL_MATRIX: u32 = 0x800a_6a08;
    /// The dart's faces in the original order: vertex indices (NormalClip
    /// argument and packet order) and the green level of each vertex (the
    /// nose, vertex 1, is bright).
    const FACES: [([u32; 3], [u8; 3]); 4] = [
        ([0, 2, 1], [0x10, 0x10, 0x40]),
        ([0, 1, 3], [0x10, 0x40, 0x10]),
        ([0, 3, 2], [0x10, 0x10, 0x10]),
        ([1, 2, 3], [0x40, 0x10, 0x10]),
    ];
    /// The dart's edges in the original order: vertex pair, green levels,
    /// and the two faces sharing the edge (the second is read only when the
    /// first is a back face).
    const EDGES: [([u32; 2], [u8; 2], [u32; 2]); 6] = [
        ([0, 1], [0x18, 0x40], [0, 1]),
        ([0, 2], [0x18, 0x18], [0, 2]),
        ([0, 3], [0x18, 0x18], [1, 2]),
        ([1, 2], [0x40, 0x18], [0, 3]),
        ([1, 3], [0x40, 0x18], [1, 3]),
        ([2, 3], [0x18, 0x18], [2, 3]),
    ];

    fn at(base: u32, offset: u32) -> u32 {
        base.wrapping_add(offset)
    }

    /// 8006466C: draw the attitude marker. In its 0x88-byte frame: angles
    /// SVECTOR +0x70 (halfwords +0x14/+0x16/+0x18 of `[gameplay+0x28]`) →
    /// 800308F0 → MATRIX +0x50; then entity 0's +0x18/+0x1A/+0x1C → 80030778
    /// → MATRIX +0x30; then the same angles with y = 0 → 80030778 → MATRIX
    /// +0x10 (not used further). The three translations become (0, 0,
    /// 0x400); SetMulMatrix(+0x50, +0x30), SetTransMatrix(+0x30), then
    /// 8006477C (0xA0, 0xC8). Reads no argument register (80064400 passes
    /// the view block and 0, both overwritten before use). Returns 8006477C's
    /// v0, which 80064400 returns.
    pub fn attitude_marker_62ea0(m: &mut Machine) -> u32 {
        const FRAME: u32 = 0x88;
        let frame = m.push_frame(FRAME);
        let level = frame + 0x10; // MATRIX (entity 0 with zero y angle)
        let attitude = frame + 0x30; // MATRIX (entity 0)
        let view = frame + 0x50; // MATRIX (view object, transposed)
        let angles = frame + 0x70; // SVECTOR
        let gameplay = gameplay(m);
        for (i, offset) in [0x14, 0x16, 0x18].into_iter().enumerate() {
            let object = m.lw(at(gameplay, 0x28));
            let angle = m.lhu(at(object, offset));
            m.sh(angles + 2 * i as u32, angle);
        }
        shared::f_800306ac(m, [angles, view, 0, 0]);
        let table = entities(m);
        for (i, offset) in [0x18, 0x1a, 0x1c].into_iter().enumerate() {
            let player = m.lw(table);
            let angle = m.lhu(at(player, offset));
            m.sh(angles + 2 * i as u32, angle);
        }
        shared::f_80030534(m, [angles, attitude, 0, 0]);
        m.sh(angles + 2, 0);
        shared::f_80030534(m, [angles, level, 0, 0]);
        // Translation words (+0x14..+0x1C of each MATRIX), original order.
        for offset in [0x64, 0x24, 0x44, 0x68, 0x28, 0x48] {
            m.sw(frame + offset, 0);
        }
        for offset in [0x6c, 0x2c, 0x4c] {
            m.sw(frame + offset, 0x400);
        }
        // a2/a3 are leftovers of 80030778; SetMulMatrix reads only a0/a1.
        crate::dispatch::call(m, SET_MUL_MATRIX, [view, attitude, 0, 0]);
        sdk::set_trans_matrix(m, attitude);
        let v0 = dart_62fb0(m, 0xa0, 0xc8);
        m.pop_frame(FRAME);
        v0
    }

    // JP-REVIEW(R0174): near function: PAL 80062FB0 -> JP 8006477C, imm:1, 1 instructions differ (whole routine)
    /// 8006477C (`x`, `y`): SetGeomOffset(`x`, `y`), RotTransPers of the four
    /// vertices at 800B2C18 into the screen words of its 0x50-byte frame
    /// (+0x10..+0x1C; depth cue +0x38 and flag +0x3C shared), SetGeomOffset
    /// back to (0xA0, 0x80), the NormalClip of each face into +0x28..+0x34,
    /// then the packets ([`dart_packets`]). Returns the original v0: the
    /// line count after the last edge, or the (negative) NormalClip of face 3
    /// when that edge is culled. Only caller: 8006466C, with (0xA0, 0xC8).
    // JP-PATCH: replaced by `crate::jp::res_p3_hud_extra::dart_62fb0` (src/jp/mod.rs REPLACES, PATCHES.md)
    pub fn dart_62fb0(m: &mut Machine, x: u32, y: u32) -> u32 {
        crate::jp::res_p3_hud_extra::dart_62fb0(m, x, y)
    }

    // JP-REVIEW(R0175): near function: PAL 80062FB0 -> JP 8006477C, imm:1, 1 instructions differ (the part at +0xac)
    /// 80064828..80064CB0, the packet half of [`dart_62fb0`] over its frame
    /// `frame` (screen words +0x10..+0x1C, NormalClip results +0x28..+0x34,
    /// read from guest memory as the original does). Faces with a positive
    /// NormalClip fill consecutive `POLY_G3` packets from the triangle-class
    /// slot of the pool count at entry (colors, then the three xy words;
    /// count reread and bumped after each). Then each edge whose first or
    /// second face has a NormalClip `>= 0` fills consecutive `LINE_G2`
    /// packets from the line-class slot of its count (colors, then the two xy
    /// words; count bumped). Returns the original v0 (see [`dart_62fb0`]).
    pub fn dart_packets(m: &mut Machine, frame: u32) -> u32 {
        let screen = at(frame, 0x10);
        let areas = at(frame, 0x28);

        let pool = m.gw(POOL);
        let count = m.lw(at(pool, TRIANGLE_COUNT));
        let buffer = m.gw(BUFFER);
        let mut prim = at(
            buffer,
            count
                .wrapping_mul(TRIANGLE_SIZE)
                .wrapping_add(TRIANGLE_BASE),
        );
        for (face, (vertices, greens)) in FACES.into_iter().enumerate() {
            if (m.lw(at(areas, 4 * face as u32)) as i32) <= 0 {
                continue;
            }
            for (k, green) in (0u32..).zip(greens) {
                let color = at(prim, 4 + 8 * k);
                m.sb(color, 0);
                m.sb(at(color, 1), green);
                m.sb(at(color, 2), 0);
            }
            for (k, vertex) in (0u32..).zip(vertices) {
                let xy = m.lw(at(screen, 4 * vertex));
                m.sw(at(prim, 8 + 8 * k), xy);
            }
            let pool = m.gw(POOL);
            let count = m.lw(at(pool, TRIANGLE_COUNT));
            prim = at(prim, TRIANGLE_SIZE);
            m.sw(at(pool, TRIANGLE_COUNT), count.wrapping_add(1));
        }

        let pool = m.gw(POOL);
        let count = m.lw(at(pool, EDGE_COUNT));
        let buffer = m.gw(BUFFER);
        let mut prim = at(
            buffer,
            count.wrapping_mul(EDGE_SIZE).wrapping_add(EDGE_BASE),
        );
        let mut v0 = 0;
        for (vertices, greens, [first, second]) in EDGES {
            if (m.lw(at(areas, 4 * first)) as i32) < 0 {
                let other = m.lw(at(areas, 4 * second));
                if (other as i32) < 0 {
                    v0 = other;
                    continue;
                }
            }
            for (k, green) in (0u32..).zip(greens) {
                let color = at(prim, 4 + 8 * k);
                m.sb(color, 0);
                m.sb(at(color, 1), green);
                m.sb(at(color, 2), 0);
            }
            for (k, vertex) in (0u32..).zip(vertices) {
                let xy = m.lw(at(screen, 4 * vertex));
                m.sw(at(prim, 8 + 8 * k), xy);
            }
            let pool = m.gw(POOL);
            let count = m.lw(at(pool, EDGE_COUNT));
            prim = at(prim, EDGE_SIZE);
            v0 = count.wrapping_add(1);
            m.sw(at(pool, EDGE_COUNT), v0);
        }
        v0
    }

    /// Native targets of this group by original address (a0/a1 in, v0 out).
    pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
        let [a0, a1, ..] = args;
        match target {
            0x8006_466c => Some(attitude_marker_62ea0(m)),
            0x8006_477c => Some(dart_62fb0(m, a0, a1)),
            _ => None,
        }
    }
}

/// Native targets owned by this module, by original address: a0..a3 in,
/// v0 out (0 for routines without a result). Stack arguments (5th and
/// later) are read from the caller's frame at `Machine::sp + 0x10..`, as the
/// original does.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    if let Some(v0) = tree_62ea0::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = extras::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_41ff0::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = marker_shapes::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = stream_draw::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = popups::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = stream::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_5cbf4::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_3e828::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_3bdf8_a::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_3bdf8_b::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_39d84::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_40a24::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_62680_a::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = tree_62680_b::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = status::dispatch(m, target, args) {
        return Some(v0);
    }
    if let Some(v0) = compose::dispatch(m, target, args) {
        return Some(v0);
    }
    let [a0, a1, a2, a3] = args;
    match target {
        0x8003_b7b0 => {
            let sp = m.sp;
            let prim = m.lw(sp + 0x10);
            let color = m.lw(sp + 0x14);
            glyph_3b00c(m, a0, a1, a2, a3, prim, color);
            Some(0)
        }
        _ => None,
    }
}
