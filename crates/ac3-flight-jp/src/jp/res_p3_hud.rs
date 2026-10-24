//! Patches of `hud` routines whose JP instructions differ from PAL
//! (REVIEW.md §2-§3), part 3 of the resident wave. Each is listed in
//! [`REPLACES`](super::REPLACES) by its generated path
//! (`hud::<inline module>::<fn>`); the patches are top-level functions of
//! the same name.
//!
//! Most differences are the 240-line NTSC layout: y coordinates near the
//! bottom of the screen move up by 20 (0xE7 -> 0xD3), the screen centre
//! 0x80 -> 0x78 and the height 0x100 -> 0xF0; the JP context has one word
//! less before +0x3AA (PAL +0x390 mission, +0x3A0, +0x3AE/+0x3AF are JP
//! +0x38C, +0x39C, +0x3AA/+0x3AB), and PAL's language byte 0x800BAEB4
//! checks are JP mission checks (context halfword +0x38C).
use crate::{
    hud::{LENGTH, LINK, add_prim, context, glyph_3b00c},
    machine::{Machine, SmallData},
    sdk,
};

// ---- stream (800337DC) ---------------------------------------------------------------

/// 8002D3F4 (JP; PAL 8002CFCC, near, `imm:1`) (`ot`, `primitives`) -> next
/// free primitive. Unpacks a new stream word into
/// `gp+0x320..0x322/0x31A/0x31B`, updates the digits, then, when the block
/// mode and `[context+0x39C]` (PAL +0x3A0) / `gp+0x318` allow, draws through
/// 8002D330 (mode 9), 8002D10C (mode 10) or 8002CE70.
pub fn overlay_2cfcc(m: &mut Machine, ot: u32, primitives: u32) -> u32 {
    use crate::hud::{stream, stream_draw};
    /// `[gp+0x314]`: overlay block (+0 mode).
    const BLOCK: u32 = 0x314;
    const FRAME: u32 = 0x28;
    let frame = m.push_frame(FRAME);
    let word_at = frame + 0x10;
    if stream::stream_word_2c558(m, word_at, 0) != 0 {
        let word = m.lw(word_at);
        if word != 0xffff_ffff && m.gw(0x31c) != word {
            let signed = word as i32;
            m.sb(crate::GP + 0x320, (word >> 25) as u8);
            m.sb(crate::GP + 0x321, ((signed >> 18) & 0x7f) as u8);
            m.sb(crate::GP + 0x322, ((signed >> 11) & 0x7f) as u8);
            m.sb(crate::GP + 0x31a, ((signed >> 5) & 0x3f) as u8);
            m.sb(crate::GP + 0x31b, ((signed >> 2) & 7) as u8);
        }
        let word = m.lw(word_at);
        m.set_gw(0x31c, word);
    }
    stream::digits_2c8fc(m);
    let block = m.gw(BLOCK);
    let mode = m.lw(block);
    if mode < 2 || mode == 7 || mode == 10 || mode == 9 {
        let context = context(m);
        if m.lbu(context + 0x39c) != 1 {
            m.pop_frame(FRAME);
            return primitives;
        }
    }
    if m.lbu(crate::GP + 0x318) != 1 {
        m.pop_frame(FRAME);
        return primitives;
    }
    let block = m.gw(BLOCK);
    let mode = m.lw(block);
    let result = match mode {
        9 => stream_draw::copy_line_2cf08(m, ot, primitives),
        10 => stream_draw::quad_lines_2cce4(m, ot, primitives),
        _ => stream_draw::sprite_lines_2ca48(m, ot, primitives),
    };
    m.pop_frame(FRAME);
    result
}

// ---- tree_5cbf4 (8005E31C, the instrument panel) ---------------------------------------

/// 800104AC: 16-bit sine table (0x1000 per quarter turn, cosine at +0x1000
/// entries).
const SINE: u32 = 0x8001_04ac;
/// 800C6D6F: blink phase byte (non-zero: draw the damage marks steady).
const BLINK: u32 = 0x800c_6d6f;

