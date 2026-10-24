//! The Japanese release's movies on its discs: the SPH directory's mode-3
//! entries in `ACE.SPB`, the game's own stream format, and a frame decoder
//! that turns a frame into the picture the game shows.
//!
//! # The stream format (not STR)
//!
//! A movie is an SPH entry (resource `0x800 | index`, `ac3_audio::xa`) whose
//! flags say mode 3 and a channel. Its extent interleaves sectors of several
//! channels: XA audio (submode bit 2) and data sectors (submode bit 3). The
//! CD driver keeps the data sectors of the entry's channel (Setfilter), and
//! the ring (`cdstream::ring_sector`, 8002B760) builds frames from them:
//!
//! - user data starts with a 32-byte chunk header: +0 status 1, +1 sector
//!   in the frame, +2 sectors in the frame, +6 the frame number (u16), +8
//!   width, +0xA height (pixels), +0x10..+0x1F the subtitle line words of
//!   that frame (`hud::stream`, overlay mode 9);
//! - the payload follows: 2016 bytes in a Form 1 sector, 2296 in Form 2
//!   (the ring's 0x800- / 0x918-byte slots less the header);
//! - a frame begins at sector 0 of it and takes consecutive sectors; the
//!   frame's data is their payloads in order: an MDEC bit stream (a 12-byte
//!   header: run-level size word `0x3800_nnnn`, quantizer, version 2 or 3).
//!
//! **Frame numbers count down**: the first frame of a movie has the highest
//! number (frames - 1) and the last one 0. The player (states 80057720 and
//! 800FD028) ends the movie when the frame it VLC-decodes is number 0, so
//! that frame is never shown. A frame's *index* here is its position from
//! the start, `first - number`.
//!
//! One extent can hold several movies: the hub's 128x96 news clips share
//! interleaved extents and the SPH entries pick the channel (0x80E plays
//! channel 4 of the extent that 0x816 plays on channel 20).
//!
//! # Decoding as the game does
//!
//! [`FrameDecoder`] runs the translated libpress VLC decoder
//! (`ac3_flight_jp::states::pending_sdk::dec_dct_vlc2`, with
//! `DecDCTvlcBuild`'s table from the resident's own data) on a scratch
//! machine, and the host MDEC ([`MdecDevice`]) with the release's default
//! tables (DecDCTReset(0)). It reads the output slice by slice exactly as
//! the player's DMA-1 callback uploads it (16-pixel columns, `48 h` bytes in
//! 24-bit mode, `32 h` in 15-bit mode) and lays the columns out as VRAM
//! holds them, so the picture is the one the screen shows (24-bit: the MDEC
//! bytes; 15-bit: the 15-bit pixels expanded as the presenter does).
use crate::machine_mdec::{DefaultTables, Format, MdecDevice};
use ac3_audio::xa::StreamDirectory;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// Bytes of a raw CD sector.
pub const SECTOR_BYTES: usize = 2352;
/// The chunk header at the start of a data sector's user data.
pub const HEADER_BYTES: usize = 32;
/// Payload bytes after the header: Form 1 / Form 2 (the ring's slots).
pub const FORM1_PAYLOAD: usize = 0x800 - HEADER_BYTES;
pub const FORM2_PAYLOAD: usize = 0x918 - HEADER_BYTES;
/// The CD reads movie streams at double speed: 150 sectors a second.
pub const SECTORS_PER_SECOND: f64 = 150.0;
/// The Japanese release's field rate (NTSC).
pub const FIELDS_PER_SECOND: f64 = 60000.0 / 1001.0;

/// An ISO 9660 image read sector by sector (raw 2352-byte sectors).
struct Iso {
    file: File,
}

