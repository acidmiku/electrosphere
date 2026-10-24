//! PAL menu font spacing, translated from overlay 1.
//!
//! These helpers operate on original single-byte character codes, not UTF-8.
//! Glyph rectangles, width overrides, line breaks and GPU packets belong to the
//! renderer. Source: `analysis/overlays/overlay_001/functions/800ceaac.c`,
//! `800cfaf0.c`, and the corresponding original MIPS disassembly.

/// Additional advance after drawing `left`, before drawing `right`.
///
/// Original function: `0x800CEAAC`. Its 32-bit return value is signed; every
/// reachable result is in -2..=2. Include the final pair with `right == 0`:
/// several fonts have unconditional adjustments even at the string terminator.
pub fn kerning(font: u16, left: u8, right: u8) -> i16 {
    match font {
        0 | 1 => small(left, right),
        2..=4 => language(left, right),
        5..=15 => menu(font, left, right),
        16 | 17 => proportional(left, right),
        18..=20 => match (left, right) {
            (b'e' | b'r', b'l') | (b'C', b'T') => -1,
            _ => 0,
        },
        21 => match (left, right) {
            (b'(', b'M') | (b'h', b'i') => 1,
            (b'L', b'T') => -2,
            (b'E', b'S' | b'D')
            | (b'M', b'C')
            | (b'T', b'o')
            | (b'a', _)
            | (b'e', b's')
            | (b'g', b'o')
            | (b'u', b'k') => -1,
            _ => 0,
        },
        22 => match (left, right) {
            (b'A', b'L') | (b'G', b'r') => 1,
            (b'o', b'j') => -2,
            (b'(', b'C')
            | (b'H' | b'T', b'a')
            | (b'L', b'T')
            | (b'V' | b'Y', _)
            | (b'c', b'u')
            | (b'e', b'n')
            | (b'i', b'a' | b'd' | b'c' | b'z')
            | (b'k', b's' | b'a')
            | (b'o', b'd' | b'z' | b's')
            | (b'p', b'e')
            | (b'r', b'e' | b'a')
            | (b'u', b'c') => -1,
            _ => 0,
        },
        _ => 0,
    }
}

/// Shift the pen before drawing a character that has a preceding source byte.
///
/// Original renderer: `0x800CFAF0`, adjustment branches `0x800CFC5C` through
/// `0x800D0038`. Call this only when the source byte index is greater than zero;
/// `previous` is the immediately preceding source byte, including CR/LF, rather
/// than the previous rendered glyph. Apply the result to the running pen as
/// well as the glyph position, before adding glyph width and [`kerning`].
pub fn pre_adjust(font: u16, previous: u8, current: u8) -> i16 {
    match font {
        5..=15 => match current {
            b'l' if !b"lbdefiju".contains(&previous) => -1,
            b't' if !b"BLQWZadfgijrwxyz".contains(&previous) => -1,
            b'.' => 1,
            _ => 0,
        },
        16 | 17 => match current {
            b'g' if !b"in\xe4raoe\xe9".contains(&previous) => -1,
            b'i' if !b"lvgMmrLGcV/sowfadEunRehzbZpNBkDSACxTH".contains(&previous) => -1,
            b't' if !b"iuopsrxkaS\xe9\xc9\xe4A".contains(&previous) => -1,
            b'c' if !b"ao\xe9ipsnr\xe4A".contains(&previous) => -1,
            b'!' => 1,
            _ => 0,
        },
        21 if current == b'j' => -1,
        22 if matches!(current, b'T' | b'V') => -1,
        _ => 0,
    }
}

fn small(left: u8, right: u8) -> i16 {
    match (left, right) {
        (b'e' | b'o', b't') => -2,
        (b'F' | b'c', b'o')
        | (b'P', b'a')
        | (b'R', b'e')
        | (b'S' | b'a' | b'f' | b'n' | b's' | b'u', b't')
        | (b'e', b'x' | b'l')
        | (b'r', b's' | b'a' | b'd' | b'p' | b'o' | b'e' | b'f' | b't')
        | (b't', b'y' | b't' | b'u' | b'o' | b'e')
        | (b'x', b'c') => -1,
        _ => 0,
    }
}