/// `mult x, magic; mfhi; sra shift; subu (x >> 31)`: the compiler's signed
/// division by a constant, replicated instruction for instruction.
fn magic_div(x: i32, magic: u32, shift: u32) -> i32 {
    let high = ((i64::from(x) * i64::from(magic as i32)) >> 32) as i32;
    (high >> shift).wrapping_sub(x >> 31)
}

/// `mult x, magic; mfhi; addu x; sra shift; subu (x >> 31)`: the signed
/// division by a constant whose magic number needs the dividend added back
/// (JP's divisions by 30, 15 and 1800).
fn magic_div_add(x: i32, magic: u32, shift: u32) -> i32 {
    let high = ((i64::from(x) * i64::from(magic as i32)) >> 32) as i32;
    (high.wrapping_add(x) >> shift).wrapping_sub(x >> 31)
}

/// Quotient by 10 (`0x66666667 >> 2`).
fn div10(x: i32) -> i32 {
    magic_div(x, 0x6666_6667, 2)
}

/// Link the packets `first..=last` (already chained) into `slot`: the last
/// tag takes the slot's link, the slot then points at `first`.
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

/// Glyph color index of the panel (`lh +0x24`, sign-extended).
fn glyph_color(m: &mut Machine, panel: u32) -> u32 {
    m.lh(panel + 0x24) as i32 as u32
}

/// 8005C680 (JP; PAL 8005AF88, near, `imm:6`): heading. Up to four digits
/// of `([panel+8] * 28) >> 12` right to left from x = 0x40 (sprites from
/// `block + 0x14`), then two four-point compass lines (`block + 0x64`,
/// `block + 0x7C`) rotated by the heading, all centred on y 0x89 (PAL
/// 0x91).
pub fn heading_5af88(m: &mut Machine, panel: u32, ot: u32, block: u32) {
    const Y: i32 = 0x89;
    let heading = m.lw(panel + 8);
    let mut value = (heading.wrapping_mul(28) as i32) >> 12;
    let mut x = 0x40u32;
    let mut sprite = block + 0x14;
    for _ in 0..4 {
        let color = glyph_color(m, panel);
        let quotient = div10(value);
        let digit = value.wrapping_sub(quotient.wrapping_mul(10));
        glyph_3b00c(m, digit as u32, x, Y as u32, 1, sprite, color);
        value = quotient;
        add_prim(m, ot, sprite);
        if value == 0 {
            break;
        }
        x = x.wrapping_sub(10);
        sprite += 0x14;
    }
    let lines = [block + 0x64, block + 0x7c];
    color(m, block, panel, 0x20);
    let angle = (m.lw(panel + 8) << 2) & 0x3fff;
    let shade = m.lbu(panel + 0x1c);
    m.sb(lines[1] + 4, shade);
    m.sb(lines[0] + 4, shade);
    // sin/cos * 37 >> 16 at angle - 0x600, angle, angle + 0x600.
    let scaled =
        |m: &mut Machine, index: u32| -> i32 { i32::from(m.lh(SINE + index * 2)).wrapping_mul(37) };
    let left = (angle.wrapping_sub(0x600)) & 0x3fff;
    let right = (angle + 0x600) & 0x3fff;
    let a = scaled(m, left) >> 16;
    let b = scaled(m, left + 0x1000).wrapping_neg() >> 16;
    let c = scaled(m, angle) >> 16;
    let d = scaled(m, angle + 0x1000).wrapping_neg() >> 16;
    let shade = m.lbu(panel + 0x1d);
    m.sb(lines[1] + 5, shade);
    m.sb(lines[0] + 5, shade);
    let e = scaled(m, right) >> 16;
    let shade = m.lbu(panel + 0x1e);
    m.sb(lines[1] + 6, shade);
    m.sb(lines[0] + 6, shade);
    let [first, second] = lines;
    m.sh(first + 8, a.wrapping_add(0x14) as u16);
    m.sh(second + 8, 0x14i32.wrapping_sub(a) as u16);
    m.sh(first + 0xa, b.wrapping_add(Y) as u16);
    m.sh(second + 0xa, Y.wrapping_sub(b) as u16);
    m.sh(first + 0xc, c.wrapping_add(0x14) as u16);
    m.sh(second + 0xc, 0x14i32.wrapping_sub(c) as u16);
    m.sh(first + 0xe, d.wrapping_add(Y) as u16);
    m.sh(second + 0xe, Y.wrapping_sub(d) as u16);
    m.sh(second + 0x10, 0x14);
    m.sh(first + 0x10, 0x14);
    m.sh(second + 0x12, Y as u16);
    m.sh(first + 0x12, Y as u16);
    m.sh(first + 0x14, e.wrapping_add(0x14) as u16);
    let f = scaled(m, right + 0x1000).wrapping_neg() >> 16;
    m.sh(second + 0x14, 0x14i32.wrapping_sub(e) as u16);
    m.sh(first + 0x16, f.wrapping_add(Y) as u16);
    m.sh(second + 0x16, Y.wrapping_sub(f) as u16);
    // The block's packets run from the first line (+0x64) to the color
    // packet at +0: the slot points at the line, +0 continues the slot.
    link_chain(m, ot, first, block);
}