impl Iso {
    fn raw(&mut self, lba: u32, count: u32) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; count as usize * SECTOR_BYTES];
        self.file
            .seek(SeekFrom::Start(u64::from(lba) * SECTOR_BYTES as u64))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    /// The 2048 user bytes of a Mode 2 Form 1 (or Mode 1) sector.
    fn user(&mut self, lba: u32) -> io::Result<Vec<u8>> {
        let raw = self.raw(lba, 1)?;
        let start = if raw[15] == 2 { 24 } else { 16 };
        Ok(raw[start..start + 2048].to_vec())
    }
    /// (first sector, bytes) of the root-directory file `name`.
    fn root_file(&mut self, name: &str) -> io::Result<Option<(u32, u32)>> {
        let volume = self.user(16)?;
        if volume[0] != 1 || &volume[1..6] != b"CD001" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no ISO 9660 volume descriptor at sector 16",
            ));
        }
        let word = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
        let root = &volume[156..190];
        let (start, size) = (word(root, 2), word(root, 10));
        for sector in 0..size.div_ceil(2048).min(64) {
            let data = self.user(start + sector)?;
            let mut at = 0;
            while at + 33 < data.len() {
                let length = usize::from(data[at]);
                if length < 34 || at + length > data.len() {
                    break;
                }
                let record = &data[at..at + length];
                let found = record.get(33..33 + usize::from(record[32])).unwrap_or(&[]);
                let found = found.split(|&b| b == b';').next().unwrap_or(&[]);
                if found.eq_ignore_ascii_case(name.as_bytes()) {
                    return Ok(Some((word(record, 2), word(record, 10))));
                }
                at += length;
            }
        }
        Ok(None)
    }
}

/// A Japanese disc's movie data: its SPH directory and `ACE.SPB` extent.
pub struct MovieDisc {
    pub path: PathBuf,
    iso: Iso,
    sph: Vec<u8>,
    /// First sector of `ACE.SPB` (46540 on disc 1, 46613 on disc 2).
    pub spb_lba: u32,
    pub spb_sectors: u32,
    /// The SPH header's type word: the disc number (1 or 2).
    pub disc: u8,
}

impl MovieDisc {
    /// Open the disc image (or cue sheet) at `path`.
    pub fn open(path: &Path) -> crate::Result<Self> {
        let image = crate::version::image_path(path)?;
        let file = File::open(&image).map_err(|e| format!("{}: {e}", image.display()))?;
        let mut iso = Iso { file };
        let (sph_lba, sph_bytes) = iso
            .root_file("ACE.SPH")?
            .ok_or_else(|| format!("{}: no ACE.SPH", image.display()))?;
        let mut sph = Vec::new();
        for sector in 0..sph_bytes.div_ceil(2048) {
            sph.extend(iso.user(sph_lba + sector)?);
        }
        sph.truncate(sph_bytes as usize);
        let (spb_lba, spb_bytes) = iso
            .root_file("ACE.SPB")?
            .ok_or_else(|| format!("{}: no ACE.SPB", image.display()))?;
        // The SPB extent's sectors are mixed Form 1 / Form 2; its byte size
        // in the directory counts 2048 per sector.
        let spb_sectors = spb_bytes.div_ceil(2048);
        let directory = StreamDirectory::parse(&sph, u64::from(spb_sectors))?;
        let disc = directory.disc() as u8;
        Ok(Self {
            path: path.to_owned(),
            iso,
            sph,
            spb_lba,
            spb_sectors,
            disc,
        })
    }

    pub fn directory(&self) -> StreamDirectory<'_> {
        StreamDirectory::parse(&self.sph, u64::from(self.spb_sectors)).expect("checked at open")
    }

    /// `count` raw sectors from SPB-relative sector `first`.
    pub fn read(&mut self, first: u32, count: u32) -> crate::Result<Vec<u8>> {
        Ok(self.iso.raw(self.spb_lba + first, count)?)
    }

    /// Every mode-3 entry present on this disc whose channel carries MDEC
    /// video (frames with a bit-stream header), with its frames demuxed.
    pub fn movies(&mut self) -> crate::Result<Vec<(MovieInfo, Vec<Frame>)>> {
        let entries: Vec<_> = {
            let directory = self.directory();
            (0..directory.len() as u16)
                .filter_map(|index| directory.entry(0x800 | index).ok())
                .filter(|entry| entry.mode == 3)
                .collect()
        };
        let mut movies = Vec::new();
        for entry in entries {
            let raw = self.read(entry.first_sector, entry.sector_count)?;
            let frames = demux(&raw, entry.channel);
            if let Some(info) = MovieInfo::of(self, &entry, &raw, &frames) {
                movies.push((info, frames));
            }
        }
        Ok(movies)
    }

    /// The movie of SPH resource `resource`, or `None` when it is not a
    /// movie on this disc.
    pub fn movie(&mut self, resource: u16) -> crate::Result<Option<(MovieInfo, Vec<Frame>)>> {
        let Ok(entry) = self.directory().entry(resource & 0x1fff & !0x1000) else {
            return Ok(None);
        };
        if entry.mode != 3 {
            return Ok(None);
        }
        let raw = self.read(entry.first_sector, entry.sector_count)?;
        let frames = demux(&raw, entry.channel);
        Ok(MovieInfo::of(self, &entry, &raw, &frames).map(|info| (info, frames)))
    }

    /// Just the first frame number of `resource`'s movie (its frame count -
    /// 1; the sectors up to its first video sector are read) and the frame
    /// rate the drive delivers it at (frames over the extent at 150 sectors
    /// a second).
    pub fn movie_facts(&mut self, resource: u16) -> crate::Result<Option<(u16, f64)>> {
        let Ok(entry) = self.directory().entry(resource & 0x1fff & !0x1000) else {
            return Ok(None);
        };
        if entry.mode != 3 {
            return Ok(None);
        }
        let mut at = 0;
        while at < entry.sector_count {
            let count = (entry.sector_count - at).min(64);
            let raw = self.read(entry.first_sector + at, count)?;
            for sector in raw.chunks_exact(SECTOR_BYTES) {
                if let Some(chunk) = Chunk::of(sector)
                    && chunk.channel == entry.channel
                    && chunk.index == 0
                {
                    let frames = f64::from(chunk.number) + 1.0;
                    let fps = SECTORS_PER_SECOND * frames / f64::from(entry.sector_count);
                    return Ok(Some((chunk.number, fps)));
                }
            }
            at += count;
        }
        Ok(None)
    }
}

