//! The Ulz versions 0 and 2 found in AC3's nested resource archives.

use crate::{Error, ErrorKind, Format, Result, buffer, word};

pub const MAX_OUTPUT_SIZE: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub payload: Vec<u8>,
    pub version: u8,
    pub displacement_bits: u8,
    pub flags_end: usize,
    pub literal_end: usize,
    pub match_end: usize,
}

fn error(offset: usize, kind: ErrorKind, detail: &'static str) -> Error {
    Error::new(Format::Ulz, offset, kind, detail)
}

fn u24(bytes: &[u8]) -> usize {
    usize::from(bytes[0]) | (usize::from(bytes[1]) << 8) | (usize::from(bytes[2]) << 16)
}

/// Decode one bounded Ulz container; stream-end positions retain padding context.
pub fn decompress(data: &[u8]) -> Result<Decoded> {
    if data.len() < 20 {
        return Err(error(
            data.len(),
            ErrorKind::Truncated,
            "truncated Ulz header/flags",
        ));
    }
    if &data[..4] != b"Ulz\x1a" {
        return Err(error(0, ErrorKind::InvalidMagic, "expected Ulz magic"));
    }
    let expected = u24(&data[4..7]);
    let version = data[7];
    let mut literal = u24(&data[8..11]);
    let bits = data[11];
    let mut match_pos = word(data, 12, Format::Ulz)? as usize;
    if ![0, 2].contains(&version) || !(1..=15).contains(&bits) {
        return Err(error(
            7,
            ErrorKind::Unsupported,
            "unsupported Ulz version or displacement width",
        ));
    }
    if expected > MAX_OUTPUT_SIZE {
        return Err(error(4, ErrorKind::InvalidSize, "output exceeds limit"));
    }
    if !(16..=data.len()).contains(&match_pos) {
        return Err(error(
            12,
            ErrorKind::InvalidOffset,
            "match stream outside input",
        ));
    }
    if literal != 0 && !(16..=match_pos).contains(&literal) {
        return Err(error(
            8,
            ErrorKind::InvalidOffset,
            "literal stream outside input",
        ));
    }
    let literal_start = literal;
    let match_start = match_pos;
    let flag_limit = if literal == 0 { match_pos } else { literal };
    let mut flag_pos = 16;
    let mut collected = 0x8000_0000u32;
    let mut output = buffer(expected, Format::Ulz)?;
    loop {
        if version == 2 && output.len() == expected {
            break;
        }
        let mut flag = collected & 0x8000_0000;
        collected <<= 1;
        if collected == 0 {
            if flag_pos + 4 > flag_limit {
                return Err(error(
                    flag_pos,
                    ErrorKind::Truncated,
                    "flags overlap next stream",
                ));
            }
            collected = word(data, flag_pos, Format::Ulz)?;
            flag_pos += 4;
            flag = collected & 0x8000_0000;
            if version == 0 && collected == 0 {
                break;
            }
            collected = (collected << 1) | u32::from(version == 2);
        }
        if flag != 0 {
            if literal_start == 0 || literal >= match_start {
                return Err(error(
                    literal,
                    ErrorKind::Truncated,
                    "literal stream exhausted",
                ));
            }
            if output.len() == expected {
                return Err(error(
                    literal,
                    ErrorKind::SizeMismatch,
                    "output exceeds declared size",
                ));
            }
            output.push(data[literal]);
            literal += 1;
        } else {
            let bytes = data
                .get(match_pos..match_pos + 2)
                .ok_or_else(|| error(match_pos, ErrorKind::Truncated, "match stream exhausted"))?;
            let token = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
            let distance = (token & ((1 << bits) - 1)) + 1;
            let length = (token >> bits) + 3;
            if distance > output.len() {
                return Err(error(
                    match_pos,
                    ErrorKind::InvalidBackReference,
                    "back reference before output start",
                ));
            }
            if length > expected - output.len() {
                return Err(error(
                    match_pos,
                    ErrorKind::SizeMismatch,
                    "output exceeds declared size",
                ));
            }
            for _ in 0..length {
                output.push(output[output.len() - distance]);
            }
            match_pos += 2;
        }
    }
    if output.len() != expected {
        return Err(error(
            flag_pos,
            ErrorKind::SizeMismatch,
            "decoded size differs from declared size",
        ));
    }
    Ok(Decoded {
        payload: output,
        version,
        displacement_bits: bits,
        flags_end: flag_pos,
        literal_end: literal,
        match_end: match_pos,
    })
}