/// 8005CB48 (JP; PAL 8005B450, near, `imm:6`): altitude. `[panel+4] >> 12`
/// scaled by -2/3, clamped to -9999..99999: a minus sign (`block + 0xF0`,
/// y 0x67, PAL 0x6F), up to five digits from x = 0x11B (`block + 0x8C`, y
/// 0x66, PAL 0x6E), a ladder of five ticks offset by the tens digit (base y
/// 0x60, PAL 0x68), and two drawing-area packets (0x104, 0x110) clipping
/// it: (0x12E, 0x60 or 0x160, 6, 0x28) and the 320x240 screen (PAL y
/// 0x68/0x168 and height 0x100).
pub fn altitude_5b450(m: &mut Machine, panel: u32, ot: u32, block: u32) {
    const FRAME: u32 = 0x50;
    let frame = m.push_frame(FRAME);
    let raw = m.lw(panel + 4) as i32;
    let doubled = (raw >> 12).wrapping_neg().wrapping_shl(1);
    let mut value = magic_div(doubled, 0x5555_5556, 0);
    let tens = div10(value);
    let mut ladder = tens.wrapping_sub(div10(tens).wrapping_mul(10));
    value = value.clamp(-9999, 99999);
    if value < 0 {
        let sign = block + 0xf0;
        let color = glyph_color(m, panel);
        glyph_3b00c(m, 11, 0xf3, 0x67, 0, sign, color);
        value = value.wrapping_neg();
        add_prim(m, ot, sign);
    }
    let mut x = 0x11bu32;
    let mut sprite = block + 0x8c;
    for _ in 0..5 {
        let color = glyph_color(m, panel);
        let quotient = div10(value);
        let digit = value.wrapping_sub(quotient.wrapping_mul(10));
        glyph_3b00c(m, digit as u32, x, 0x66, 1, sprite, color);
        value = quotient;
        add_prim(m, ot, sprite);
        if value == 0 {
            break;
        }
        x = x.wrapping_sub(10);
        sprite += 0x14;
    }
    color(m, block, panel, 0x20);
    color(m, block + 0x14, panel, 0x20);
    if ladder < 0 {
        ladder += 10;
    }
    let mut offset: i32 = -10;
    for tick in 0..5 {
        let at = block + 0x2c + tick * 0x14;
        let y = offset.wrapping_add(ladder.wrapping_add(0x60));
        let r = m.lbu(panel + 0x20);
        m.sb(at, r);
        let g = m.lbu(panel + 0x21);
        offset += 10;
        m.sb(at + 1, g);
        let b = m.lbu(panel + 0x22);
        m.sh(at + 6, y as u16);
        m.sb(at + 2, b);
    }
    let rect = frame + 0x18;
    let context = context(m);
    m.sh(rect, 0x12e);
    m.sh(rect + 2, 0x60);
    m.sh(rect + 4, 6);
    m.sh(rect + 6, 0x28);
    if m.lw(context + 4) == 1 {
        m.sh(rect + 2, 0x160);
    }
    let clip = block + 0x104;
    sdk::set_draw_area(m, clip, rect);
    let context = crate::hud::context(m);
    m.sh(rect + 4, 0x140);
    m.sh(rect, 0);
    m.sh(rect + 2, 0);
    m.sh(rect + 6, 0xf0);
    if m.lw(context + 4) == 1 {
        m.sh(rect + 2, 0x100);
    }
    let restore = block + 0x110;
    sdk::set_draw_area(m, restore, rect);
    sdk::cat_prim(m, block + 0x14, clip);
    sdk::cat_prim(m, clip, block + 0x28);
    sdk::cat_prim(m, block + 0x78, restore);
    link_chain(m, ot, block, block + 0x110);
    m.pop_frame(FRAME);
}