/// One data sector of a movie stream.
#[derive(Clone, Copy, Debug)]
struct Chunk<'a> {
    channel: u8,
    index: u8,
    count: u8,
    number: u16,
    header: &'a [u8],
    payload: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// A data sector with a chunk header of status 1 (the ring drops the
    /// rest).
    fn of(sector: &'a [u8]) -> Option<Self> {
        let submode = sector[18];
        if submode & 0x08 == 0 || submode & 0x04 != 0 || sector[24] != 1 {
            return None;
        }
        let payload = if submode & 0x20 != 0 {
            FORM2_PAYLOAD
        } else {
            FORM1_PAYLOAD
        };
        Some(Self {
            channel: sector[17],
            index: sector[25],
            count: sector[26],
            number: u16::from_le_bytes([sector[30], sector[31]]),
            header: &sector[24..24 + HEADER_BYTES],
            payload: &sector[24 + HEADER_BYTES..24 + HEADER_BYTES + payload],
        })
    }
}

/// A frame of a movie stream as the ring hands it to the player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The header's frame number (counting down to 0).
    pub number: u16,
    pub width: u16,
    pub height: u16,
    /// The chunk header of its first sector.
    pub header: [u8; HEADER_BYTES],
    /// The payloads of its sectors in order: the MDEC bit stream.
    pub data: Vec<u8>,
    /// SPB-relative position of its first sector within the extent.
    pub first_sector: u32,
}

impl Frame {
    /// Whether the data starts with an MDEC bit-stream header (run-level
    /// word 0x3800xxxx, version 2 or 3).
    pub fn is_video(&self) -> bool {
        self.data.len() >= 12
            && self.data[2..4] == [0x00, 0x38]
            && matches!(u16::from_le_bytes([self.data[6], self.data[7]]), 2 | 3)
            && self.width > 0
            && self.height > 0
            && self.width.is_multiple_of(16)
            && self.height.is_multiple_of(16)
    }
    /// The bit stream's version (2 or 3).
    pub fn version(&self) -> u16 {
        u16::from_le_bytes([self.data[6], self.data[7]])
    }
}

