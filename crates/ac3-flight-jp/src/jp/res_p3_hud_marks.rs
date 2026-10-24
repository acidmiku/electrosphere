//! Patch of the `hud` target-marker group (generated `hud::tree_3e828`)
//! whose JP instructions differ from PAL, part 3 of the resident wave:
//! 8003F520 with the JP record +4 marker (80040900). Listed in
//! [`REPLACES`](super::REPLACES); the function keeps the generated name.
use crate::hud::tree_3e828::{arrow_3fd24, box_3f28c, label_40040, status_3ff34};
use crate::hud::{self, glyph_3b00c};
use crate::machine::{Machine, SmallData};

/// `[gp+0x444]`: marker list (+0/+4 per-buffer blocks, +8..+0x14 counts).
const LIST: u32 = 0x444;

/// `[gp+0x448]`: this frame's marker block.
const BLOCK: u32 = 0x448;

/// 800C6FD0: per-entity marker flags.
const FLAGS: u32 = 0x800c_6fd0;

/// 800C67A0: per-entity screen records (+0 kind, +2 shape, +3 label,
/// +5 status, +6 half size, +8 x, +A y).
const RECORDS: u32 = 0x800c_67a0;

/// 800C6ED0: per-entity distance records (+0 flags, +4 distance).
const DISTANCES: u32 = 0x800c_6ed0;

/// 800C6D60: HUD state word (bit 1: blink phase).
const STATE: u32 = 0x800c_6d60;

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

/// 8003F520 (JP; PAL 8003ED64, changed, `call+:1 del:9 imm:44 ins:23 op:3
/// reg:10`): per-entity markers. `color` (the box color word) carries
/// over from the previous entity when the record kind is not 1..3 (JP
/// leaves the caller's `fp` as its first value, PAL sets 0xFF00; this
/// keeps 0xFF00: the kinds of the screen records are 0..3). JP adds the
/// record +4 marker (80040900 at the box's right/top unless 0xFF) after
/// the distance and label, for every kind; its frame is 0x78 (PAL 0x70).
pub fn targets_3ed64(m: &mut Machine) {
    m.push_frame(0x78);
    targets_body(m);
    m.pop_frame(0x78);
}

/// Body of [`targets_3ed64`] in its frame.
fn targets_body(m: &mut Machine) {
    let mut color: u32 = 0xff00;
    for index in 0..64u32 {
        let flags_at = FLAGS + index * 4;
        if m.lw(flags_at) & 0x1000 == 0 {
            continue;
        }
        let record = m.lw(RECORDS + index * 4);
        if record == 0 {
            continue;
        }
        let kind = m.lbu(record);
        if kind == 0 {
            continue;
        }
        match kind {
            1 => color = 0xff00,
            2 => {
                color = if m.lw(STATE) & 2 != 0 { 0x0040_ff40 } else { 0 };
            }
            3 => color = 0xff,
            _ => {}
        }
        // Box edges as halfwords (left, top, bottom) and the right edge.
        let (left, top, bottom, right);
        if m.lw(flags_at) & 0x0004_0000 != 0 && m.lbu(record + 5) != 1 {
            let x = u32::from(m.lhu(record + 8));
            let size = u32::from(m.lhu(record + 6));
            left = x.wrapping_sub(size) as u16;
            let y = u32::from(m.lhu(record + 0xa)).wrapping_sub(4) as u16;
            right = x.wrapping_add(size);
            top = y;
            bottom = y;
        } else {
            let x = u32::from(m.lhu(record + 8));
            let size = u32::from(m.lhu(record + 6));
            left = x.wrapping_sub(size) as u16;
            let y = u32::from(m.lhu(record + 0xa));
            right = x.wrapping_add(size);
            top = y.wrapping_sub(size).wrapping_sub(4) as u16;
            bottom = y.wrapping_add(size) as u16;
        }
        let left_x = left as i16 as u32;
        if m.lbu(record) != 1 {
            let distance = m.lw(DISTANCES + index * 4);
            if distance == 0 {
                continue;
            }
            if m.lw(flags_at) & 0x400 != 0 {
                arrow_3fd24(m, index);
            }
            distance_digits(m, distance, right, bottom);
            let label = u32::from(m.lbu(record + 3));
            label_40040(m, label, left_x, top as i16 as u32);
        }
        // JP: the record +4 marker at the box's right/top.
        let marker = m.lbu(record + 4);
        if marker != 0xff {
            let x = right as u16 as i16 as u32;
            crate::jp::res_p3_resident::label_40900(m, u32::from(marker), x, top as i16 as u32);
        }
        let status = m.lbu(record + 5);
        match status {
            1 | 2 => {
                status_3ff34(m, u32::from(status), left_x, bottom as i16 as u32);
            }
            3 => {
                color = 0x00ff_4000;
                let label = u32::from(m.lbu(record + 3));
                label_40040(m, label, left_x, top as i16 as u32);
                let status = u32::from(m.lbu(record + 5));
                status_3ff34(m, status, left_x, bottom as i16 as u32);
            }
            _ => {}
        }
        if m.lw(flags_at) & 0x0004_0000 != 0 && m.lbu(record) == 1 {
            let label = u32::from(m.lbu(record + 3));
            label_40040(m, label, left_x, top as i16 as u32);
            continue;
        }
        let options = m.gw(0x340);
        if m.lw(options) & 4 != 0 {
            hud::marker_shapes::corner_ticks_3f9a8(m, index, color);
            continue;
        }
        match m.lbu(record + 2) & 0xf {
            1 => box_3f28c(m, index, color),
            2 => hud::marker_shapes::hexagon_3f54c(m, index, color),
            3 => hud::marker_shapes::corner_ticks_3f9a8(m, index, color),
            _ => {}
        }
    }
}

/// 8003F520's distance: four glyphs (font 2, green) of `[record+4] >> 2`
/// right of the box at (right + 2.., bottom), thousands first.
fn distance_digits(m: &mut Machine, distance: u32, right: u32, bottom: u16) {
    let x = right as u16 as i16 as u32;
    let y = bottom as i16 as u32;
    let raw = m.lw(distance + 4) as i32;
    let value = raw >> 2;
    let sign = raw >> 31;
    let high = |magic: u32| ((i64::from(value) * i64::from(magic as i32)) >> 32) as i32;
    let mut sprite = next(m, 0x14, 0x1508, 0x14);
    let thousands = (high(0x1062_4dd3) >> 6).wrapping_sub(sign);
    glyph_3b00c(m, thousands as u32, x.wrapping_add(2), y, 2, sprite, 0);
    sprite += 0x14;
    let hundreds = (high(0x51eb_851f) >> 5).wrapping_sub(sign);
    count(m, 0x14);
    let digit = hundreds.wrapping_sub(thousands.wrapping_mul(10));
    glyph_3b00c(m, digit as u32, x.wrapping_add(7), y, 2, sprite, 0);
    sprite += 0x14;
    let tens = (high(0x6666_6667) >> 2).wrapping_sub(sign);
    count(m, 0x14);
    let digit = tens.wrapping_sub(hundreds.wrapping_mul(10));
    glyph_3b00c(m, digit as u32, x.wrapping_add(0xc), y, 2, sprite, 0);
    sprite += 0x14;
    let digit = value.wrapping_sub(tens.wrapping_mul(10));
    count(m, 0x14);
    glyph_3b00c(m, digit as u32, x.wrapping_add(0x11), y, 2, sprite, 0);
    count(m, 0x14);
}
