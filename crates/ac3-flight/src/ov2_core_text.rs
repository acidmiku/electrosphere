//! Overlay 2 text lists and font strings, translated from the original
//! instructions.
//!
//! The text block (`[0x800F5398]`, see [`TEXT`]) holds a list of
//! 0x18-byte records (+0 font, +2 offset pair, +3 flags: 1 format with
//! +0xC, 2 per-language positions; +4/+6 x/y, per language at +4 + 4 *
//! language), the localized string table and 256 visibility bytes.
//! [`draw_lists`] (800D0FC0) draws every visible record with
//! [`draw_string`] (800D04C8): one textured sprite per glyph from the font
//! records at 0x800F5104 (16 bytes: +0/+1 texture origin, +3 glyph height,
//! +4 line spacing, +6 texture page, +8 CLUT, +0xC glyph table of {u, v,
//! width, y offset}), glyph indexes from 0x800F5294, per-font letter-pair
//! adjustments and [`kerning`] (800CF484).
use super::{TEXT, call, context, framed, sext8, sext16};
use crate::machine::Machine;

/// Font records (16 bytes each).
pub const FONTS: u32 = 0x800f_5104;
/// Glyph index of each character code (256 bytes).
pub const GLYPHS: u32 = 0x800f_5294;

fn text_block(m: &mut Machine) -> u32 {
    m.lw(TEXT)
}

// ---- 800D0E90..800D0F20: list and visibility --------------------------------------------------

/// 800D0E90: no list; every record hidden. Its v0 is the loop's exit
/// test, 0 (the dispatch arm returns it).
pub fn reset_visibility(m: &mut Machine) {
    let block = text_block(m);
    m.sw(block, 0);
    for index in 0..0x100u32 {
        let block = text_block(m);
        m.sb(block.wrapping_add(index) + 0x10, 0);
    }
}
/// 800D0ECC: the record list and string table. Returns the text block.
pub fn set_lists(m: &mut Machine, list: u32, strings: u32) -> u32 {
    let block = text_block(m);
    m.sw(block, list);
    m.sw(block + 4, strings);
    block
}
/// 800D0EE4: visibility byte of record `index`. Returns text block +
/// `index`.
pub fn set_visibility(m: &mut Machine, index: u32, value: u32) -> u32 {
    let block = text_block(m);
    let at = block.wrapping_add(index);
    m.sb(at + 0x10, value as u8);
    at
}
/// 800D0F20: hide every record. Its v0 is the loop's exit test, 0 (the
/// dispatch arm returns it).
pub fn clear_visibility(m: &mut Machine) {
    for index in 0..0x100u32 {
        let block = text_block(m);
        m.sb(block.wrapping_add(index) + 0x10, 0);
    }
}

/// 800D0B28: string `index` of the current language (context +0x38C) in a
/// localized table: two levels of offsets with the low two bits masked.
pub fn string(m: &mut Machine, index: u32, table: u32) -> u32 {
    let ctx = context(m);
    let language = m.lw(ctx + 0x38c);
    let offset = m.lw(table.wrapping_add(language << 2).wrapping_add(4));
    let strings = table.wrapping_add((offset >> 2) << 2);
    let offset = m.lw((index << 2).wrapping_add(strings).wrapping_add(4));
    strings.wrapping_add(offset & !3)
}

// ---- 800D0FC0: text lists --------------------------------------------------------------------