fn language(left: u8, right: u8) -> i16 {
    match (left, right) {
        (b'i', b'd') => 1,
        (b'e', b's') => -2,
        (b'N' | b'd' | b'g' | b'j' | b'p' | b'r' | b'u' | b'y', b'e')
        | (b'S' | b'o' | 0xed, b'c')
        | (b'c', b'h' | b'i')
        | (b'e', b'u' | b'w' | b'n')
        | (b'f', 0xe1)
        | (b'l', b'i' | b'o' | b'e')
        | (b'n', 0xe7 | b'f' | b'e' | b'u')
        | (b's', b'c' | b'p' | b'y' | 0xed)
        | (b'w', b'i') => -1,
        _ => 0,
    }
}

fn menu(font: u16, left: u8, right: u8) -> i16 {
    match (left, right) {
        (b'.', _) | (b'w', b'h' | b'i') => 1,
        (b'1', _) if font == 6 || font == 8 => 0,
        (b'C', b'h') => 0,
        (b'O', b'Y') | (b'g', b't' | b'e') => -2,
        (b'r', _) if b"sdctoeui\xe9mfra\xfc".contains(&right) => -2,
        (b'1' | b'C' | b'D' | b'R' | b'n' | 0xf1 | b'r', _)
        | (b'0', b'1')
        | (b'E', b'S' | b'T' | b'V' | b'R')
        | (b'F', b'A' | 0xc1)
        | (b'I', 0xd3 | b'S')
        | (b'L', b'O' | b'e' | b'U' | b'S')
        | (b'M', b'A')
        | (b'N', b'e' | b'o' | b'A' | b'C' | b'Y' | b'S' | b'O')
        | (b'O', b'S' | b'T' | b'P' | b'R' | b'b' | b'v')
        | (b'P', b'R' | b'o' | b'E' | 0xc9 | b'O')
        | (b'T', b'A' | 0xd3)
        | (b'U', b'T')
        | (b'a', b's' | b't' | b'v' | b'y' | b'j')
        | (b'd' | b't', b'!')
        | (b'e', b'v')
        | (b'g', b'i' | b'a' | b'o')
        | (b'i', b's' | b't' | b'v')
        | (b'l', b'o' | 0xf3)
        | (b'o', b's' | b'v' | b'r' | b'u' | b'n' | b'g' | b'p')
        | (b'v', b'e') => -1,
        _ => 0,
    }
}

