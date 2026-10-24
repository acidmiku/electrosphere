//! A bounded address-preserving main-RAM buffer, not an emulated PSX bus.
use std::{fmt, ops::Range};

pub const RAM_SIZE: usize = 2 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct AddressError {
    pub address: u32,
    pub length: usize,
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported main-RAM range {:#010X} + {:#X}",
            self.address, self.length
        )
    }
}
impl std::error::Error for AddressError {}

/// Supports the physical, cached and uncached aliases of the first 2 MiB.
/// Hardware registers, scratchpad and the extra RAM mirrors are not implemented.
pub struct Ram {
    bytes: Box<[u8]>,
}

impl Default for Ram {
    fn default() -> Self {
        Self {
            bytes: vec![0; RAM_SIZE].into_boxed_slice(),
        }
    }
}

impl Ram {
    fn range(address: u32, length: usize) -> Result<Range<usize>, AddressError> {
        let err = || AddressError { address, length };
        if !matches!(address >> 29, 0 | 4 | 5) {
            return Err(err());
        }
        let offset = (address & 0x1fff_ffff) as usize;
        let end = offset.checked_add(length).ok_or_else(err)?;
        if offset >= RAM_SIZE || end > RAM_SIZE {
            return Err(err());
        }
        Ok(offset..end)
    }

    pub fn read(&self, address: u32, length: usize) -> Result<&[u8], AddressError> {
        Ok(&self.bytes[Self::range(address, length)?])
    }
    pub fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), AddressError> {
        let range = Self::range(address, bytes.len())?;
        self.bytes[range].copy_from_slice(bytes);
        Ok(())
    }
    pub fn read_u32(&self, address: u32) -> Result<u32, AddressError> {
        let b = self.read(address, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn clear(&mut self, address: u32, length: usize) -> Result<(), AddressError> {
        let range = Self::range(address, length)?;
        self.bytes[range].fill(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_share_bytes_and_little_endian_words() {
        let mut ram = Ram::default();
        ram.write(0x80010000, &[0x78, 0x56, 0x34, 0x12]).unwrap();
        assert_eq!(ram.read_u32(0x10000).unwrap(), 0x12345678);
        assert_eq!(ram.read_u32(0xa0010000).unwrap(), 0x12345678);
    }
    #[test]
    fn unsupported_bus_addresses_and_crossing_ranges_fail_atomically() {
        let mut ram = Ram::default();
        for a in [0x1f800000, 0x1f801810, 0xbfc00000, 0x20000000, 0x80200000] {
            assert!(ram.read(a, 1).is_err());
        }
        assert!(ram.write(0x801fffff, &[1, 2]).is_err());
        assert_eq!(ram.read(0x801fffff, 1).unwrap(), &[0]);
        assert!(ram.read(0x80000000, usize::MAX).is_err());
    }
}
