//! Readers for the PAL release's recovered PS-Y, BPH/BPB, and Ulz formats.
//!
//! These readers consume caller-supplied bytes; the crate contains no game data.
//! The algorithms follow the validated Python recovery tools in this workspace.
//! They decode data only: they do not execute MIPS instructions.

pub mod bph;
pub mod dot1;
pub mod psy;
pub mod ulz;

use std::fmt;

/// Format whose input failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Psy,
    Bph,
    Dot1,
    Ulz,
}

/// A machine-readable reason for rejecting an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Truncated,
    InvalidMagic,
    Unsupported,
    InvalidSize,
    SizeMismatch,
    ChecksumMismatch,
    InvalidOffset,
    InvalidBackReference,
    OverlappingExtent,
    AllocationFailed,
}

/// A parse/decompression error, including the byte offset being interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub format: Format,
    pub offset: usize,
    pub kind: ErrorKind,
    pub detail: &'static str,
}

impl Error {
    pub(crate) fn new(
        format: Format,
        offset: usize,
        kind: ErrorKind,
        detail: &'static str,
    ) -> Self {
        Self {
            format,
            offset,
            kind,
            detail,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} at {:#x}: {} ({:?})",
            self.format, self.offset, self.detail, self.kind
        )
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn word(data: &[u8], offset: usize, format: Format) -> Result<u32> {
    let bytes = offset
        .checked_add(4)
        .and_then(|end| data.get(offset..end))
        .ok_or_else(|| Error::new(format, offset, ErrorKind::Truncated, "missing 32-bit word"))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(crate) fn buffer(size: usize, format: Format) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    output.try_reserve_exact(size).map_err(|_| {
        Error::new(
            format,
            0,
            ErrorKind::AllocationFailed,
            "cannot allocate output",
        )
    })?;
    Ok(output)
}