/// 800D0FC0: draw every visible record of the list into `ot` from
/// `packet`; position = record x/y (per language when flag 2) plus its
/// offset pair; flag 1 formats the string with sprintf and +0xC first.
/// Returns the packet pointer.
pub fn draw_lists(m: &mut Machine, packet: u32, ot: u32) -> u32 {
    framed(m, 0xa0, |m, sp| {
        let mut packet = packet;
        let block = text_block(m);
        let list = m.lw(block);
        m.sw(block + 8, list);
        for index in 0..0x100u32 {
            let block = text_block(m);
            if m.lbu(block.wrapping_add(index) + 0x10) != 0 {
                let record = m.lw(block + 8);
                let pair = u32::from(m.lbu(record + 2));
                super::visual::get_offset(m, pair, sp + 0x70, sp + 0x72);
                let (x, y) = if m.lbu(record + 3) & 2 != 0 {
                    let ctx = context(m);
                    let language = m.lw(ctx + 0x38c);
                    let dx = m.lhu(sp + 0x70);
                    let at = record.wrapping_add(language << 2);
                    let x = m.lhu(at + 4);
                    let y = m.lhu(at + 6);
                    (x.wrapping_add(dx), y)
                } else {
                    let x = m.lhu(record + 4);
                    let dx = m.lhu(sp + 0x70);
                    let y = m.lhu(record + 6);
                    (x.wrapping_add(dx), y)
                };
                let y = y.wrapping_add(m.lhu(sp + 0x72));
                let block = text_block(m);
                let strings = m.lw(block + 4);
                let text = if m.lbu(record + 3) & 1 != 0 {
                    let format = string(m, index, strings);
                    let block = text_block(m);
                    let argument = m.lw(block + 0xc);
                    call(m, 0x800a_cd28, [sp + 0x20, format, argument, 0]);
                    sp + 0x20
                } else {
                    string(m, index, strings)
                };
                let font = sext16(m.lh(record));
                m.sw(sp + 0x10, sext16(x as i16));
                m.sw(sp + 0x14, sext16(y as i16));
                m.sw(sp + 0x18, 0x0080_8080);
                packet = draw_string(m, font, text, ot, packet);
            }
            let block = text_block(m);
            let cursor = m.lw(block + 8);
            m.sw(block + 8, cursor.wrapping_add(0x18));
        }
        packet
    })
}

// ---- 800D04C8: font strings ------------------------------------------------------------------

/// 800D04C8 (font, string, ot, packet; x, y, color on the stack at
/// `sp+0x10..`): one SPRT (code 0x66, semi-transparent) per glyph linked
/// into `[ot]`, then a DR_TPAGE with the font's page. Newline returns to x
/// and moves down by the line spacing + 1; carriage return is skipped. The
/// advance is the glyph width plus [`kerning`] with the next character,
/// after the per-font adjustments of the jump table at 0x800CABAC. Returns
/// the packet pointer after the DR_TPAGE.
pub fn draw_string(m: &mut Machine, font: u32, string: u32, ot: u32, packet: u32) -> u32 {
    let args = m.sp;
    let left = m.lw(args + 0x10);
    let top = m.lw(args + 0x14);
    let color = m.lw(args + 0x18);
    framed(m, 0x48, |m, sp| {
        m.sw(sp + 0x48, font);
        m.sw(sp + 0x50, ot);
        let record = FONTS.wrapping_add(font << 4);
        m.sw(sp + 0x10, top);
        let mut y = top;
        let mut sprite = packet;
        let length = call(m, 0x800a_c328, [string, 0, 0, 0]);
        m.sw(sp + 0x14, length);
        let mut x = left;
        let mut at = string;
        let mut index = 0i32;
        while index < length as i32 {
            let current = u32::from(m.lbu(at));
            at = at.wrapping_add(1);
            if current == 0xa {
                x = left;
                let spacing = sext8(m.lb(record + 4));
                y = y.wrapping_add(1).wrapping_add(spacing);
                index += 1;
                continue;
            }
            if current == 0xd {
                index += 1;
                continue;
            }
            let glyph = u32::from(m.lbu(GLYPHS.wrapping_add(current)));
            let table = m.lw(record + 0xc);
            let origin_u = u32::from(m.lbu(record));
            let origin_v = u32::from(m.lbu(record + 1));
            let entry = table.wrapping_add(glyph << 2);
            let mut width = u32::from(m.lbu(entry + 2));
            let u = u32::from(m.lbu(entry));
            let v = u32::from(m.lbu(entry + 1));
            let offset = sext8(m.lb(entry + 3));
            m.sw(sprite + 4, color | 0x6600_0000);
            let clut = u32::from(m.lhu(record + 8));
            let uv = origin_u.wrapping_add(u) | (origin_v.wrapping_add(v) << 8);
            m.sw(sprite + 0xc, uv | (clut << 16));
            let mut size = None;
            match font.wrapping_sub(5) {
                0..=10 => {
                    if (font == 6 || font == 8) && (glyph == 0 || glyph.wrapping_sub(0x10) < 0xa) {
                        width = 5;
                    }
                    if index > 0 {
                        let before = m.lbu(at.wrapping_sub(2));
                        x = latin_pairs(x, current, before);
                    }
                }
                11 | 12 => {
                    if current.wrapping_sub(1) < 6 {
                        size = Some(width | 0x14_0000);
                    } else {
                        if current == 7 {
                            width = 6;
                        }
                        if index > 0 {
                            let before = m.lbu(at.wrapping_sub(2));
                            x = slanted_pairs(x, current, before);
                        }
                    }
                }
                13..=15 => {
                    if glyph >= 0x9c {
                        size = Some(width | 0x14_0000);
                    }
                }
                16 if index > 0 && current == 0x6a => x = x.wrapping_sub(1),
                17 if index > 0 && (current == 0x54 || current == 0x56) => {
                    x = x.wrapping_sub(1);
                }
                _ => {}
            }
            let size = match size {
                Some(size) => size,
                None => width | (u32::from(m.lbu(record + 3)) << 16),
            };
            m.sw(sprite + 0x10, size);
            m.sh(sprite + 8, x as u16);
            m.sh(sprite + 0xa, y.wrapping_add(offset) as u16);
            let link = m.lw(ot);
            m.sw(sprite, link | 0x0400_0000);
            m.sw(ot, sprite & 0x00ff_ffff);
            let next = u32::from(m.lbu(at));
            sprite = sprite.wrapping_add(0x14);
            let adjust = kerning(font, current, next);
            x = x.wrapping_add(width.wrapping_add(adjust));
            index += 1;
        }
        m.sb(sprite + 3, 1);
        let page = u32::from(m.lhu(record + 6));
        m.sw(sprite + 4, (page & 0x9ff) | 0xe100_0000);
        let link = m.lw(ot);
        super::link_word(m, sprite, link);
        super::link_word(m, ot, sprite);
        sprite.wrapping_add(8)
    })
}