/// The frames of `channel` in the raw sectors `raw`, by the ring's rules
/// (8002B760): a frame starts at its sector 0 and takes the following
/// sectors of the same number in order; an out-of-order sector is dropped;
/// a frame is complete with its last sector.
pub fn demux(raw: &[u8], channel: u8) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut current: Option<(Frame, u8, u8)> = None;
    for (at, sector) in raw.chunks_exact(SECTOR_BYTES).enumerate() {
        let Some(chunk) = Chunk::of(sector) else {
            continue;
        };
        if chunk.channel != channel {
            continue;
        }
        let continues = current
            .as_ref()
            .is_some_and(|(frame, _, _)| frame.number == chunk.number);
        if continues {
            let (frame, last, _) = current.as_mut().unwrap();
            if chunk.index != last.wrapping_add(1) {
                continue;
            }
            *last = chunk.index;
            frame.data.extend_from_slice(chunk.payload);
        } else {
            if chunk.index != 0 {
                continue;
            }
            let header: [u8; HEADER_BYTES] = chunk.header.try_into().unwrap();
            current = Some((
                Frame {
                    number: chunk.number,
                    width: u16::from_le_bytes([header[8], header[9]]),
                    height: u16::from_le_bytes([header[10], header[11]]),
                    header,
                    data: chunk.payload.to_vec(),
                    first_sector: at as u32,
                },
                0,
                chunk.count,
            ));
        }
        if let Some((_, last, count)) = &current
            && last.wrapping_add(1) == *count
        {
            frames.push(current.take().unwrap().0);
        }
    }
    frames
}

/// XA audio of a movie's channel.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct AudioInfo {
    /// XA file and channel numbers (subheader bytes 0 and 1).
    pub file: u8,
    pub channel: u8,
    pub sectors: u32,
    pub rate: u32,
    pub stereo: bool,
    pub seconds: f64,
}

/// What the manifest says about a movie.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct MovieInfo {
    /// The SPH resource the game asks for (`0x800 | index`).
    pub resource: u16,
    /// The data channel of its video (the entry's channel).
    pub channel: u8,
    /// The disc it was read from (1 or 2).
    pub disc: u8,
    /// Absolute first sector of its extent, and the extent's length.
    pub lba: u32,
    pub sectors: u32,
    pub frames: u32,
    /// The first frame's number (the count-down's start).
    pub first_number: u16,
    pub width: u16,
    pub height: u16,
    /// Frames per second as the drive delivers them (150 sectors a second
    /// over the extent), and the fields per frame at 59.94 Hz.
    pub fps: f64,
    pub vblanks_per_frame: f64,
    /// Bit-stream versions seen.
    pub versions: Vec<u16>,
    pub form2: bool,
    pub audio: Option<AudioInfo>,
    /// SHA-256 of the frames (headers and data): the content identity.
    pub content: String,
}

impl MovieInfo {
    fn of(
        disc: &MovieDisc,
        entry: &ac3_audio::xa::StreamEntry,
        raw: &[u8],
        frames: &[Frame],
    ) -> Option<Self> {
        let first = frames.first()?;
        if !first.is_video() {
            return None;
        }
        let mut versions: Vec<u16> = frames
            .iter()
            .filter(|f| f.is_video())
            .map(Frame::version)
            .collect();
        versions.sort_unstable();
        versions.dedup();
        let mut audio: Option<AudioInfo> = None;
        let mut form2 = false;
        for sector in raw.chunks_exact(SECTOR_BYTES) {
            let (file, channel, submode, coding) = (sector[16], sector[17], sector[18], sector[19]);
            if submode & 0x04 != 0 && channel == entry.channel {
                let stereo = coding & 1 != 0;
                let rate = if coding & 4 != 0 { 18_900 } else { 37_800 };
                let info = audio.get_or_insert(AudioInfo {
                    file,
                    channel,
                    sectors: 0,
                    rate,
                    stereo,
                    seconds: 0.0,
                });
                info.sectors += 1;
                let frames_per_sector = if stereo { 2016.0 } else { 4032.0 };
                info.seconds += frames_per_sector / f64::from(rate);
            } else if submode & 0x08 != 0 && channel == entry.channel && sector[24] == 1 {
                form2 |= submode & 0x20 != 0;
            }
        }
        let content = {
            use sha2::{Digest, Sha256};
            let mut hash = Sha256::new();
            for frame in frames {
                hash.update(frame.header);
                hash.update(&frame.data);
            }
            format!("{:x}", hash.finalize())
        };
        let fps = SECTORS_PER_SECOND * frames.len() as f64 / f64::from(entry.sector_count);
        Some(Self {
            resource: entry.resource,
            channel: entry.channel,
            disc: disc.disc,
            lba: disc.spb_lba + entry.first_sector,
            sectors: entry.sector_count,
            frames: frames.len() as u32,
            first_number: first.number,
            width: first.width,
            height: first.height,
            fps,
            vblanks_per_frame: FIELDS_PER_SECOND / fps,
            versions,
            form2,
            audio,
            content,
        })
    }

    /// The movie's stable key: its SPH resource (`movie_09e6`).
    pub fn key(&self) -> String {
        key_of(self.resource)
    }
}