/// 8005D000 (JP; PAL 8005B908, changed, `del:4 imm:10 ins:16 op:9 reg:16`):
/// mission timer from `[panel+0x28]`, which JP counts in 1/30 s (PAL
/// 1/25 s): `mm:ss`, or in timer mode 1 below 1800 (one minute) `ss:hh`
/// (hundredths = `100 * (ticks % 30) / 30`) in red, at y 0xD3 (PAL 0xE7).
/// The separator blinks with bit 0 of `2 * ticks / 15` (PAL `4 * ticks /
/// 25`), or with tick bit 2 in timer mode 1.
pub fn timer_5b908(m: &mut Machine, panel: u32, ot: u32, block: u32) {
    const Y: u32 = 0xd3;
    let mode = m.lh(panel + 0x26);
    let ticks = m.lw(panel + 0x28) as i32;
    let (major, minor) = if mode == 1 && ticks < 0x708 {
        let seconds = magic_div_add(ticks, 0x8888_8889, 4);
        let rest = ticks.wrapping_sub(seconds.wrapping_mul(30));
        let hundredths = magic_div_add(rest.wrapping_mul(100), 0x8888_8889, 4);
        (seconds, hundredths)
    } else {
        let ticks = m.lw(panel + 0x28) as i32;
        let minutes = magic_div_add(ticks, 0x91a2_b3c5, 10);
        let seconds = magic_div_add(ticks, 0x8888_8889, 4);
        (minutes, seconds.wrapping_sub(minutes.wrapping_mul(60)))
    };
    let lit = u32::from(m.lh(panel + 0x26) == 1 || m.lbu(panel + 0x20) != 0);
    let tens = div10(major);
    glyph_3b00c(m, tens as u32, 0x18, Y, 1, block + 0x28, lit);
    let ones = major.wrapping_sub(tens.wrapping_mul(10));
    glyph_3b00c(m, ones as u32, 0x22, Y, 1, block + 0x3c, lit);
    glyph_3b00c(m, 10, 0x2c, Y, 1, block + 0x50, lit);
    let tens = div10(minor);
    glyph_3b00c(m, tens as u32, 0x31, Y, 1, block + 0x64, lit);
    let ones = minor.wrapping_sub(tens.wrapping_mul(10));
    glyph_3b00c(m, ones as u32, 0x3b, Y, 1, block + 0x78, lit);
    if m.lh(panel + 0x26) == 1 {
        m.sb(block + 4, 0x83);
        m.sb(block + 5, 0);
        m.sb(block + 6, 0);
    } else {
        color(m, block, panel, 0x20);
    }
    let separator = block + 0x14;
    let dark = if m.lh(panel + 0x26) == 1 {
        m.lw(panel + 0x28) & 4 != 0
    } else {
        let doubled = (m.lw(panel + 0x28) << 1) as i32;
        magic_div_add(doubled, 0x8888_8889, 3) & 1 != 0
    };
    if dark {
        m.sb(separator + 4, 0);
        m.sb(separator + 5, 0);
        m.sb(separator + 6, 0);
    } else if m.lh(panel + 0x26) == 1 {
        m.sb(separator + 4, 0x83);
        m.sb(separator + 5, 0);
        m.sb(separator + 6, 0);
    } else {
        color(m, separator, panel, 0x20);
    }
    link_chain(m, ot, block, block + 0x78);
}

