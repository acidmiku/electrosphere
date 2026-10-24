//! Unrelocated AC3 resource tables: a count followed by relative child offsets.
//!
//! Resident 0x80026E98 uses the low 16 bits of the first word as the count and
//! adds the container address to each unrelocated pointer. The host keeps slices
//! instead of rewriting guest pointers. Child extents require ascending offsets;
//! already relocated tables and aliasing/unsorted tables are rejected explicitly.
use crate::{Error, ErrorKind, Format, Result, word};
use std::ops::Range;

#[derive(Debug)]
pub struct Container<'a> {
    data: &'a [u8],
    pub header: u32,
    ranges: Vec<Range<usize>>,
}

impl<'a> Container<'a> {
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    pub fn range(&self, index: usize) -> Result<Range<usize>> {
        self.ranges
            .get(index)
            .cloned()
            .ok_or_else(|| error(0, ErrorKind::InvalidOffset, "child index outside directory"))
    }

    pub fn child(&self, index: usize) -> Result<&'a [u8]> {
        Ok(&self.data[self.range(index)?])
    }
}

fn error(offset: usize, kind: ErrorKind, detail: &'static str) -> Error {
    Error::new(Format::Dot1, offset, kind, detail)
}

pub fn parse(data: &[u8]) -> Result<Container<'_>> {
    let header = word(data, 0, Format::Dot1)?;
    let count = (header & 0xffff) as usize;
    let table_end = 4 + count * 4;
    if table_end > data.len() {
        return Err(error(4, ErrorKind::Truncated, "truncated child table"));
    }
    let mut ranges = Vec::with_capacity(count);
    let mut previous: Option<usize> = None;
    for index in 0..count {
        let table_offset = 4 + index * 4;
        let offset = word(data, table_offset, Format::Dot1)? as usize;
        if offset & 0x8000_0000 != 0 {
            return Err(error(
                table_offset,
                ErrorKind::Unsupported,
                "already relocated child pointer",
            ));
        }
        if offset < table_end || offset >= data.len() || offset & 3 != 0 {
            return Err(error(
                table_offset,
                ErrorKind::InvalidOffset,
                "child offset outside data or unaligned",
            ));
        }
        if let Some(start) = previous {
            if offset <= start {
                return Err(error(
                    table_offset,
                    ErrorKind::OverlappingExtent,
                    "child offsets must increase",
                ));
            }
            ranges.push(start..offset);
        }
        previous = Some(offset);
    }
    if let Some(start) = previous {
        ranges.push(start..data.len());
    }
    Ok(Container {
        data,
        header,
        ranges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_children_keep_padding_and_count_uses_low_halfword() {
        let words = [0x10002_u32, 16, 20, 0, 0x12345678, 0x87654321];
        let bytes: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let table = parse(&bytes).unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table.range(0).unwrap(), 16..20);
        assert_eq!(table.child(1).unwrap(), 0x87654321_u32.to_le_bytes());
        assert!(table.child(2).is_err());
    }

    #[test]
    fn invalid_tables_fail_without_slicing_outside_input() {
        for offsets in [
            [4, 16],
            [16, 16],
            [20, 16],
            [16, 99],
            [16, 0x80000014],
            [17, 20],
        ] {
            let words = [2_u32, offsets[0], offsets[1], 0, 0, 0];
            let bytes: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
            assert!(parse(&bytes).is_err(), "{offsets:?}");
        }
        assert!(parse(&[]).is_err());
        assert!(parse(&[2, 0, 0, 0]).is_err());
        assert!(parse(&[0; 4]).unwrap().is_empty());
    }
}