/// A stream decoded by [`decompress_bounded`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedDecoded {
    pub decoded: Decoded,
    /// [`decompress`] refused the stream and the size bound decoded it.
    pub bounded: bool,
    /// With the bound: bytes the last back-reference would have written
    /// past the declared size (cut off).
    pub overshoot: usize,
}

/// Size-bounded decoding for the asset tools: [`decompress`], or when it
/// refuses the stream, the same decoding stopped at the declared size.
///
/// The Japanese release has version-0 streams (423 on disc 1, 361 on disc
/// 2; the translation patch's recompressed files) whose last flag word is
/// padded, the terminator at bit 0. After the declared size their flags
/// continue: the game's decoder (800AE118, JP 800AFD08, translated with
/// that behaviour in `ac3-flight`) writes past the output and reads past
/// the back-reference stream until a zero flag word, which [`decompress`]
/// refuses ("output exceeds declared size"). Stopping at the declared size
/// decodes them with every read inside the stream. On failure the strict
/// decoder's error is returned.
pub fn decompress_bounded(data: &[u8]) -> Result<BoundedDecoded> {
    match decompress(data) {
        Ok(decoded) => Ok(BoundedDecoded {
            decoded,
            bounded: false,
            overshoot: 0,
        }),
        Err(strict) => decompress_to_size(data)
            .map(|(decoded, overshoot)| BoundedDecoded {
                decoded,
                bounded: true,
                overshoot,
            })
            .map_err(|_| strict),
    }
}

/// [`decompress`] for both versions stopped once the declared size is
/// reached; a back-reference crossing it is cut there.
fn decompress_to_size(data: &[u8]) -> Result<(Decoded, usize)> {
    if data.len() < 20 {
        return Err(error(
            data.len(),
            ErrorKind::Truncated,
            "truncated Ulz header/flags",
        ));
    }
    if &data[..4] != b"Ulz\x1a" {
        return Err(error(0, ErrorKind::InvalidMagic, "expected Ulz magic"));
    }
    let expected = u24(&data[4..7]);
    let version = data[7];
    let mut literal = u24(&data[8..11]);
    let bits = data[11];
    let mut match_pos = word(data, 12, Format::Ulz)? as usize;
    if ![0, 2].contains(&version) || !(1..=15).contains(&bits) {
        return Err(error(
            7,
            ErrorKind::Unsupported,
            "unsupported Ulz version or displacement width",
        ));
    }
    if expected > MAX_OUTPUT_SIZE {
        return Err(error(4, ErrorKind::InvalidSize, "output exceeds limit"));
    }
    if !(16..=data.len()).contains(&match_pos) {
        return Err(error(
            12,
            ErrorKind::InvalidOffset,
            "match stream outside input",
        ));
    }
    if literal != 0 && !(16..=match_pos).contains(&literal) {
        return Err(error(
            8,
            ErrorKind::InvalidOffset,
            "literal stream outside input",
        ));
    }
    let literal_start = literal;
    let match_start = match_pos;
    let flag_limit = if literal == 0 { match_pos } else { literal };
    let mut flag_pos = 16;
    let mut collected = 0x8000_0000u32;
    let mut output = buffer(expected, Format::Ulz)?;
    let mut overshoot = 0;
    while output.len() < expected {
        let mut flag = collected & 0x8000_0000;
        collected <<= 1;
        if collected == 0 {
            if flag_pos + 4 > flag_limit {
                return Err(error(
                    flag_pos,
                    ErrorKind::Truncated,
                    "flags overlap next stream",
                ));
            }
            collected = word(data, flag_pos, Format::Ulz)?;
            flag_pos += 4;
            flag = collected & 0x8000_0000;
            if version == 0 && collected == 0 {
                break;
            }
            collected = (collected << 1) | u32::from(version == 2);
        }
        if flag != 0 {
            if literal_start == 0 || literal >= match_start {
                return Err(error(
                    literal,
                    ErrorKind::Truncated,
                    "literal stream exhausted",
                ));
            }
            output.push(data[literal]);
            literal += 1;
        } else {
            let bytes = data
                .get(match_pos..match_pos + 2)
                .ok_or_else(|| error(match_pos, ErrorKind::Truncated, "match stream exhausted"))?;
            let token = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
            let distance = (token & ((1 << bits) - 1)) + 1;
            let length = (token >> bits) + 3;
            if distance > output.len() {
                return Err(error(
                    match_pos,
                    ErrorKind::InvalidBackReference,
                    "back reference before output start",
                ));
            }
            let copied = length.min(expected - output.len());
            overshoot = length - copied;
            for _ in 0..copied {
                output.push(output[output.len() - distance]);
            }
            match_pos += 2;
        }
    }
    if output.len() != expected {
        return Err(error(
            flag_pos,
            ErrorKind::SizeMismatch,
            "decoded size differs from declared size",
        ));
    }
    let decoded = Decoded {
        payload: output,
        version,
        displacement_bits: bits,
        flags_end: flag_pos,
        literal_end: literal,
        match_end: match_pos,
    };
    Ok((decoded, overshoot))
}

