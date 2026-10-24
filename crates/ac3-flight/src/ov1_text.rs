//! Overlay 1 (front-end menus): text layout and drawing on the
//! address-preserving machine: the glyph renderer `800CFAF0`, its spacing
//! table `800CEAAC`, the string-bank lookup `800D0150` and the text table
//! `[0x800D8740]` (+0 descriptor table of 24-byte records, +4 string bank,
//! +8 current descriptor, +0xC `sprintf` argument, +0x10 256 visibility
//! bytes) with its helpers `800D04B8`..`800D0548` and the per-frame
//! drawing `800D05E8`.
//!
//! A text descriptor: +0 font (halfword), +2 offset slot (byte), +3 flags
//! (bit 0: format the string with `sprintf` and +0xC, bit 1: per-language
//! positions), +4/+6 position, +4 + 4 * language per-language positions.
//! A font descriptor (16 bytes at `0x800D7AFC`): +0/+1 texture U/V base, +3
//! glyph height, +4 line spacing, +6 texture page, +8 CLUT, +0xC glyph
//! table (4-byte records: U, V, width, signed y displacement). Character
//! codes map to glyphs through the 256 bytes at `0x800D7C8C`.
use crate::{
    common::CONTEXT,
    dispatch,
    machine::{Machine, SmallData},
    ov1::{TEXT, lb, lbu, lh, lhu, off},
    ov1_script,
};

/// 800D7AFC: sixteen-byte font descriptors.
const FONTS: u32 = 0x800d_7afc;
/// 800D7C8C: character code to glyph index.
const GLYPH_MAP: u32 = 0x800d_7c8c;
/// 800AC328 strlen (libc, owned by `sdk.rs`).
pub const STRLEN: u32 = 0x800a_c328;
/// 800ACD28 sprintf (libc, owned by `sdk.rs`).
pub const SPRINTF: u32 = 0x800a_cd28;

fn table(m: &mut Machine) -> u32 {
    m.lw(TEXT)
}

// ---- spacing ---------------------------------------------------------------------

fn among(value: u32, set: &[u8]) -> bool {
    value < 0x100 && set.contains(&(value as u8))
}

/// 800CEAAC: the signed advance adjustment (as the original `v0`) after
/// character `left` when `right` follows, for `font`. The original is a
/// jump table per font family (`0x800C9EF4`) into per-family jump tables on
/// `left` and comparison chains on `right`; fonts 23 and up and left codes
/// outside the tables give 0. The terminating zero participates as
/// `right`.
pub fn kerning(font: u32, left: u32, right: u32) -> u32 {
    if font >= 23 || left > 0xff {
        return 0;
    }
    let value: i32 = match font {
        0 | 1 => kerning_small(left as u8, right),
        2..=4 => kerning_language(left as u8, right),
        5..=15 => kerning_menu(font, left as u8, right),
        16 | 17 => kerning_proportional(left as u8, right),
        18..=20 => match left as u8 {
            b'e' | b'r' if right == u32::from(b'l') => -1,
            b'C' if right == u32::from(b'T') => -1,
            _ => 0,
        },
        21 => kerning_21(left as u8, right),
        _ => kerning_22(left as u8, right),
    };
    value as u32
}

/// Fonts 0 and 1 (800CEAD4, table `0x800C9F54` for `left` 0x46..0x78).
fn kerning_small(left: u8, right: u32) -> i32 {
    let is = |set: &[u8]| among(right, set);
    match left {
        b'e' | b'o' if is(b"t") => -2,
        b'e' if is(b"xl") => -1,
        b'F' | b'c' if is(b"o") => -1,
        b'P' if is(b"a") => -1,
        b'R' if is(b"e") => -1,
        b'S' | b'a' | b'f' | b'n' | b's' | b'u' if is(b"t") => -1,
        b'r' if is(b"sadpoeft") => -1,
        b't' if is(b"ytuoe") => -1,
        b'x' if is(b"c") => -1,
        _ => 0,
    }
}

/// Fonts 2..4 (800CEBF8, table `0x800CA024` for `left` 0x4E..0xED).
fn kerning_language(left: u8, right: u32) -> i32 {
    let is = |set: &[u8]| among(right, set);
    match left {
        b'i' if is(b"d") => 1,
        b'e' if is(b"s") => -2,
        b'N' | b'd' | b'g' | b'j' | b'p' | b'r' | b'u' | b'y' if is(b"e") => -1,
        b'S' | b'o' | 0xed if is(b"c") => -1,
        b'c' if is(b"hi") => -1,
        b'e' if is(b"uwn") => -1,
        b'f' if is(b"\xe1") => -1,
        b'l' if is(b"ioe") => -1,
        b'n' if is(b"\xe7feu") => -1,
        b's' if is(b"cpy\xed") => -1,
        b'w' if is(b"i") => -1,
        _ => 0,
    }
}

