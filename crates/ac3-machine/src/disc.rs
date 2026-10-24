//! The boot's disc: [`DiscDevice`], a host device answering the blocking
//! libcd file calls (CdSearchFile, CdControlB Setloc, CdRead, CdReadSync)
//! from a raw disc image. Version-independent: the only game RAM address it
//! knows is the libcd file cache it returns entries of, given by the version
//! at construction.
use crate::host_ids::{CD_CONTROL_B, CD_READ, CD_READ_SYNC, CD_SEARCH_FILE};
use crate::machine::{Device, HostCall};

/// A host [`Device`] answering the boot's CD calls from a raw disc image of
/// 2352-byte sectors (Mode 2 Form 1 user data at byte 24); every other call
/// goes to `inner`. The drive is not emulated: it supplies what the CD
/// routines leave in RAM.
///
/// - CdSearchFile(fp, name): looks `name` (a backslash and an ISO name) up
///   in the root directory of the disc it holds (after a disc change, the
///   new one: [`DiscDevice::set_image`]); fills `fp` with the position (BCD
///   minute, second, sector of lba + 150, track 0) and size and returns
///   libcd's cached entry (`file_cache` + 24 * directory index, counting `.`
///   and `..`; PAL 0x800C5CD0, JP 0x800C7EF8), else 0.
/// - CdControlB(CdlSetloc, loc, result): seeks to `loc`; the 8 result bytes
///   become the drive status 0x02 (spinning, idle) and zeros; returns 1.
/// - CdRead(n, buf, mode): writes `n` sectors from the seek position to
///   `buf` and advances it; returns 1. CdReadSync(0, result) returns 0.
pub struct DiscDevice<R> {
    image: R,
    position: u32,
    /// libcd's file cache (24-byte CdlFILE entries) of the game version:
    /// CdSearchFile's results point into it. Configuration, not device
    /// state.
    file_cache: u32,
    pub inner: Option<Box<dyn Device>>,
}

/// The device's state for save states: the seek position (the image,
/// `inner` and the file cache address stay with the device).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DiscDeviceState {
    position: u32,
}

impl<R> DiscDevice<R> {
    /// The device's state (see [`DiscDeviceState`]).
    pub fn snapshot(&self) -> DiscDeviceState {
        // Every field named: a new one must be placed here or excluded.
        let Self {
            image: _,
            position,
            file_cache: _,
            inner: _,
        } = self;
        DiscDeviceState {
            position: *position,
        }
    }
    /// Replace the device's state, keeping its image, `inner` and file
    /// cache address.
    pub fn restore(&mut self, state: DiscDeviceState) {
        let DiscDeviceState { position } = state;
        self.position = position;
    }
    /// Read another disc from now on (a disc change: the host gives it the
    /// image the CD drive now holds); returns the previous image. The seek
    /// position stays, as the drive's does.
    pub fn set_image(&mut self, image: R) -> R {
        std::mem::replace(&mut self.image, image)
    }
}

impl<R: std::io::Read + std::io::Seek> DiscDevice<R> {
    /// The device on `image`, returning entries of the libcd file cache at
    /// `file_cache` (the game version's address).
    pub fn new(image: R, file_cache: u32) -> Self {
        Self {
            image,
            position: 0,
            file_cache,
            inner: None,
        }
    }
    fn read(&mut self, lba: u32) -> std::io::Result<[u8; 2048]> {
        let mut data = [0; 2048];
        self.image
            .seek(std::io::SeekFrom::Start(u64::from(lba) * 2352 + 24))?;
        self.image.read_exact(&mut data)?;
        Ok(data)
    }
    /// Root-directory lookup: (lba, size, record index).
    fn search(&mut self, name: &[u8]) -> Option<(u32, u32, u32)> {
        let name = name.strip_prefix(b"\\")?;
        if name.contains(&b'\\') {
            return None;
        }
        let word = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let volume = self.read(16).ok()?;
        let root = &volume[156..190];
        let (start, size) = (word(&root[2..6]), word(&root[10..14]));
        let mut entry = 0;
        for sector in 0..size.div_ceil(2048) {
            let data = self.read(start + sector).ok()?;
            let mut offset = 0;
            while offset + 33 < data.len() {
                let length = usize::from(data[offset]);
                if length == 0 || offset + length > data.len() {
                    break;
                }
                let record = &data[offset..offset + length];
                let name_length = usize::from(record[32]);
                let record_name = record.get(33..33 + name_length)?;
                if record_name.eq_ignore_ascii_case(name) {
                    return Some((word(&record[2..6]), word(&record[10..14]), entry));
                }
                entry += 1;
                offset += length;
            }
        }
        None
    }
}

fn ram_index(address: u32) -> usize {
    (address & 0x1f_ffff) as usize
}

/// Device-side RAM access with the machine's 2 MiB wrap-around.
fn ram_read(ram: &[u8], address: u32) -> u8 {
    ram[ram_index(address)]
}

fn ram_write(ram: &mut [u8], address: u32, bytes: &[u8]) {
    for (i, &byte) in bytes.iter().enumerate() {
        ram[ram_index(address.wrapping_add(i as u32))] = byte;
    }
}

impl<R: std::io::Read + std::io::Seek> Device for DiscDevice<R> {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        let [a0, a1, a2, _] = call.args;
        match call.function {
            CD_SEARCH_FILE => {
                let name: Vec<u8> = (0..0x40)
                    .map(|i| ram_read(ram, a1.wrapping_add(i)))
                    .take_while(|&b| b != 0)
                    .collect();
                let Some((lba, size, entry)) = self.search(&name) else {
                    return Some(0);
                };
                let sector = lba + 150;
                let position = [
                    bcd(sector / 4500),
                    bcd(sector / 75 % 60),
                    bcd(sector % 75),
                    0,
                ];
                ram_write(ram, a0, &position);
                ram_write(ram, a0.wrapping_add(4), &size.to_le_bytes());
                Some(self.file_cache + entry * 0x18)
            }
            CD_CONTROL_B if a0 == 2 => {
                let decode = |byte: u8| u32::from(byte >> 4) * 10 + u32::from(byte & 0xf);
                let [minute, second, sector] =
                    std::array::from_fn(|i| decode(ram_read(ram, a1.wrapping_add(i as u32))));
                self.position = ((minute * 60 + second) * 75 + sector).wrapping_sub(150);
                if a2 != 0 {
                    ram_write(ram, a2, &[2, 0, 0, 0, 0, 0, 0, 0]);
                }
                Some(1)
            }
            CD_READ => {
                for i in 0..a0 {
                    let data = self.read(self.position).unwrap_or([0; 2048]);
                    ram_write(ram, a1.wrapping_add(i * 0x800), &data);
                    self.position = self.position.wrapping_add(1);
                }
                Some(1)
            }
            CD_READ_SYNC => Some(0),
            _ => self.inner.as_mut()?.call(ram, scratch, call),
        }
    }
}

fn bcd(value: u32) -> u8 {
    (((value / 10) << 4) | (value % 10)) as u8
}