/// 800D0634..800D0730 (fonts 5..15, not the first character): 'l' after
/// a letter other than l, b, d, e, f, i, j, u moves left by 1; 't' after
/// one other than B, L, Q, W, Z, a, d, f, g, i, j, r, w, x, y, z moves
/// left by 1; '.' moves right by 1.
fn latin_pairs(x: u32, current: u32, before: u8) -> u32 {
    let mut x = x;
    if current == 0x6c && !b"lbdefiju".contains(&before) {
        x = x.wrapping_sub(1);
    }
    if current == 0x74 && !b"BLQWZadfgijrwxyz".contains(&before) {
        x = x.wrapping_sub(1);
    }
    if current == 0x2e {
        x = x.wrapping_add(1);
    }
    x
}

/// 800D0754..800D09D0 (fonts 16, 17, not the first character): '!' moves
/// right by 1; 'i', 't', 'g' and 'c' move left by 1 unless the previous
/// character is in their exception lists.
fn slanted_pairs(x: u32, current: u32, before: u8) -> u32 {
    let keep: &[u8] = match current {
        0x21 => return x.wrapping_add(1),
        0x69 => b"lvgMmrLGcV/sowfadEunRehzbZpNBkDSACxTH",
        0x74 => b"iuopsrxkaS\xe9\xc9\xe4A",
        0x67 => b"in\xe4raoe\xe9",
        0x63 => b"ao\xe9ipsnr\xe4A",
        _ => return x,
    };
    if keep.contains(&before) {
        x
    } else {
        x.wrapping_sub(1)
    }
}

// ---- 800CF484: kerning ------------------------------------------------------------------------

// The kerning routine is a pure decision tree (jump tables 0x800C9F54..
// 0x800CAA74 and compare chains on the next character). The arms below
// list, per font group and character, every next character whose result
// differs from that character's default. `.ov2core/kerning.py` evaluates
// the original instructions for every byte argument and checks that
// non-byte characters behave as unmatched; the unit test compares an FNV
// hash of all 23 x 256 x 256 results with that evaluation.