/// Fonts 5..15 (800CED8C, table `0x800CA2A4` for `left` 0x2E..0xF1).
fn kerning_menu(font: u32, left: u8, right: u32) -> i32 {
    let is = |set: &[u8]| among(right, set);
    match left {
        b'.' => 1,
        b'w' if is(b"hi") => 1,
        b'1' if font == 6 || font == 8 => 0,
        b'C' if is(b"h") => 0,
        b'O' if is(b"Y") => -2,
        b'g' if is(b"te") => -2,
        b'r' if is(b"sdctoeui\xe9mfra\xfc") => -2,
        b'1' | b'C' | b'D' | b'R' | b'n' | 0xf1 | b'r' => -1,
        b'0' if is(b"1") => -1,
        b'E' if is(b"STVR") => -1,
        b'F' if is(b"A\xc1") => -1,
        b'I' if is(b"\xd3S") => -1,
        b'L' if is(b"OeUS") => -1,
        b'M' if is(b"A") => -1,
        b'N' if is(b"eoACYSO") => -1,
        b'O' if is(b"STPRbv") => -1,
        b'P' if is(b"RoE\xc9O") => -1,
        b'T' if is(b"A\xd3") => -1,
        b'U' if is(b"T") => -1,
        b'a' if is(b"stvyj") => -1,
        b'd' | b't' if is(b"!") => -1,
        b'e' if is(b"v") => -1,
        b'g' if is(b"iao") => -1,
        b'i' if is(b"stv") => -1,
        b'l' if is(b"o\xf3") => -1,
        b'o' if is(b"svrungp") => -1,
        b'v' if is(b"e") => -1,
        _ => 0,
    }
}

/// Fonts 16 and 17 (800CF10C, table `0x800CA5B4` for `left` 0x2D..0xF6):
/// six letters default to -1 unless the right character is listed.
fn kerning_proportional(left: u8, right: u32) -> i32 {
    let is = |set: &[u8]| among(right, set);
    let not = |set: &[u8]| -i32::from(!among(right, set));
    match left {
        b'M' => return not(b"oiaeuSrR"),
        b'a' => return not(b"tub/wsrmanloek\xf1zi;vpg\xe9fq\xdf"),
        b'i' if right == 0xf9 => return 1,
        b'i' => return not(b"brzeosav\xe8quplmn\xe9ik"),
        b'n' => return not(b"t/kesozapi\xf6ubq\xe9m"),
        b'p' => return not(b"/euoarim"),
        b'r' => return not(b".maiesSkbc"),
        _ => {}
    }
    match left {
        b'W' if is(b"i") => 2,
        b'E' if is(b"nSx") => -1,
        b'c' if is(b"h") => -1,
        b'e' if is(b"d") => -1,
        b'g' if is(b"z") => -1,
        b'h' | b'u' if is(b"y") => -1,
        b'l' if is(b"\xd3") => -1,
        b't' if is(b"\xf3y\xed") => -1,
        b'-' => 1,
        b'A' if is(b"URis") => 1,
        b'B' if is(b"M") => 1,
        b'F' if is(b"F") => 1,
        b'G' if is(b"aue") => 1,
        b'J' if is(b"u") => 1,
        b'N' if is(b"aeU") => 1,
        b'O' if is(b"pUNfFu") => 1,
        b'P' if is(b"uE") => 1,
        b'R' if is(b"aeiOB") => 1,
        b'S' if is(b"pa/ei") => 1,
        b'V' | 0xe1 if is(b"i") => 1,
        b'W' if is(b"a") => 1,
        b'e' if is(b"ste") => 1,
        b'k' if is(b"seatpibuk") => 1,
        b'o' if is(b"/bamkiusre\xf9\xdfzp") => 1,
        b's' if is(b"eiaosmpuk\xfa") => 1,
        b't' if is(b"i/a") => 1,
        b'u' if is(b"asei") => 1,
        b'v' if is(b"ie") => 1,
        b'w' if is(b"ipeka") => 1,
        b'x' if is(b"pi") => 1,
        0xdf if is(b"eb") => 1,
        0xe4 if is(b"rhu") => 1,
        0xe8 if is(b"t") => 1,
        0xe9 if is(b"resaqup") => 1,
        0xf6 if is(b"s") => 1,
        _ => 0,
    }
}

