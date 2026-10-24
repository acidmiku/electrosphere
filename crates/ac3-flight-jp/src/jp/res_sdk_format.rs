//! The game's own printf engine (resident, not PsyQ's sprintf): vsprintf
//! (8002877C) and its formatter (80028074), translated from the JP
//! instructions (`ACE3.BIN`; exact matches of PAL 800282DC / 80027BD4, which
//! our PAL crate never translated). The formatter's helpers 80027C24 (put
//! a character), 80027C3C (the digits of a number) and 80027E80 (one padded
//! field) are the generated `ai` module's.
//!
//! Every routine is `pub`, so patch code can call it natively; [`dispatch`]
//! claims exactly these addresses.
use crate::ai::{format_field_279e0, format_number_2779c, put_char_27784};
use crate::machine::Machine;
use crate::sdk;

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// 80010134 / 80010148: the digit tables "0123456789ABCDEF" and
/// "0123456789abcdef".
const DIGITS_UPPER: u32 = 0x8001_0134;
const DIGITS_LOWER: u32 = 0x8001_0148;
/// 800BCA55: the character class table (bit 2: a decimal digit).
const CHAR_CLASS: u32 = 0x800b_ca55;
/// 800BCB9C: "(null)", printed for a null `%s`.
const NULL_TEXT: u32 = 0x800b_cb9c;

/// Conversion flags (the formatter's `s2`, passed on to 80027C3C and
/// 80027E80): 1 `h`, 2 `l` (and `D`/`I`/`U`/`Z`), 4 and 8 `L` (bit 2: a
/// 64-bit argument), 0x10 `#`, 0x20 `-` (or a negative `*` width), 0x40 `0`;
/// with `#` and a nonzero value the prefix 0x80 `0x` (`x`, `p`), 0x100 `0X`
/// (`X`), 0x200 `0` (`o`), 0x400 `0b` (`b`).
const HALF: u32 = 1;
const LONG: u32 = 2;
const WIDE: u32 = 4;
const ALTERNATE: u32 = 0x10;
const LEFT: u32 = 0x20;
const ZERO_PAD: u32 = 0x40;

fn is_digit(m: &mut Machine, c: u32) -> bool {
    m.lbu(at(CHAR_CLASS, c)) & 4 != 0
}

/// The next argument of an unsigned conversion as (low, high): two words
/// (flags [`WIDE`]), the low halfword of a word ([`HALF`]), else a word.
fn unsigned_argument(m: &mut Machine, ap: &mut u32, flags: u32) -> (u32, u32) {
    if flags & WIDE != 0 {
        *ap = ap.wrapping_add(8);
        let low = m.lw(ap.wrapping_sub(8));
        let high = m.lw(ap.wrapping_sub(4));
        (low, high)
    } else {
        *ap = ap.wrapping_add(4);
        let low = if flags & HALF != 0 {
            u32::from(m.lhu(ap.wrapping_sub(4)))
        } else {
            m.lw(ap.wrapping_sub(4))
        };
        (low, 0)
    }
}

/// The next argument of a signed conversion as (low, high): two words, or
/// the low halfword or the word sign-extended to 64 bits.
fn signed_argument(m: &mut Machine, ap: &mut u32, flags: u32) -> (u32, u32) {
    if flags & WIDE != 0 {
        return unsigned_argument(m, ap, flags);
    }
    *ap = ap.wrapping_add(4);
    let low = if flags & HALF != 0 {
        i32::from(m.lhu(ap.wrapping_sub(4)) as i16) as u32
    } else {
        m.lw(ap.wrapping_sub(4))
    };
    (low, ((low as i32) >> 31) as u32)
}

/// One number field: 80027C3C writes the digits of `value` in `base`
/// (stack words +0x10 base, +0x14 the length's address +0x20, +0x18 the
/// digit table), then 80027E80 the field (+0x10 width, +0x14 precision,
/// +0x18 the length) with `sign`. Returns the characters written.
#[allow(clippy::too_many_arguments)]
fn number_field(
    m: &mut Machine,
    frame: u32,
    cursor: u32,
    (low, high): (u32, u32),
    flags: u32,
    (width, precision): (u32, u32),
    (base, digits): (u32, u32),
    sign: u32,
) -> u32 {
    m.sw(at(frame, 0x10), base);
    m.sw(at(frame, 0x14), at(frame, 0x20));
    m.sw(at(frame, 0x18), digits);
    let text = format_number_2779c(m, low, high, flags, precision);
    m.sw(at(frame, 0x10), width);
    m.sw(at(frame, 0x14), precision);
    let length = m.lw(at(frame, 0x20));
    m.sw(at(frame, 0x18), length);
    format_field_279e0(m, cursor, text, flags, sign)
}