fn proportional(left: u8, right: u8) -> i16 {
    // These letters default to -1, with zero/positive exceptions. Preserve the
    // original defaults for all 256 byte values, not just printable characters.
    match left {
        b'M' => return -i16::from(!b"oiaeuSrR".contains(&right)),
        b'a' => return -i16::from(!b"tub/wsrmanloek\xf1zi;vpg\xe9fq\xdf".contains(&right)),
        b'i' => {
            return if right == 0xf9 {
                1
            } else {
                -i16::from(!b"brzeosav\xe8quplmn\xe9ik".contains(&right))
            };
        }
        b'n' => return -i16::from(!b"t/kesozapi\xf6ubq\xe9m".contains(&right)),
        b'p' => return -i16::from(!b"/euoarim".contains(&right)),
        b'r' => return -i16::from(!b".maiesSkbc".contains(&right)),
        _ => {}
    }
    match (left, right) {
        (b'W', b'i') => 2,
        (b'E', b'n' | b'S' | b'x')
        | (b'c', b'h')
        | (b'e', b'd')
        | (b'g', b'z')
        | (b'h' | b'u', b'y')
        | (b'l', 0xd3)
        | (b't', 0xf3 | b'y' | 0xed) => -1,
        (b'-', _)
        | (b'A', b'U' | b'R' | b'i' | b's')
        | (b'B', b'M')
        | (b'F', b'F')
        | (b'G', b'a' | b'u' | b'e')
        | (b'J', b'u')
        | (b'N', b'a' | b'e' | b'U')
        | (b'O', b'p' | b'U' | b'N' | b'f' | b'F' | b'u')
        | (b'P', b'u' | b'E')
        | (b'R', b'a' | b'e' | b'i' | b'O' | b'B')
        | (b'S', b'p' | b'a' | b'/' | b'e' | b'i')
        | (b'V' | 0xe1, b'i')
        | (b'W', b'a')
        | (b'e', b's' | b't' | b'e')
        | (b'k', b's' | b'e' | b'a' | b't' | b'p' | b'i' | b'b' | b'u' | b'k')
        | (
            b'o',
            b'/' | b'b' | b'a' | b'm' | b'k' | b'i' | b'u' | b's' | b'r' | b'e' | 0xf9 | 0xdf
            | b'z' | b'p',
        )
        | (b's', b'e' | b'i' | b'a' | b'o' | b's' | b'm' | b'p' | b'u' | b'k' | 0xfa)
        | (b't', b'i' | b'/' | b'a')
        | (b'u', b'a' | b's' | b'e' | b'i')
        | (b'v', b'i' | b'e')
        | (b'w', b'i' | b'p' | b'e' | b'k' | b'a')
        | (b'x', b'p' | b'i')
        | (0xdf, b'e' | b'b')
        | (0xe4, b'r' | b'h' | b'u')
        | (0xe8, b't')
        | (0xe9, b'r' | b'e' | b's' | b'a' | b'q' | b'u' | b'p')
        | (0xf6, b's') => 1,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{kerning, pre_adjust};

    #[test]
    fn language_screen_pairs_preserve_signed_and_positive_spacing() {
        for font in 2..=4 {
            assert_eq!(kerning(font, b'e', b's'), -2);
            assert_eq!(kerning(font, b'i', b'd'), 1);
            assert_eq!(kerning(font, b'f', 0xe1), -1);
            assert_eq!(kerning(font, b'e', b'l'), 0);
        }
    }

    #[test]
    fn menu_fonts_keep_digit_overrides_and_unconditional_defaults() {
        for font in 5..=15 {
            assert_eq!(
                kerning(font, b'1', 0),
                if font == 6 || font == 8 { 0 } else { -1 }
            );
            assert_eq!(kerning(font, b'0', b'1'), -1);
            assert_eq!(kerning(font, b'C', b'h'), 0);
            assert_eq!(kerning(font, b'C', 0), -1);
            assert_eq!(kerning(font, b'r', b'e'), -2);
            assert_eq!(kerning(font, b'r', 0), -1);
            assert_eq!(kerning(font, b'.', 0), 1);
            assert_eq!(kerning(font, b'O', b'Y'), -2);
        }
    }

    #[test]
    fn proportional_defaults_include_non_ascii_and_terminator() {
        for font in 16..=17 {
            assert_eq!(kerning(font, b'a', 0), -1);
            assert_eq!(kerning(font, b'a', 0xdf), 0);
            assert_eq!(kerning(font, b'i', 0xf9), 1);
            assert_eq!(kerning(font, b'i', 0xe8), 0);
            assert_eq!(kerning(font, b'i', 0xff), -1);
            assert_eq!(kerning(font, b'W', b'i'), 2);
            assert_eq!(kerning(font, b't', 0xed), -1);
        }
    }

    #[test]
    fn pre_adjust_depends_on_previous_source_byte_and_font_family() {
        assert_eq!(pre_adjust(5, b'e', b'l'), 0);
        assert_eq!(pre_adjust(5, b'a', b'l'), -1);
        assert_eq!(pre_adjust(5, b'B', b't'), 0);
        assert_eq!(pre_adjust(5, b'c', b't'), -1);
        assert_eq!(pre_adjust(5, b'\n', b'l'), -1);
        assert_eq!(pre_adjust(5, b'a', b'.'), 1);
        assert_eq!(pre_adjust(16, 0xe9, b'g'), 0);
        assert_eq!(pre_adjust(16, 0xe8, b'g'), -1);
        assert_eq!(pre_adjust(17, b'H', b'i'), 0);
        assert_eq!(pre_adjust(17, b'I', b'i'), -1);
        assert_eq!(pre_adjust(17, 0xe4, b't'), 0);
        assert_eq!(pre_adjust(17, b'a', b'!'), 1);
        assert_eq!(pre_adjust(21, b'a', b'j'), -1);
        assert_eq!(pre_adjust(22, b'a', b'T'), -1);
        assert_eq!(pre_adjust(22, b'a', b'Y'), 0);
    }

    #[test]
    fn remaining_families_and_out_of_range_fonts() {
        assert_eq!(kerning(0, b'e', b't'), -2);
        assert_eq!(kerning(1, b'r', b'f'), -1);
        for font in 18..=20 {
            assert_eq!(kerning(font, b'C', b'T'), -1);
        }
        assert_eq!(kerning(21, b'L', b'T'), -2);
        assert_eq!(kerning(21, b'a', 0), -1);
        assert_eq!(kerning(22, b'o', b'j'), -2);
        assert_eq!(kerning(22, b'V', 0), -1);
        for font in [23, 255, u16::MAX] {
            assert_eq!(kerning(font, b'r', b'e'), 0);
            assert_eq!(pre_adjust(font, b'a', b'l'), 0);
        }
    }
}