/// 8005D468 (JP; PAL 8005BD40, near, `del:28 imm:4 ins:28 op:4 reg:3`):
/// damage. `[panel+0xC]` (0..0x1000) as a percentage: the gauge color turns
/// from green to red (darker when low and blinking) unless the marks are
/// drawn steady (JP computes the color only then), the gauge texture page
/// steps with damage, and `100 - percent` is printed with up to three
/// digits at y 0xDB (PAL 0xEF).
pub fn damage_5bd40(m: &mut Machine, panel: u32, ot: u32, block: u32) {
    const Y: u32 = 0xdb;
    let raw = m.lw(panel + 0xc) as i32;
    let damage = if raw >= 0x1001 {
        0x1000
    } else if raw < 0 {
        0
    } else {
        raw
    };
    color(m, block, panel, 0x20);
    let mut percent = damage.wrapping_mul(100) >> 12;
    let gauge = block + 0x14;
    let steady = m.lbu(BLINK) != 0;
    // `red`/`green` (a3/t0) are only computed, and only read, when the
    // gauge is not drawn steady.
    let (mut red, mut green) = (0i32, 0i32);
    if steady && m.lw(panel) & 0x20 != 0 {
        color(m, gauge, panel, 0x20);
    } else {
        let scaled = percent.wrapping_mul(131);
        green = magic_div(scaled, 0x51eb_851f, 5);
        red = 0x83i32.wrapping_sub(green);
        if percent < 0x14 && m.lw(panel + 0x28) & 1 != 0 {
            red = (red & 0xff) >> 2;
        }
        m.sb(gauge + 4, red as u8);
        m.sb(gauge + 5, green as u8);
        m.sb(gauge + 6, 0);
    }
    let mut step = 0xfi32.wrapping_sub(damage >> 8);
    if step < 0 {
        step = 0;
    }
    let page = (step & 3)
        .wrapping_add((step & 0xc) << 4)
        .wrapping_add(0x7e18);
    m.sh(gauge + 0xe, page as u16);
    link_chain(m, ot, block, gauge);
    if m.lw(panel) & 0x20 != 0 {
        let marks = block + 0x28;
        if m.lbu(BLINK) != 0 {
            color(m, marks, panel, 0x20);
        } else {
            m.sb(marks + 4, red as u8);
            m.sb(marks + 5, green as u8);
            m.sb(marks + 6, 0);
        }
        add_prim(m, ot, marks);
    }
    percent = 100i32.wrapping_sub(percent);
    let mut sprite = block + 0x3c;
    if percent >= 100 {
        let color = glyph_color(m, panel);
        glyph_3b00c(m, 1, 0xee, Y, 1, sprite, color);
        add_prim(m, ot, sprite);
        sprite = block + 0x50;
    }
    if percent >= 10 {
        let tens = div10(percent);
        let color = glyph_color(m, panel);
        let digit = tens.wrapping_sub(div10(tens).wrapping_mul(10));
        glyph_3b00c(m, digit as u32, 0xf4, Y, 1, sprite, color);
        add_prim(m, ot, sprite);
        sprite += 0x14;
    }
    let color = glyph_color(m, panel);
    let digit = percent.wrapping_sub(div10(percent).wrapping_mul(10));
    glyph_3b00c(m, digit as u32, 0xfc, Y, 1, sprite, color);
    add_prim(m, ot, sprite);
}

/// 8005D998 (JP; PAL 8005C270, near, `imm:3`): missiles. One bar per
/// capacity slot (`[panel+0x14]`), dimmed below `[panel+0x18]` and below
/// `capacity - count`, then the count (`[panel+0x10]`) with up to three
/// digits at y 0xC8 (PAL 0xDC).
pub fn missiles_5c270(m: &mut Machine, panel: u32, ot: u32, block: u32) {
    const Y: u32 = 0xc8;
    let tag = m.lw(block);
    color(m, block, panel, 0x20);
    let link = m.lw(ot);
    m.sw(block, (tag & LENGTH) | (link & LINK));
    let link = m.lw(ot);
    m.sw(ot, (link & LENGTH) | (block & LINK));
    let capacity = m.lw(panel + 0x14) as i32;
    let count = m.lw(panel + 0x10) as i32;
    let empty = capacity.wrapping_sub(count);
    if capacity > 0 {
        let mut bar = block + 0x14;
        let mut index = 0i32;
        loop {
            let dim = index < m.lw(panel + 0x18) as i32 || index < empty;
            for i in 0..3 {
                let value = m.lbu(panel + 0x20 + i);
                let value = if dim { value >> 2 } else { value };
                m.sb(bar + 4 + i, value);
            }
            index += 1;
            add_prim(m, ot, bar);
            if index >= m.lw(panel + 0x14) as i32 {
                break;
            }
            bar += 0x14;
        }
    }
    if m.lw(panel + 0x10) as i32 >= 100 {
        let sprite = block + 0x8c;
        let color = glyph_color(m, panel);
        glyph_3b00c(m, 1, 0xed, Y, 1, sprite, color);
        add_prim(m, ot, sprite);
    }
    let count = m.lw(panel + 0x10) as i32;
    if count >= 10 {
        let sprite = block + 0xa0;
        let tens = div10(count);
        let color = glyph_color(m, panel);
        let digit = tens.wrapping_sub(div10(tens).wrapping_mul(10));
        glyph_3b00c(m, digit as u32, 0xf3, Y, 1, sprite, color);
        add_prim(m, ot, sprite);
    }
    let count = m.lw(panel + 0x10) as i32;
    let sprite = block + 0xb4;
    let color = glyph_color(m, panel);
    let digit = count.wrapping_sub(div10(count).wrapping_mul(10));
    glyph_3b00c(m, digit as u32, 0xfb, Y, 1, sprite, color);
    add_prim(m, ot, sprite);
}