/// The pack and export key of SPH resource `resource` (bit 12, the stream
/// control flag, ignored).
pub fn key_of(resource: u16) -> String {
    format!("movie_{:04x}", resource & 0x0fff)
}

/// MDEC output depth of a player.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum Depth {
    /// 24-bit (the state-5 player's full-screen movies).
    Rgb24,
    /// 15-bit (in-mission event movies and the hub's clips).
    Rgb15,
}

impl Depth {
    pub fn bits(self) -> u32 {
        match self {
            Self::Rgb24 => 24,
            Self::Rgb15 => 15,
        }
    }
    /// VRAM halfwords per 16-pixel slice column.
    pub fn slice_halfwords(self) -> usize {
        match self {
            Self::Rgb24 => 24,
            Self::Rgb15 => 16,
        }
    }
}

/// libpress's VLC table (DecDCTvlcBuild) and the scratch buffers of
/// [`FrameDecoder`] in its machine's RAM.
const VLC_TABLE: u32 = 0x8010_0000;
const BIT_STREAM: u32 = 0x8012_0000;
const RUN_LEVEL: u32 = 0x8014_0000;
/// The largest frame the scratch buffers take.
const MAX_FRAME_BYTES: usize = 0x2_0000;

/// Decodes a movie's frames as the game does: VLC on the translated code,
/// then the host MDEC, read out slice by slice ([`decode`](Self::decode)).
pub struct FrameDecoder {
    machine: ac3_flight_jp::machine::Machine,
    mdec: MdecDevice,
    pub depth: Depth,
}

impl FrameDecoder {
    /// A decoder for `depth` with the Japanese resident `payload` (its
    /// VLC table data and default MDEC tables).
    pub fn new(payload: &[u8], depth: Depth) -> Self {
        use ac3_flight_jp::states::pending_sdk;
        let mut machine = ac3_flight_jp::machine::Machine::default();
        ac3_flight_jp::boot::load_payload(&mut machine, payload);
        pending_sdk::dec_dct_vlc_size2(&mut machine, 0);
        pending_sdk::dec_dct_vlc_build(&mut machine, VLC_TABLE);
        let mut decoder = Self {
            machine,
            mdec: MdecDevice::new(),
            depth,
        };
        decoder.reset();
        decoder
    }

    /// Output transfers that found too little output (none for a sound
    /// movie).
    pub fn stalls(&self) -> u64 {
        self.mdec.stalls
    }

    /// DecDCTReset(0): no pending output, the release's default tables.
    pub fn reset(&mut self) {
        let tables = DefaultTables::JP;
        self.mdec = MdecDevice::new();
        self.mdec.set_release(tables);
        for (address, words) in [(tables.quant, 0x20), (tables.scale, 0x20)] {
            let command = self.machine.peek(address);
            let parameters = self.machine.read_bytes(address + 4, words * 4);
            self.mdec.command(command, &parameters);
        }
    }

    /// The MDEC command DecDCTin sends for `header` (the run-level size
    /// word) in this depth: 24-bit clears bit 27, 15-bit sets it; bit 25
    /// (mode bit 1, as both players pass it) set.
    pub fn command(&self, header: u32) -> u32 {
        let header = match self.depth {
            Depth::Rgb24 => header & 0xf7ff_ffff,
            Depth::Rgb15 => header | 0x0800_0000,
        };
        header | 0x0200_0000
    }

    /// The MDEC's run-level input of `frame`: the command word and its
    /// parameter bytes, as DecDCTin sends them.
    pub fn run_level(&mut self, frame: &Frame) -> Option<(u32, Vec<u8>)> {
        use ac3_flight_jp::states::pending_sdk;
        if !frame.is_video() || frame.data.len() > MAX_FRAME_BYTES {
            return None;
        }
        let m = &mut self.machine;
        m.write_bytes(BIT_STREAM, &frame.data);
        // The bit reader may look one halfword past the data.
        m.write_bytes(BIT_STREAM + frame.data.len() as u32, &[0; 8]);
        if pending_sdk::dec_dct_vlc2(m, BIT_STREAM, RUN_LEVEL, VLC_TABLE) != 0 {
            return None;
        }
        let header = m.peek(RUN_LEVEL);
        let words = usize::from(m.peek_half(RUN_LEVEL));
        let parameters = m.read_bytes(RUN_LEVEL + 4, words * 4);
        Some((self.command(header), parameters))
    }

