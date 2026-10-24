//! Namco's PS-Y executable container and compression, as used by PAL ACE.BIN.

use crate::{Error, ErrorKind, Format, Result, buffer, word};

pub const MAX_PAYLOAD_SIZE: usize = 2 * 1024 * 1024;
const HEADER_SIZE: usize = 0x5c;

/// Decoded executable bytes and the embedded PS-X register/load fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsyExecutable {
    pub payload: Vec<u8>,
    pub entry_pc: u32,
    pub initial_gp: u32,
    pub load_address: u32,
    pub stack_address: u32,
    /// Offset added to `stack_address` by the BIOS's EXE loading convention.
    pub stack_offset: u32,
    pub stream_offset: usize,
    pub stream_bytes_consumed: usize,
    /// Bytes after the compression end marker; retained as metadata, not decoded.
    pub trailing_bytes: usize,
    pub output_checksum: u32,
}

fn error(offset: usize, kind: ErrorKind, detail: &'static str) -> Error {
    Error::new(Format::Psy, offset, kind, detail)
}

/// Sum little-endian words with wrapping arithmetic, zero-padding the last word.
pub fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |sum, chunk| {
        let mut bytes = [0u8; 4];
        bytes[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_le_bytes(bytes))
    })
}

/// Decode a PS-Y compression stream and return `(payload, consumed_input_bytes)`.
///
/// The control-byte sentinel is not output. References may overlap their own
/// output. Both the declared size and every source/destination extent are checked.
pub fn decompress(stream: &[u8], expected_size: usize) -> Result<(Vec<u8>, usize)> {
    if expected_size == 0 || expected_size > MAX_PAYLOAD_SIZE {
        return Err(error(
            0,
            ErrorKind::InvalidSize,
            "output size must be within 1..=2 MiB",
        ));
    }
    let bits = *stream
        .first()
        .ok_or_else(|| error(0, ErrorKind::Truncated, "missing displacement width"))?;
    if !(1..=15).contains(&bits) {
        return Err(error(
            0,
            ErrorKind::Unsupported,
            "displacement width must be within 1..=15",
        ));
    }
    let mask = (1usize << bits) - 1;
    let mut pos = 1;
    let mut output = buffer(expected_size, Format::Psy)?;
    loop {
        let mut control = *stream
            .get(pos)
            .ok_or_else(|| error(pos, ErrorKind::Truncated, "missing end marker"))?;
        pos += 1;
        if control == 0 {
            break;
        }
        while control > 1 {
            if control & 1 != 0 {
                let literal = *stream
                    .get(pos)
                    .ok_or_else(|| error(pos, ErrorKind::Truncated, "truncated literal"))?;
                if output.len() == expected_size {
                    return Err(error(
                        pos,
                        ErrorKind::SizeMismatch,
                        "output exceeds declared size",
                    ));
                }
                output.push(literal);
                pos += 1;
            } else {
                let token_bytes = stream
                    .get(pos..pos + 2)
                    .ok_or_else(|| error(pos, ErrorKind::Truncated, "truncated back reference"))?;
                let token = usize::from(u16::from_be_bytes([token_bytes[0], token_bytes[1]]));
                let distance = match token & mask {
                    0 => 1 << bits,
                    distance => distance,
                };
                let length = match token >> bits {
                    0 => 1 << (16 - bits),
                    length => length,
                };
                if distance > output.len() {
                    return Err(error(
                        pos,
                        ErrorKind::InvalidBackReference,
                        "back reference before output start",
                    ));
                }
                if length > expected_size - output.len() {
                    return Err(error(
                        pos,
                        ErrorKind::SizeMismatch,
                        "output exceeds declared size",
                    ));
                }
                pos += 2;
                for _ in 0..length {
                    output.push(output[output.len() - distance]);
                }
            }
            control >>= 1;
        }
    }
    if output.len() != expected_size {
        return Err(error(
            pos,
            ErrorKind::SizeMismatch,
            "decoded size differs from declared size",
        ));
    }
    Ok((output, pos))
}

/// Verify both loader checksums and decode a complete PS-Y file.
///
/// The low two stream-offset bits are masked as in the original loader. This
/// does not manufacture a synthetic PS-X header or interpret unknown fields.
pub fn unpack(data: &[u8]) -> Result<PsyExecutable> {
    if data.len() < HEADER_SIZE {
        return Err(error(
            data.len(),
            ErrorKind::Truncated,
            "truncated PS-Y header",
        ));
    }
    if &data[..4] != b"PS-Y" {
        return Err(error(0, ErrorKind::InvalidMagic, "expected PS-Y magic"));
    }
    let read = |offset| word(data, offset, Format::Psy);
    if u64::from(read(0x0c)?) != data.len() as u64 {
        return Err(error(
            0x0c,
            ErrorKind::SizeMismatch,
            "file length differs from header",
        ));
    }
    if checksum(data) != 0 {
        return Err(error(
            4,
            ErrorKind::ChecksumMismatch,
            "packed checksum is nonzero",
        ));
    }
    let size = read(0x10)? as usize;
    if size == 0 || size > MAX_PAYLOAD_SIZE {
        return Err(error(
            0x10,
            ErrorKind::InvalidSize,
            "output size must be within 1..=2 MiB",
        ));
    }
    let stream_offset = (read(0x14)? & !3) as usize;
    if !(HEADER_SIZE..data.len()).contains(&stream_offset) {
        return Err(error(
            0x14,
            ErrorKind::InvalidOffset,
            "stream offset outside payload area",
        ));
    }
    if read(0x28)? as usize != size {
        return Err(error(
            0x28,
            ErrorKind::SizeMismatch,
            "embedded EXE size differs from output size",
        ));
    }
    let (payload, consumed) = decompress(&data[stream_offset..], size).map_err(|mut error| {
        error.offset += stream_offset;
        error
    })?;
    let output_checksum = checksum(&payload);
    if output_checksum != read(0x58)? {
        return Err(error(
            0x58,
            ErrorKind::ChecksumMismatch,
            "decoded checksum differs from header",
        ));
    }
    Ok(PsyExecutable {
        entry_pc: read(0x1c)?,
        initial_gp: read(0x20)?,
        load_address: read(0x24)?,
        stack_address: read(0x3c)?,
        stack_offset: read(0x40)?,
        stream_offset,
        stream_bytes_consumed: consumed,
        trailing_bytes: data.len() - stream_offset - consumed,
        output_checksum,
        payload,
    })
}
