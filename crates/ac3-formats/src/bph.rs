//! AC3E type-3 BPH directory for raw ACE.BPB extents.

use crate::{Error, ErrorKind, Format, Result, word};

/// Conservative allocation limit; the recovered PAL directory has 127 entries.
pub const MAX_ENTRIES: usize = 65_536;
pub const SECTOR_SIZE: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extent {
    pub offset: usize,
    pub size: usize,
}

impl Extent {
    /// Recheck the extent when slicing a body, including a different body.
    pub fn bytes<'a>(&self, body: &'a [u8]) -> Result<&'a [u8]> {
        self.offset
            .checked_add(self.size)
            .and_then(|end| body.get(self.offset..end))
            .ok_or_else(|| {
                error(
                    self.offset,
                    ErrorKind::InvalidOffset,
                    "extent outside BPB body",
                )
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub index: usize,
    pub flags: u32,
    pub sector: u32,
    pub directory_offset: usize,
    /// `None` preserves an unsupported special entry without inventing an extent.
    pub extent: Option<Extent>,
}

impl Entry {
    /// Original resource-loader mode, bits 27..28. Validated raw entries use 0.
    pub fn mode(&self) -> u8 {
        ((self.flags >> 27) & 3) as u8
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub entries: Vec<Entry>,
}

fn error(offset: usize, kind: ErrorKind, detail: &'static str) -> Error {
    Error::new(Format::Bph, offset, kind, detail)
}

/// Parse all directory records and validate ordinary raw extents against BPB size.
///
/// Flags bit 31 marks an ordinary entry. Its low 19 bits count 32-bit words;
/// the second word gives a 2048-byte sector offset. Ordinary entries with
/// unsupported streaming flags are rejected, as in the validated Python reader.
/// Special entries remain visible with their raw flags and no guessed extent.
pub fn parse(header: &[u8], body_size: usize) -> Result<Directory> {
    if header.len() < 20 {
        return Err(error(
            header.len(),
            ErrorKind::Truncated,
            "truncated BPH header",
        ));
    }
    if &header[..4] != b"AC3E" {
        return Err(error(0, ErrorKind::InvalidMagic, "expected AC3E magic"));
    }
    if word(header, 4, Format::Bph)? != 3 {
        return Err(error(
            4,
            ErrorKind::Unsupported,
            "expected type-3 BPH directory",
        ));
    }
    let count = word(header, 16, Format::Bph)? as usize;
    if count > (header.len() - 20) / 8 {
        return Err(error(16, ErrorKind::Truncated, "truncated BPH directory"));
    }
    if count > MAX_ENTRIES {
        return Err(error(
            16,
            ErrorKind::InvalidSize,
            "directory exceeds entry limit",
        ));
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| error(16, ErrorKind::AllocationFailed, "cannot allocate directory"))?;
    let mut previous_end = 0;
    for index in 0..count {
        let directory_offset = 20 + index * 8;
        let flags = word(header, directory_offset, Format::Bph)?;
        let sector = word(header, directory_offset + 4, Format::Bph)?;
        let extent = if flags & 0x8000_0000 != 0 {
            if flags & 0x1ff8_0000 != 0 {
                return Err(error(
                    directory_offset,
                    ErrorKind::Unsupported,
                    "unsupported streaming flags",
                ));
            }
            let size = ((flags & 0x7_ffff) * 4) as usize;
            let offset = usize::try_from(u64::from(sector) * SECTOR_SIZE as u64).map_err(|_| {
                error(
                    directory_offset + 4,
                    ErrorKind::InvalidOffset,
                    "sector offset exceeds addressable size",
                )
            })?;
            let end = offset
                .checked_add(size)
                .filter(|&end| end <= body_size)
                .ok_or_else(|| {
                    error(
                        directory_offset,
                        ErrorKind::InvalidOffset,
                        "entry outside BPB body",
                    )
                })?;
            if offset < previous_end {
                return Err(error(
                    directory_offset,
                    ErrorKind::OverlappingExtent,
                    "overlapping or unsorted BPB extents",
                ));
            }
            previous_end = end;
            Some(Extent { offset, size })
        } else {
            None
        };
        entries.push(Entry {
            index,
            flags,
            sector,
            directory_offset,
            extent,
        });
    }
    Ok(Directory { entries })
}