    /// Decode `frame` into its picture (`width * height` 0RGB words), with
    /// the MDEC state carried from the frame before as in the player.
    /// `None` for a frame that is not MDEC video.
    pub fn decode(&mut self, frame: &Frame) -> Option<Vec<u32>> {
        let (command, parameters) = self.run_level(frame)?;
        self.mdec
            .decode(Format::from_command(command), &halfwords(&parameters));
        let (w, h) = (usize::from(frame.width), usize::from(frame.height));
        let slice_bytes = h * self.depth.slice_halfwords() * 2;
        let mut picture = vec![0u32; w * h];
        let mut bytes = vec![0u8; slice_bytes];
        for column in 0..w.div_ceil(16) {
            self.mdec.read_output_into(&mut bytes);
            for y in 0..h {
                for x in 0..16.min(w - column * 16) {
                    let rgb = match self.depth {
                        Depth::Rgb24 => {
                            let at = y * 48 + x * 3;
                            (u32::from(bytes[at]) << 16)
                                | (u32::from(bytes[at + 1]) << 8)
                                | u32::from(bytes[at + 2])
                        }
                        Depth::Rgb15 => {
                            let at = y * 32 + x * 2;
                            rgb15(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
                        }
                    };
                    picture[y * w + column * 16 + x] = rgb;
                }
            }
        }
        Some(picture)
    }
}

/// Little-endian halfwords of `bytes`.
fn halfwords(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect()
}

/// A 15-bit VRAM pixel as the presenter shows it (`ac3_graphics::rgba8`).
pub fn rgb15(pixel: u16) -> u32 {
    let [r, g, b, _] = ac3_graphics::rgba8(pixel);
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raw data sector of `channel` with the chunk header fields.
    fn sector(channel: u8, index: u8, count: u8, number: u16, fill: u8, form2: bool) -> Vec<u8> {
        let mut s = vec![0u8; SECTOR_BYTES];
        s[1..11].fill(0xff);
        s[15] = 2;
        s[17] = channel;
        s[18] = 0x48 | if form2 { 0x20 } else { 0 };
        s[24] = 1;
        s[25] = index;
        s[26] = count;
        s[30..32].copy_from_slice(&number.to_le_bytes());
        s[32..34].copy_from_slice(&320u16.to_le_bytes());
        s[34..36].copy_from_slice(&176u16.to_le_bytes());
        s[56..].fill(fill);
        s
    }

    #[test]
    fn demux_follows_the_ring() {
        let mut raw = Vec::new();
        // Frame 9: two sectors, an audio sector between them, channel 4 noise.
        raw.extend(sector(0, 0, 2, 9, 1, false));
        let mut audio = vec![0u8; SECTOR_BYTES];
        audio[18] = 0x64;
        raw.extend(audio);
        raw.extend(sector(4, 0, 1, 3, 7, false));
        raw.extend(sector(0, 1, 2, 9, 2, false));
        // Frame 8 starts at sector 1: dropped; frame 7 skips sector 1: dropped.
        raw.extend(sector(0, 1, 2, 8, 3, false));
        raw.extend(sector(0, 0, 3, 7, 4, false));
        raw.extend(sector(0, 2, 3, 7, 4, false));
        // Frame 6 in Form 2.
        raw.extend(sector(0, 0, 1, 6, 5, true));
        let frames = demux(&raw, 0);
        assert_eq!(frames.len(), 2);
        assert_eq!(
            (frames[0].number, frames[0].width, frames[0].height),
            (9, 320, 176)
        );
        assert_eq!(frames[0].data.len(), 2 * FORM1_PAYLOAD);
        assert!(frames[0].data[..FORM1_PAYLOAD].iter().all(|&b| b == 1));
        assert!(frames[0].data[FORM1_PAYLOAD..].iter().all(|&b| b == 2));
        assert_eq!(frames[0].first_sector, 0);
        assert_eq!(frames[1].number, 6);
        assert_eq!(frames[1].data.len(), FORM2_PAYLOAD);
        assert_eq!(demux(&raw, 4).len(), 1);
    }

    #[test]
    fn keys_name_the_resource() {
        assert_eq!(key_of(0x9e6), "movie_09e6");
        assert_eq!(key_of(0x1820), "movie_0820");
    }
}