#[cfg(test)]
mod bounded_tests {
    use super::*;

    /// A version-0 stream: header, flag words, literals, match tokens.
    fn stream(expected: usize, flags: &[u32], literals: &[u8], tokens: &[u16]) -> Vec<u8> {
        let bits = 12u8;
        let literal_start = 16 + 4 * flags.len();
        let match_start = literal_start + literals.len();
        let mut data = b"Ulz\x1a".to_vec();
        data.extend_from_slice(&(expected as u32).to_le_bytes()[..3]);
        data.push(0);
        data.extend_from_slice(&(literal_start as u32).to_le_bytes()[..3]);
        data.push(bits);
        data.extend_from_slice(&(match_start as u32).to_le_bytes());
        for flag in flags {
            data.extend_from_slice(&flag.to_le_bytes());
        }
        data.extend_from_slice(literals);
        for token in tokens {
            data.extend_from_slice(&token.to_le_bytes());
        }
        data
    }

    #[test]
    fn namco_streams_decode_the_same_both_ways() {
        // Literals A, B, then a 4-byte copy at distance 2: ABABAB. Flags
        // 1, 1, 0 then the terminator; then a zero word ends the stream.
        let data = stream(6, &[0b1101 << 28, 0], b"AB", &[(1 << 12) | 1]);
        let strict = decompress(&data).unwrap();
        assert_eq!(strict.payload, b"ABABAB");
        let bounded = decompress_bounded(&data).unwrap();
        assert_eq!(
            (bounded.decoded, bounded.bounded, bounded.overshoot),
            (strict, false, 0)
        );
    }

    #[test]
    fn padded_last_flag_words_stop_at_the_declared_size() {
        // The patch's style: one flag word with the terminator at bit 0;
        // the three wanted flags (1, 1, 0), then padding zeros ("copy")
        // that the game's decoder would keep following past the output.
        let data = stream(6, &[(0b110 << 29) | 1, 0], b"AB", &[(1 << 12) | 1, 0, 0]);
        let strict = decompress(&data).unwrap_err();
        assert_eq!(strict.kind, ErrorKind::SizeMismatch);
        let bounded = decompress_bounded(&data).unwrap();
        assert!(bounded.bounded);
        assert_eq!(bounded.decoded.payload, b"ABABAB");
        assert_eq!(bounded.overshoot, 0);
        // Every read stayed in its stream: one flag word, two literals,
        // one token.
        assert_eq!(bounded.decoded.flags_end, 20);
        assert_eq!(bounded.decoded.literal_end, 26);
        assert_eq!(bounded.decoded.match_end, 28);
        // A last copy crossing the size is cut there.
        let data = stream(5, &[(0b110 << 29) | 1, 0], b"AB", &[(1 << 12) | 1]);
        let bounded = decompress_bounded(&data).unwrap();
        assert_eq!(
            (bounded.decoded.payload.as_slice(), bounded.overshoot),
            (&b"ABABA"[..], 1)
        );
    }

    #[test]
    fn broken_streams_keep_the_strict_error() {
        // The literal stream runs out before the declared size.
        let data = stream(8, &[(0b111 << 29) | 1, 0], b"AB", &[]);
        let strict = decompress(&data).unwrap_err();
        assert_eq!(decompress_bounded(&data).unwrap_err(), strict);
        assert!(decompress_bounded(b"Ulz\x1a").is_err());
    }
}