/// Font 21 (800CF928, table `0x800CA8DC` for `left` 0x28..0x75).
fn kerning_21(left: u8, right: u32) -> i32 {
    let is = |set: &[u8]| among(right, set);
    match left {
        b'(' if is(b"M") => 1,
        b'h' if is(b"i") => 1,
        b'L' if is(b"T") => -2,
        b'E' if is(b"SD") => -1,
        b'M' if is(b"C") => -1,
        b'T' if is(b"o") => -1,
        b'a' => -1,
        b'e' if is(b"s") => -1,
        b'g' if is(b"o") => -1,
        b'u' if is(b"k") => -1,
        _ => 0,
    }
}

/// Font 22 (800CF9B8, table `0x800CAA14` for `left` 0x28..0x75).
fn kerning_22(left: u8, right: u32) -> i32 {
    let is = |set: &[u8]| among(right, set);
    match left {
        b'A' if is(b"L") => 1,
        b'G' if is(b"r") => 1,
        b'o' if is(b"j") => -2,
        b'(' if is(b"C") => -1,
        b'H' | b'T' if is(b"a") => -1,
        b'L' if is(b"T") => -1,
        b'V' | b'Y' => -1,
        b'c' if is(b"u") => -1,
        b'e' if is(b"n") => -1,
        b'i' if is(b"adcz") => -1,
        b'k' if is(b"sa") => -1,
        b'o' if is(b"dzs") => -1,
        b'p' if is(b"e") => -1,
        b'r' if is(b"ea") => -1,
        b'u' if is(b"c") => -1,
        _ => 0,
    }
}

/// The pen adjustment `800CFAF0` applies before a character that has a
/// preceding source byte (`previous`, which may be a line break):
/// branches `0x800CFC5C`..`0x800D0038`.
fn pre_adjust(font: u32, previous: u32, current: u32) -> i32 {
    let not_after = |set: &[u8]| -i32::from(!among(previous, set));
    match font {
        5..=15 => match current as u8 {
            b'l' => not_after(b"lbdefiju"),
            b't' => not_after(b"BLQWZadfgijrwxyz"),
            b'.' => 1,
            _ => 0,
        },
        16 | 17 => match current as u8 {
            b'g' => not_after(b"in\xe4raoe\xe9"),
            b'i' => not_after(b"lvgMmrLGcV/sowfadEunRehzbZpNBkDSACxTH"),
            b't' => not_after(b"iuopsrxkaS\xe9\xc9\xe4A"),
            b'c' => not_after(b"ao\xe9ipsnr\xe4A"),
            b'!' => 1,
            _ => 0,
        },
        21 if current == u32::from(b'j') => -1,
        22 if current == u32::from(b'T') || current == u32::from(b'V') => -1,
        _ => 0,
    }
}

// ---- glyph renderer -----------------------------------------------------------------