/// 800CF484 (font, character, next character): advance adjustment in
/// pixels (fonts 0x17 and above: 0).
pub fn kerning(font: u32, current: u32, next: u32) -> u32 {
    let adjust = match font {
        0 | 1 => kerning_0(current, next),
        2..=4 => kerning_1(current, next),
        5..=15 => kerning_2(font, current, next),
        16 | 17 => kerning_3(current, next),
        18..=20 => kerning_4(current, next),
        21 => kerning_5(current, next),
        22 => kerning_6(current, next),
        _ => 0,
    };
    adjust as u32
}

/// Fonts 0, 1.
fn kerning_0(current: u32, next: u32) -> i32 {
    match current {
        0x46 => match next {
            0x6f => -1,
            _ => 0,
        },
        0x50 => match next {
            0x61 => -1,
            _ => 0,
        },
        0x52 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x53 => match next {
            0x74 => -1,
            _ => 0,
        },
        0x61 => match next {
            0x74 => -1,
            _ => 0,
        },
        0x63 => match next {
            0x6f => -1,
            _ => 0,
        },
        0x65 => match next {
            0x74 => -2,
            0x6c | 0x78 => -1,
            _ => 0,
        },
        0x66 => match next {
            0x74 => -1,
            _ => 0,
        },
        0x6e => match next {
            0x74 => -1,
            _ => 0,
        },
        0x6f => match next {
            0x74 => -2,
            _ => 0,
        },
        0x72 => match next {
            0x61 | 0x64 | 0x65 | 0x66 | 0x6f | 0x70 | 0x73 | 0x74 => -1,
            _ => 0,
        },
        0x73 => match next {
            0x74 => -1,
            _ => 0,
        },
        0x74 => match next {
            0x65 | 0x6f | 0x74 | 0x75 | 0x79 => -1,
            _ => 0,
        },
        0x75 => match next {
            0x74 => -1,
            _ => 0,
        },
        0x78 => match next {
            0x63 => -1,
            _ => 0,
        },
        _ => 0,
    }
}
/// Fonts 2, 3, 4.
fn kerning_1(current: u32, next: u32) -> i32 {
    match current {
        0x4e => match next {
            0x65 => -1,
            _ => 0,
        },
        0x53 => match next {
            0x63 => -1,
            _ => 0,
        },
        0x63 => match next {
            0x68 | 0x69 => -1,
            _ => 0,
        },
        0x64 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x65 => match next {
            0x73 => -2,
            0x6e | 0x75 | 0x77 => -1,
            _ => 0,
        },
        0x66 => match next {
            0xe1 => -1,
            _ => 0,
        },
        0x67 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x69 => match next {
            0x64 => 1,
            _ => 0,
        },
        0x6a => match next {
            0x65 => -1,
            _ => 0,
        },
        0x6c => match next {
            0x65 | 0x69 | 0x6f => -1,
            _ => 0,
        },
        0x6e => match next {
            0x65 | 0x66 | 0x75 | 0xe7 => -1,
            _ => 0,
        },
        0x6f => match next {
            0x63 => -1,
            _ => 0,
        },
        0x70 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x72 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x73 => match next {
            0x63 | 0x70 | 0x79 | 0xed => -1,
            _ => 0,
        },
        0x75 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x77 => match next {
            0x69 => -1,
            _ => 0,
        },
        0x79 => match next {
            0x65 => -1,
            _ => 0,
        },
        0xed => match next {
            0x63 => -1,
            _ => 0,
        },
        _ => 0,
    }
}
/// Fonts 5..15 (6 and 8 without the "1" pair).
fn kerning_2(font: u32, current: u32, next: u32) -> i32 {
    match current {
        0x2e => 1,
        0x30 => match next {
            0x31 => -1,
            _ => 0,
        },
        0x31 if font != 6 && font != 8 => -1,
        0x43 => match next {
            0x68 => 0,
            _ => -1,
        },
        0x44 => -1,
        0x45 => match next {
            0x52 | 0x53 | 0x54 | 0x56 => -1,
            _ => 0,
        },
        0x46 => match next {
            0x41 | 0xc1 => -1,
            _ => 0,
        },
        0x49 => match next {
            0x53 | 0xd3 => -1,
            _ => 0,
        },
        0x4c => match next {
            0x4f | 0x53 | 0x55 | 0x65 => -1,
            _ => 0,
        },
        0x4d => match next {
            0x41 => -1,
            _ => 0,
        },
        0x4e => match next {
            0x41 | 0x43 | 0x4f | 0x53 | 0x59 | 0x65 | 0x6f => -1,
            _ => 0,
        },
        0x4f => match next {
            0x59 => -2,
            0x50 | 0x52 | 0x53 | 0x54 | 0x62 | 0x76 => -1,
            _ => 0,
        },
        0x50 => match next {
            0x45 | 0x4f | 0x52 | 0x6f | 0xc9 => -1,
            _ => 0,
        },
        0x52 => -1,
        0x54 => match next {
            0x41 | 0xd3 => -1,
            _ => 0,
        },
        0x55 => match next {
            0x54 => -1,
            _ => 0,
        },
        0x61 => match next {
            0x6a | 0x73 | 0x74 | 0x76 | 0x79 => -1,
            _ => 0,
        },
        0x64 => match next {
            0x21 => -1,
            _ => 0,
        },
        0x65 => match next {
            0x76 => -1,
            _ => 0,
        },
        0x67 => match next {
            0x65 | 0x74 => -2,
            0x61 | 0x69 | 0x6f => -1,
            _ => 0,
        },
        0x69 => match next {
            0x73 | 0x74 | 0x76 => -1,
            _ => 0,
        },
        0x6c => match next {
            0x6f | 0xf3 => -1,
            _ => 0,
        },
        0x6e => -1,
        0x6f => match next {
            0x67 | 0x6e | 0x70 | 0x72 | 0x73 | 0x75 | 0x76 => -1,
            _ => 0,
        },
        0x72 => match next {
            0x61 | 0x63 | 0x64 | 0x65 | 0x66 | 0x69 | 0x6d | 0x6f | 0x72 | 0x73 | 0x74 | 0x75
            | 0xe9 | 0xfc => -2,
            _ => -1,
        },
        0x74 => match next {
            0x21 => -1,
            _ => 0,
        },
        0x76 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x77 => match next {
            0x68 | 0x69 => 1,
            _ => 0,
        },
        0xf1 => -1,
        _ => 0,
    }
}
/// Fonts 16, 17.
fn kerning_3(current: u32, next: u32) -> i32 {
    match current {
        0x2d => 1,
        0x41 => match next {
            0x52 | 0x55 | 0x69 | 0x73 => 1,
            _ => 0,
        },
        0x42 => match next {
            0x4d => 1,
            _ => 0,
        },
        0x45 => match next {
            0x53 | 0x6e | 0x78 => -1,
            _ => 0,
        },
        0x46 => match next {
            0x46 => 1,
            _ => 0,
        },
        0x47 => match next {
            0x61 | 0x65 | 0x75 => 1,
            _ => 0,
        },
        0x4a => match next {
            0x75 => 1,
            _ => 0,
        },
        0x4d => match next {
            0x52 | 0x53 | 0x61 | 0x65 | 0x69 | 0x6f | 0x72 | 0x75 => 0,
            _ => -1,
        },
        0x4e => match next {
            0x55 | 0x61 | 0x65 => 1,
            _ => 0,
        },
        0x4f => match next {
            0x46 | 0x4e | 0x55 | 0x66 | 0x70 | 0x75 => 1,
            _ => 0,
        },
        0x50 => match next {
            0x45 | 0x75 => 1,
            _ => 0,
        },
        0x52 => match next {
            0x42 | 0x4f | 0x61 | 0x65 | 0x69 => 1,
            _ => 0,
        },
        0x53 => match next {
            0x2f | 0x61 | 0x65 | 0x69 | 0x70 => 1,
            _ => 0,
        },
        0x56 => match next {
            0x69 => 1,
            _ => 0,
        },
        0x57 => match next {
            0x61 => 1,
            0x69 => 2,
            _ => 0,
        },
        0x61 => match next {
            0x2f | 0x3b | 0x61 | 0x62 | 0x65 | 0x66 | 0x67 | 0x69 | 0x6b | 0x6c | 0x6d | 0x6e
            | 0x6f | 0x70 | 0x71 | 0x72 | 0x73 | 0x74 | 0x75 | 0x76 | 0x77 | 0x7a | 0xdf | 0xe9
            | 0xf1 => 0,
            _ => -1,
        },
        0x63 => match next {
            0x68 => -1,
            _ => 0,
        },
        0x65 => match next {
            0x64 => -1,
            0x65 | 0x73 | 0x74 => 1,
            _ => 0,
        },
        0x67 => match next {
            0x7a => -1,
            _ => 0,
        },
        0x68 => match next {
            0x79 => -1,
            _ => 0,
        },
        0x69 => match next {
            0x61 | 0x62 | 0x65 | 0x69 | 0x6b | 0x6c | 0x6d | 0x6e | 0x6f | 0x70 | 0x71 | 0x72
            | 0x73 | 0x75 | 0x76 | 0x7a | 0xe8 | 0xe9 => 0,
            0xf9 => 1,
            _ => -1,
        },
        0x6b => match next {
            0x61 | 0x62 | 0x65 | 0x69 | 0x6b | 0x70 | 0x73 | 0x74 | 0x75 => 1,
            _ => 0,
        },
        0x6c => match next {
            0xd3 => -1,
            _ => 0,
        },
        0x6e => match next {
            0x2f | 0x61 | 0x62 | 0x65 | 0x69 | 0x6b | 0x6d | 0x6f | 0x70 | 0x71 | 0x73 | 0x74
            | 0x75 | 0x7a | 0xe9 | 0xf6 => 0,
            _ => -1,
        },
        0x6f => match next {
            0x2f | 0x61 | 0x62 | 0x65 | 0x69 | 0x6b | 0x6d | 0x70 | 0x72 | 0x73 | 0x75 | 0x7a
            | 0xdf | 0xf9 => 1,
            _ => 0,
        },
        0x70 => match next {
            0x2f | 0x61 | 0x65 | 0x69 | 0x6d | 0x6f | 0x72 | 0x75 => 0,
            _ => -1,
        },
        0x72 => match next {
            0x2e | 0x53 | 0x61 | 0x62 | 0x63 | 0x65 | 0x69 | 0x6b | 0x6d | 0x73 => 0,
            _ => -1,
        },
        0x73 => match next {
            0x61 | 0x65 | 0x69 | 0x6b | 0x6d | 0x6f | 0x70 | 0x73 | 0x75 | 0xfa => 1,
            _ => 0,
        },
        0x74 => match next {
            0x79 | 0xed | 0xf3 => -1,
            0x2f | 0x61 | 0x69 => 1,
            _ => 0,
        },
        0x75 => match next {
            0x79 => -1,
            0x61 | 0x65 | 0x69 | 0x73 => 1,
            _ => 0,
        },
        0x76 => match next {
            0x65 | 0x69 => 1,
            _ => 0,
        },
        0x77 => match next {
            0x61 | 0x65 | 0x69 | 0x6b | 0x70 => 1,
            _ => 0,
        },
        0x78 => match next {
            0x69 | 0x70 => 1,
            _ => 0,
        },
        0xdf => match next {
            0x62 | 0x65 => 1,
            _ => 0,
        },
        0xe1 => match next {
            0x69 => 1,
            _ => 0,
        },
        0xe4 => match next {
            0x68 | 0x72 | 0x75 => 1,
            _ => 0,
        },
        0xe8 => match next {
            0x74 => 1,
            _ => 0,
        },
        0xe9 => match next {
            0x61 | 0x65 | 0x70 | 0x71 | 0x72 | 0x73 | 0x75 => 1,
            _ => 0,
        },
        0xf6 => match next {
            0x73 => 1,
            _ => 0,
        },
        _ => 0,
    }
}
/// Fonts 18, 19, 20.
fn kerning_4(current: u32, next: u32) -> i32 {
    match current {
        0x43 => match next {
            0x54 => -1,
            _ => 0,
        },
        0x65 => match next {
            0x6c => -1,
            _ => 0,
        },
        0x72 => match next {
            0x6c => -1,
            _ => 0,
        },
        _ => 0,
    }
}
/// Fonts 21.
fn kerning_5(current: u32, next: u32) -> i32 {
    match current {
        0x28 => match next {
            0x4d => 1,
            _ => 0,
        },
        0x45 => match next {
            0x44 | 0x53 => -1,
            _ => 0,
        },
        0x4c => match next {
            0x54 => -2,
            _ => 0,
        },
        0x4d => match next {
            0x43 => -1,
            _ => 0,
        },
        0x54 => match next {
            0x6f => -1,
            _ => 0,
        },
        0x61 => -1,
        0x65 => match next {
            0x73 => -1,
            _ => 0,
        },
        0x67 => match next {
            0x6f => -1,
            _ => 0,
        },
        0x68 => match next {
            0x69 => 1,
            _ => 0,
        },
        0x75 => match next {
            0x6b => -1,
            _ => 0,
        },
        _ => 0,
    }
}
/// Fonts 22.
fn kerning_6(current: u32, next: u32) -> i32 {
    match current {
        0x28 => match next {
            0x43 => -1,
            _ => 0,
        },
        0x41 => match next {
            0x4c => 1,
            _ => 0,
        },
        0x47 => match next {
            0x72 => 1,
            _ => 0,
        },
        0x48 => match next {
            0x61 => -1,
            _ => 0,
        },
        0x4c => match next {
            0x54 => -1,
            _ => 0,
        },
        0x54 => match next {
            0x61 => -1,
            _ => 0,
        },
        0x56 => -1,
        0x59 => -1,
        0x63 => match next {
            0x75 => -1,
            _ => 0,
        },
        0x65 => match next {
            0x6e => -1,
            _ => 0,
        },
        0x69 => match next {
            0x61 | 0x63 | 0x64 | 0x7a => -1,
            _ => 0,
        },
        0x6b => match next {
            0x61 | 0x73 => -1,
            _ => 0,
        },
        0x6f => match next {
            0x6a => -2,
            0x64 | 0x73 | 0x7a => -1,
            _ => 0,
        },
        0x70 => match next {
            0x65 => -1,
            _ => 0,
        },
        0x72 => match next {
            0x61 | 0x65 => -1,
            _ => 0,
        },
        0x75 => match next {
            0x63 => -1,
            _ => 0,
        },
        _ => 0,
    }
}