// ---- tree_40a24 -------------------------------------------------------------------

/// 800476F0 (JP; PAL 80046AA8, changed, `del:7 imm:2 ins:7 op:4 reg:4`):
/// area status of entity `index`: 2 when its x/z position is outside the
/// `[gp+0x33C]` box, 1 when the position 256 frames ahead (velocity
/// +0x38..+0x40) is outside it, or its predicted altitude passes the limits
/// of mission 0x2A (above 0x03A98000 or below 0xF15A0000) or 0x3A (above
/// 0xFFD00000), else 0. JP selects the limits by the context's mission
/// halfword +0x38C (PAL: the mode byte 0x800BAEB4).
pub fn area_status_46aa8(m: &mut Machine, index: u32) -> u32 {
    let table = crate::hud::entities(m);
    let area = m.gw(0x33c);
    let entity = m.lw((index << 2).wrapping_add(table));
    let min_x = m.lw(area + 0x10) as i32;
    let x = m.lw(entity + 8);
    if (x as i32) < min_x {
        return 2;
    }
    let max_x = m.lw(area + 0x14) as i32;
    if max_x < x as i32 {
        return 2;
    }
    let z = m.lw(entity + 0x10);
    let min_z = m.lw(area + 0x18) as i32;
    if (z as i32) < min_z {
        return 2;
    }
    let max_z = m.lw(area + 0x1c) as i32;
    if max_z < z as i32 {
        return 2;
    }
    let vx = m.lw(entity + 0x38);
    let vz = m.lw(entity + 0x40);
    let ahead_x = x.wrapping_add(vx << 8) as i32;
    let ahead_z = z.wrapping_add(vz << 8) as i32;
    if ahead_x < min_x || max_x < ahead_x || ahead_z < min_z || max_z < ahead_z {
        return 1;
    }
    let vy = m.lw(entity + 0x3c);
    let y = m.lw(entity + 0xc);
    let context = context(m);
    let ahead_y = y.wrapping_add(vy << 8) as i32;
    if m.lh(context + 0x38c) == 0x2a && (0x03a9_8000 < ahead_y || ahead_y < 0xf15a_0000u32 as i32) {
        return 1;
    }
    let context = crate::hud::context(m);
    if m.lh(context + 0x38c) == 0x3a {
        return u32::from((0xffd0_0000u32 as i32) < ahead_y);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::magic_div_add;

    #[test]
    fn jp_divisions_are_truncated_quotients() {
        for x in [
            0i32,
            1,
            29,
            30,
            31,
            59,
            1799,
            1800,
            1801,
            -1,
            -30,
            -31,
            i32::MAX,
            i32::MIN,
        ] {
            assert_eq!(magic_div_add(x, 0x8888_8889, 4), x / 30, "{x}");
            assert_eq!(magic_div_add(x, 0x8888_8889, 3), x / 15, "{x}");
            assert_eq!(magic_div_add(x, 0x91a2_b3c5, 10), x / 1800, "{x}");
        }
    }
}