/// 800CFAF0: draw `string` in `font` at the stack arguments x (`sp+0x10`),
/// y (`sp+0x14`) with colour word `sp+0x18`: one SPRT (0x66) per glyph
/// from `packet`, each linked at the head of the ordering-table entry `ot`
/// (the entry is overwritten, length byte included), then the font's
/// texture page as a DR_MODE (E1) in front of them. Line feed restarts at
/// x on the next line (font +4 + 1 lower), carriage return is skipped.
/// Fonts 6/8 draw glyph 0 and glyphs 16..25 5 wide, fonts 16/17 code 7 6
/// wide and codes 1..6 and fonts 18..20 glyphs from 0x9C 20 high; the pen
/// advances by the glyph width and [`kerning`], adjusted per family before
/// glyphs with a preceding byte. Returns the next free packet address.
pub fn draw_text(m: &mut Machine, font: u32, string: u32, ot: u32, packet: u32) -> u32 {
    let args = m.sp;
    m.sw(args, font);
    m.sw(off(args, 8), ot);
    let x0 = m.lw(off(args, 0x10));
    let mut y = m.lw(off(args, 0x14));
    let color = m.lw(off(args, 0x18));
    m.push_frame(0x48);
    let descriptor = off(FONTS, font << 4);
    let length = dispatch::call(m, STRLEN, [string, 0, 0, 0]);
    let mut x = x0;
    let mut sprite = packet;
    let mut source = string;
    let mut index = 0u32;
    while (index as i32) < (length as i32) {
        let c = lbu(m, source);
        source = source.wrapping_add(1);
        index = index.wrapping_add(1);
        if c == 0xa {
            x = x0;
            let spacing = lb(m, off(descriptor, 4));
            y = y.wrapping_add(1).wrapping_add(spacing);
            continue;
        }
        if c == 0xd {
            continue;
        }
        let i = index.wrapping_sub(1);
        let glyph = lbu(m, off(GLYPH_MAP, c));
        let glyphs = m.lw(off(descriptor, 0xc));
        let u0 = lbu(m, descriptor);
        let v0 = lbu(m, off(descriptor, 1));
        let record = off(glyphs, glyph << 2);
        let mut width = lbu(m, off(record, 2));
        let u = lbu(m, record);
        let v = lbu(m, off(record, 1));
        let dy = lb(m, off(record, 3));
        m.sw(off(sprite, 4), color | 0x6600_0000);
        let clut = lhu(m, off(descriptor, 8));
        let uv = u0.wrapping_add(u) | v0.wrapping_add(v) << 8 | clut << 16;
        m.sw(off(sprite, 0xc), uv);
        let previous = if i as i32 > 0 {
            lbu(m, source.wrapping_sub(2))
        } else {
            0
        };
        // Glyph height: the font's, or a fixed 20 for the special cases.
        let mut height = None;
        match font {
            5..=15 => {
                if (font == 6 || font == 8) && (glyph == 0 || glyph.wrapping_sub(0x10) < 10) {
                    width = 5;
                }
                if i as i32 > 0 {
                    x = x.wrapping_add(pre_adjust(font, previous, c) as u32);
                }
            }
            16 | 17 => {
                if c.wrapping_sub(1) < 6 {
                    height = Some(0x14);
                } else {
                    if c == 7 {
                        width = 6;
                    }
                    if i as i32 > 0 {
                        x = x.wrapping_add(pre_adjust(font, previous, c) as u32);
                    }
                }
            }
            18..=20 => {
                if (glyph as i32) >= 0x9c {
                    height = Some(0x14);
                }
            }
            21 | 22 if i as i32 > 0 => {
                x = x.wrapping_add(pre_adjust(font, previous, c) as u32);
            }
            _ => {}
        }
        let height = match height {
            Some(h) => h,
            None => lbu(m, off(descriptor, 3)),
        };
        m.sw(off(sprite, 0x10), width | height << 16);
        m.sh(off(sprite, 8), x as u16);
        m.sh(off(sprite, 0xa), y.wrapping_add(dy) as u16);
        let link = m.lw(ot);
        m.sw(sprite, link | 0x0400_0000);
        m.sw(ot, sprite & 0x00ff_ffff);
        let next = lbu(m, source);
        sprite = sprite.wrapping_add(0x14);
        let spacing = kerning(font, c, next);
        x = x.wrapping_add(width.wrapping_add(spacing));
    }
    m.sb(off(sprite, 3), 1);
    let tpage = lhu(m, off(descriptor, 6));
    m.sw(off(sprite, 4), (tpage & 0x9ff) | 0xe100_0000);
    let tag = m.lw(sprite);
    let link = m.lw(ot);
    m.sw(sprite, (tag & 0xff00_0000) | (link & 0x00ff_ffff));
    let link = m.lw(ot);
    m.sw(ot, (link & 0xff00_0000) | (sprite & 0x00ff_ffff));
    m.pop_frame(0x48);
    sprite.wrapping_add(8)
}

// ---- string banks ----------------------------------------------------------------

/// 800D0150: string `index` of the language bank (context +0x38C) in the
/// bank set `banks` (word offsets from `banks`, low two bits ignored).
pub fn string(m: &mut Machine, index: u32, banks: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let language = m.lw(off(context, 0x38c));
    let bank = m.lw(off(off(banks, language << 2), 4));
    let base = banks.wrapping_add((bank >> 2) << 2);
    let entry = m.lw(off(off(base, index << 2), 4));
    base.wrapping_add((entry >> 2) << 2)
}

// ---- text table ----------------------------------------------------------------------

/// 800D04B8: clear the descriptor pointer and every visibility byte.
pub fn reset_texts(m: &mut Machine) {
    let table = table(m);
    m.sw(table, 0);
    clear_visibility(m);
}

/// 800D04F4: install the descriptor table and the string banks.
pub fn set_texts(m: &mut Machine, descriptors: u32, banks: u32) {
    let table = table(m);
    m.sw(table, descriptors);
    m.sw(off(table, 4), banks);
}

/// 800D050C: visibility of text `index` = `value`.
pub fn set_visible(m: &mut Machine, index: u32, value: u32) {
    let table = table(m);
    m.sb(off(off(table, index), 0x10), value as u8);
}

/// 800D0524: font of text `index` (descriptor halfword +0) = `font`.
pub fn set_font(m: &mut Machine, index: u32, font: u32) {
    let table = table(m);
    let descriptors = m.lw(table);
    m.sh(off(descriptors, index.wrapping_mul(24)), font as u16);
}