// ---- dispatch -----------------------------------------------------------------------------------

/// Text routines by original address (asked by `ov2_core::dispatch` while
/// module 2 is loaded). 800D04C8 reads its x, y and color at `sp+0x10..`.
pub(crate) fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x800d_0e90 => {
            reset_visibility(m);
            0
        }
        0x800d_0ecc => set_lists(m, a0, a1),
        0x800d_0ee4 => set_visibility(m, a0, a1),
        0x800d_0f20 => {
            clear_visibility(m);
            0
        }
        0x800d_0b28 => string(m, a0, a1),
        0x800d_0fc0 => draw_lists(m, a0, a1),
        0x800d_04c8 => draw_string(m, a0, a1, a2, a3),
        0x800c_f484 => kerning(a0, a1, a2),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a of the low byte of every result for fonts 0..0x17, all byte
    /// characters and next characters, as `.ov2core/kerning.py --hash`
    /// computes it from the original instructions.
    #[test]
    fn kerning_matches_the_instruction_evaluation() {
        let mut hash: u32 = 0x811c_9dc5;
        for font in 0..0x18 {
            for current in 0..0x100 {
                for next in 0..0x100 {
                    let value = kerning(font, current, next);
                    hash = (hash ^ (value & 0xff)).wrapping_mul(0x0100_0193);
                }
            }
        }
        assert_eq!(hash, 0xecf9_ca7a);
        assert_eq!(kerning(0, 0x100, 0x6f), 0, "non-byte character");
        assert_eq!(kerning(0x8000_0000, 0x46, 0x6f), 0, "font out of range");
        assert_eq!(kerning(0, 0x46, 0x6f), (-1i32) as u32);
    }

    #[test]
    fn pair_adjustments() {
        assert_eq!(latin_pairs(10, 0x6c, b'a'), 9);
        assert_eq!(latin_pairs(10, 0x6c, b'b'), 10);
        assert_eq!(latin_pairs(10, 0x74, b'Z'), 10);
        assert_eq!(latin_pairs(10, 0x2e, b'x'), 11);
        assert_eq!(slanted_pairs(10, 0x21, b'x'), 11);
        assert_eq!(slanted_pairs(10, 0x69, b'H'), 10);
        assert_eq!(slanted_pairs(10, 0x63, 0xe4), 10);
        assert_eq!(slanted_pairs(10, 0x63, b'q'), 9);
    }
}