/// 80028074 (JP; PAL 80027BD4, exact): format `format` with the argument
/// list `arguments` (words) through the output cursor `cursor` (80027C24
/// appends a character at `[cursor]`). Returns the characters written.
///
/// Text other than `%` is copied. A conversion takes, in any order and
/// number, the flags ` ` (a space sign unless one is set), `+`, `-`, `#`
/// and `0` (ignored with a positive precision), a width (digits or `*`,
/// whose negative value left-justifies), a precision (`.` and digits or
/// `*`; `.` alone is 0) and the sizes `h`, `l` and `L`, then one of `d`/`i`
/// (`D`/`I` long), `u` (`U`/`Z` long), `x`, `X`, `o`, `b`, `p` (`x` with a
/// default precision of 8), `c`, `s` (a null one prints "(null)"; a
/// precision limits the length through memchr, else strlen) and `n` (the
/// count so far, a halfword with `h` alone). Any other character is printed
/// for the conversion; a NUL ends the format there. The dispatch is the
/// jump table at 8001015C (characters below 0x79).
pub fn format(m: &mut Machine, format: u32, arguments: u32, cursor: u32) -> u32 {
    let frame = m.push_frame(0x50);
    let mut p = format;
    let mut ap = arguments;
    let mut count = 0u32;
    'text: loop {
        let mut c = u32::from(m.lbu(p));
        p = p.wrapping_add(1);
        while c != 0 && c != u32::from(b'%') {
            put_char_27784(m, c, cursor);
            count = count.wrapping_add(1);
            c = u32::from(m.lbu(p));
            p = p.wrapping_add(1);
        }
        p = p.wrapping_sub(1);
        if c == 0 {
            break;
        }
        let mut flags = 0u32;
        let mut width = 0u32;
        let mut precision = u32::MAX;
        let mut sign = 0u32;
        let written = loop {
            p = p.wrapping_add(1);
            let c = m.lbu(p);
            match c {
                0 => break 'text,
                b' ' => {
                    if sign == 0 {
                        sign = u32::from(b' ');
                    }
                }
                b'#' => flags |= ALTERNATE,
                b'+' => sign = u32::from(b'+'),
                b'-' => flags |= LEFT,
                b'.' => {
                    p = p.wrapping_add(1);
                    let c = m.lbu(p);
                    if c == b'*' {
                        ap = ap.wrapping_add(4);
                        precision = m.lw(ap.wrapping_sub(4));
                    } else {
                        let mut value = 0u32;
                        let mut c = u32::from(c);
                        while is_digit(m, c) {
                            value = value.wrapping_mul(10).wrapping_sub(0x30).wrapping_add(c);
                            p = p.wrapping_add(1);
                            c = u32::from(m.lbu(p));
                        }
                        p = p.wrapping_sub(1);
                        precision = value;
                    }
                }
                b'0' => {
                    if sign != u32::from(b'-') && (precision as i32) <= 0 {
                        flags |= ZERO_PAD;
                    }
                }
                b'*' => {
                    ap = ap.wrapping_add(4);
                    width = m.lw(ap.wrapping_sub(4));
                    if (width as i32) < 0 {
                        width = 0u32.wrapping_sub(width);
                        flags |= LEFT;
                    }
                }
                b'1'..=b'9' => {
                    let mut value = 0u32;
                    loop {
                        let digit = u32::from(m.lbu(p));
                        p = p.wrapping_add(1);
                        let next = u32::from(m.lbu(p));
                        value = value
                            .wrapping_mul(10)
                            .wrapping_sub(0x30)
                            .wrapping_add(digit);
                        if !is_digit(m, next) {
                            break;
                        }
                    }
                    width = value;
                    p = p.wrapping_sub(1);
                }
                b'L' => flags |= WIDE | 8,
                b'h' => flags |= HALF,
                b'l' => flags |= LONG,
                b'n' => {
                    ap = ap.wrapping_add(4);
                    let target = m.lw(ap.wrapping_sub(4));
                    if flags & LONG == 0 && flags & HALF != 0 {
                        m.sh(target, count as u16);
                    } else {
                        m.sw(target, count);
                    }
                    break 0;
                }
                b'X' | b'x' | b'p' | b'o' | b'b' => {
                    if c == b'p' && precision == u32::MAX {
                        precision = 8;
                    }
                    let (prefix, base, digits) = match c {
                        b'X' => (0x100, 16, DIGITS_UPPER),
                        b'o' => (0x200, 8, DIGITS_LOWER),
                        b'b' => (0x400, 2, DIGITS_LOWER),
                        _ => (0x80, 16, DIGITS_LOWER),
                    };
                    let value = unsigned_argument(m, &mut ap, flags);
                    if flags & ALTERNATE != 0 && value.0 | value.1 != 0 {
                        flags |= prefix;
                    }
                    break number_field(
                        m,
                        frame,
                        cursor,
                        value,
                        flags,
                        (width, precision),
                        (base, digits),
                        0,
                    );
                }
                b'U' | b'Z' | b'u' => {
                    if c != b'u' {
                        flags |= LONG;
                    }
                    let value = unsigned_argument(m, &mut ap, flags);
                    break number_field(
                        m,
                        frame,
                        cursor,
                        value,
                        flags,
                        (width, precision),
                        (10, DIGITS_LOWER),
                        0,
                    );
                }
                b'D' | b'I' | b'd' | b'i' => {
                    if c == b'D' || c == b'I' {
                        flags |= LONG;
                    }
                    let (mut low, mut high) = signed_argument(m, &mut ap, flags);
                    if (high as i32) < 0 {
                        low = 0u32.wrapping_sub(low);
                        high = 0u32.wrapping_sub(high).wrapping_sub(u32::from(low != 0));
                        sign = u32::from(b'-');
                    }
                    break number_field(
                        m,
                        frame,
                        cursor,
                        (low, high),
                        flags,
                        (width, precision),
                        (10, DIGITS_LOWER),
                        sign,
                    );
                }
                b'c' => {
                    ap = ap.wrapping_add(4);
                    let byte = m.lbu(ap.wrapping_sub(4));
                    let text = at(frame, 0x24);
                    m.sb(text, byte);
                    m.sw(at(frame, 0x10), width);
                    m.sw(at(frame, 0x14), precision);
                    m.sw(at(frame, 0x18), 1);
                    break format_field_279e0(m, cursor, text, flags, 0);
                }
                b's' => {
                    ap = ap.wrapping_add(4);
                    let mut text = m.lw(ap.wrapping_sub(4));
                    if text == 0 {
                        text = NULL_TEXT;
                    }
                    let length = if (precision as i32) >= 0 {
                        match sdk::memchr(m, text, 0, precision) {
                            0 => precision,
                            end => end.wrapping_sub(text),
                        }
                    } else {
                        sdk::strlen(m, text)
                    };
                    m.sw(at(frame, 0x20), length);
                    m.sw(at(frame, 0x10), width);
                    let length = m.lw(at(frame, 0x20));
                    m.sw(at(frame, 0x14), length);
                    m.sw(at(frame, 0x18), length);
                    break format_field_279e0(m, cursor, text, flags, 0);
                }
                _ => {
                    let c = u32::from(m.lbu(p));
                    put_char_27784(m, c, cursor);
                    break 1;
                }
            }
        };
        count = count.wrapping_add(written);
        p = p.wrapping_add(1);
    }
    m.pop_frame(0x50);
    count
}

/// 8002877C (JP; PAL 800282DC, exact): vsprintf(buffer, format,
/// arguments): [`format`] (80028074) with an output cursor in the frame
/// (+0x10, both words `buffer`), terminate the string and return its
/// length.
pub fn vsprintf(m: &mut Machine, buffer: u32, format: u32, arguments: u32) -> u32 {
    let frame = m.push_frame(0x20);
    let cursor = at(frame, 0x10);
    m.sw(at(frame, 0x14), buffer);
    m.sw(cursor, buffer);
    let length = self::format(m, format, arguments, cursor);
    m.sb(buffer.wrapping_add(length), 0);
    m.pop_frame(0x20);
    length
}

/// The routines of this file by JP address.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, _] = args;
    Some(match target {
        0x8002_8074 => format(m, a0, a1, a2),
        0x8002_877c => vsprintf(m, a0, a1, a2),
        _ => return None,
    })
}