/// 800D0548: hide every text.
pub fn clear_visibility(m: &mut Machine) {
    for i in 0..0x100 {
        let table = table(m);
        m.sb(off(off(table, i), 0x10), 0);
    }
}

/// 800D05E8: draw every visible text of the table with packets from
/// `packet` into ordering-table entry `ot` (white, 0x808080), at its
/// position plus its offset slot; returns the next packet address.
pub fn draw_texts(m: &mut Machine, packet: u32, ot: u32) -> u32 {
    let sp = m.push_frame(0xa0);
    let mut packet = packet;
    let table = table(m);
    let first = m.lw(table);
    m.sw(off(table, 8), first);
    for i in 0..0x100u32 {
        let table = self::table(m);
        if lbu(m, off(off(table, i), 0x10)) != 0 {
            let descriptor = m.lw(off(table, 8));
            let slot = lbu(m, off(descriptor, 2));
            ov1_script::get_offset(m, slot, off(sp, 0x70), off(sp, 0x72));
            let (x, y) = if lbu(m, off(descriptor, 3)) & 2 != 0 {
                let context = m.gw(CONTEXT);
                let language = m.lw(off(context, 0x38c));
                let entry = off(descriptor, language << 2);
                (lhu(m, off(entry, 4)), lhu(m, off(entry, 6)))
            } else {
                (lhu(m, off(descriptor, 4)), lhu(m, off(descriptor, 6)))
            };
            let y = y.wrapping_add(lhu(m, off(sp, 0x72)));
            let x = x.wrapping_add(lhu(m, off(sp, 0x70)));
            let text = if lbu(m, off(descriptor, 3)) & 1 != 0 {
                let table = self::table(m);
                let banks = m.lw(off(table, 4));
                let format = string(m, i, banks);
                let table = self::table(m);
                let argument = m.lw(off(table, 0xc));
                dispatch::call(m, SPRINTF, [off(sp, 0x20), format, argument, 0]);
                off(sp, 0x20)
            } else {
                let table = self::table(m);
                let banks = m.lw(off(table, 4));
                string(m, i, banks)
            };
            let font = lh(m, descriptor);
            m.sw(off(sp, 0x10), x as u16 as i16 as u32);
            m.sw(off(sp, 0x14), y as u16 as i16 as u32);
            m.sw(off(sp, 0x18), 0x0080_8080);
            packet = draw_text(m, font, text, ot, packet);
        }
        let table = self::table(m);
        let descriptor = m.lw(off(table, 8));
        m.sw(off(table, 8), descriptor.wrapping_add(0x18));
    }
    m.pop_frame(0xa0);
    packet
}

/// 800D019C (unreferenced): entry `j` of sub-bank `i` of the language
/// bank in `banks`.
pub fn string3(m: &mut Machine, i: u32, j: u32, banks: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let language = m.lw(off(context, 0x38c));
    let bank = m.lw(off(off(banks, language << 2), 4));
    let base = banks.wrapping_add((bank >> 2) << 2);
    let sub = m.lw(off(off(base, i << 2), 4));
    let base = base.wrapping_add((sub >> 2) << 2);
    let entry = m.lw(off(off(base, j << 2), 4));
    base.wrapping_add((entry >> 2) << 2)
}

/// 800D0200 (unreferenced): the first word of entry `index` of the
/// language bank in `banks`.
pub fn entry_word(m: &mut Machine, index: u32, banks: u32) -> u32 {
    let context = m.gw(CONTEXT);
    let language = m.lw(off(context, 0x38c));
    let bank = m.lw(off(off(banks, language << 2), 4));
    let base = banks.wrapping_add((bank >> 2) << 2);
    let entry = m.lw(off(off(base, index << 2), 4));
    let entry = base.wrapping_add((entry >> 2) << 2);
    m.lw(entry)
}

/// 800D0578 (unreferenced): the `sprintf` argument of formatted texts.
pub fn set_format_argument(m: &mut Machine, value: u32) {
    let table = table(m);
    m.sw(off(table, 0xc), value);
}

/// 800D0588 (unreferenced): replace the spaces of `string` with code 7.
pub fn spaces_to_code7(m: &mut Machine, string: u32) {
    m.push_frame(0x18);
    let length = dispatch::call(m, STRLEN, [string, 0, 0, 0]);
    for i in 0..length as i32 {
        let at = off(string, i as u32);
        if lbu(m, at) == 0x20 {
            m.sb(at, 7);
        }
    }
    m.pop_frame(0x18);
}
