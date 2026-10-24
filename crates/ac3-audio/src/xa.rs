//! Checked SPH resource metadata and bounded XA-ADPCM decoding.
//!
//! The decoder implements every XA profile on AC3's discs: 4-bit ADPCM,
//! stereo or mono, at 37800 or 18900 Hz ([`Coding`]). PAL uses stereo at
//! both rates (18900 Hz in most movies); the Japanese release adds mono
//! 18900 Hz (voice clips and some movies). No disc sector uses 8-bit
//! samples or emphasis; those are rejected. A caller owns file I/O, disc seek
//! latency, start/stop and mixing. Decoder state starts at zero and continues
//! across selected sectors.
//!
//! Every profile goes through the same 37800-Hz resampler to 44100 Hz. Mono
//! samples enter both channels. At 18900 Hz each source frame enters it
//! twice: first the mean of the channel's previous sample and the new one
//! (rounded down), then the new one. That rule was found and checked on
//! the pinned emulator's output (`tests/xa_jp.rs`): every sample of 65
//! stereo and 16 mono 18900-Hz sectors of the Japanese disc 1.
use crate::{Error, Result, StereoPcm, xa_coefficients::ZIGZAG};

pub const RAW_SECTOR_BYTES: usize = 2352;
/// Output frames (44.1 kHz) of one stereo 37800-Hz sector, the PAL menu and
/// briefing profile. Other profiles yield [`Coding::output_frames`].
pub const OUTPUT_FRAMES_PER_SECTOR: usize = 2352;
/// The most output frames one sector yields (mono 18900 Hz).
pub const MAX_OUTPUT_FRAMES_PER_SECTOR: usize = 4 * OUTPUT_FRAMES_PER_SECTOR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamEntry {
    pub resource: u16,
    pub flags: u32,
    pub mode: u8,
    pub channel: u8,
    /// Sector offset in ACE.SPB.raw2352; the ISO extent begins at LBA 17388
    /// on the PAL disc (46540 / 46613 on the Japanese discs 1 / 2).
    pub first_sector: u32,
    /// Physical sectors, including interleaved channels and data sectors.
    pub sector_count: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct StreamDirectory<'a> {
    records: &'a [u8],
    raw_sectors: u64,
    kind: u32,
}

impl<'a> StreamDirectory<'a> {
    /// Parse an AC3E SPH directory: type 1 (PAL, Japanese disc 1) or type 2
    /// (Japanese disc 2; the type word is the disc number). The extent of
    /// every entry present on this disc (flags bit 31) is checked eagerly;
    /// the Japanese discs keep the entries of the other disc with bit 31
    /// clear, and [`entry`](Self::entry) refuses those.
    pub fn parse(header: &'a [u8], raw_sectors: u64) -> Result<Self> {
        if header.len() < 20 {
            return Err(Error::Truncated("SPH header"));
        }
        let kind = u32::from_le_bytes(header[4..8].try_into().unwrap());
        if &header[..4] != b"AC3E" || !matches!(kind, 1 | 2) {
            return Err(Error::Invalid(
                "expected type-1 or type-2 AC3E SPH directory",
            ));
        }
        let count = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        if count > 2048 {
            return Err(Error::LimitExceeded("SPH resource IDs"));
        }
        let records = header
            .get(20..20 + count * 8)
            .ok_or(Error::Truncated("SPH entries"))?;
        let directory = Self {
            records,
            raw_sectors,
            kind,
        };
        for index in 0..count {
            if directory.present(index) {
                directory.entry(0x800 | index as u16)?;
            }
        }
        Ok(directory)
    }

    pub fn len(&self) -> usize {
        self.records.len() / 8
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    /// The header's type word: 1, or 2 on the Japanese disc 2 (the disc
    /// number the game reads from it).
    pub fn disc(&self) -> u32 {
        self.kind
    }
    fn flags(&self, index: usize) -> Option<u32> {
        let record = self.records.get(index * 8..index * 8 + 4)?;
        Some(u32::from_le_bytes(record.try_into().unwrap()))
    }
    fn present(&self, index: usize) -> bool {
        self.flags(index)
            .is_some_and(|flags| flags & 0x8000_0000 != 0)
    }

    /// Bit11 selects SPH; bit12 is the original stream-control flag and does
    /// not change directory indexing. Other high bits are rejected.
    pub fn entry(&self, resource: u16) -> Result<StreamEntry> {
        if resource & 0x800 == 0 || resource & !0x1fff != 0 {
            return Err(Error::Invalid("resource does not select SPH"));
        }
        let index = usize::from(resource & 0x7ff);
        let record = self
            .records
            .get(index * 8..index * 8 + 8)
            .ok_or(Error::Invalid("SPH resource index"))?;
        let flags = u32::from_le_bytes(record[..4].try_into().unwrap());
        let first_sector = u32::from_le_bytes(record[4..].try_into().unwrap());
        let mode = ((flags >> 27) & 3) as u8;
        if flags & 0x80000000 == 0 {
            return Err(Error::Invalid("SPH resource is not on this disc"));
        }
        if !matches!(mode, 2 | 3) {
            return Err(Error::Invalid("unsupported SPH entry mode"));
        }
        let sector_count = (flags & 0x7ffff) << ((flags >> 24) & 7);
        if sector_count == 0 || u64::from(first_sector) + u64::from(sector_count) > self.raw_sectors
        {
            return Err(Error::Invalid("SPH extent outside raw SPB"));
        }
        Ok(StreamEntry {
            resource,
            flags,
            mode,
            channel: ((flags >> 19) & 31) as u8,
            first_sector,
            sector_count,
        })
    }
}

/// The coding of an XA audio sector (subheader byte 3) the decoder plays:
/// 4-bit ADPCM, stereo (bit 0) or mono, 18900 Hz (bit 2) or 37800 Hz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Coding {
    pub stereo: bool,
    pub half_rate: bool,
}

impl Coding {
    /// The PAL menu and briefing profile.
    pub const STEREO_37800: Self = Self {
        stereo: true,
        half_rate: false,
    };

    /// Stereo or mono, 37800 or 18900 Hz; 8-bit samples, emphasis and the
    /// reserved values are refused.
    pub fn parse(byte: u8) -> Result<Self> {
        if byte & !0x05 != 0 {
            return Err(Error::Invalid(
                "XA coding is not 4-bit mono/stereo 37800/18900-Hz audio",
            ));
        }
        Ok(Self {
            stereo: byte & 1 != 0,
            half_rate: byte & 4 != 0,
        })
    }
    /// The subheader byte.
    pub fn byte(self) -> u8 {
        u8::from(self.stereo) | (u8::from(self.half_rate) << 2)
    }
    pub fn sample_rate(self) -> u32 {
        if self.half_rate { 18900 } else { 37800 }
    }
    pub fn channels(self) -> usize {
        if self.stereo { 2 } else { 1 }
    }
    /// Source frames in one sector: 18 sound groups of 8 blocks of 28
    /// samples, shared by the channels.
    pub fn source_frames(self) -> usize {
        18 * 8 * 28 / self.channels()
    }
    /// Output frames at 44.1 kHz of one sector: at 18900 Hz every source
    /// frame enters the 37800-Hz resampler twice (see the module
    /// documentation), which turns every 6 into 7. 2352 (stereo 37800 Hz),
    /// 4704 (stereo 18900, mono 37800) or 9408 (mono 18900).
    pub fn output_frames(self) -> usize {
        self.source_frames() * if self.half_rate { 2 } else { 1 } * 7 / 6
    }
}

/// A fixed-size decoder state. `atv` follows the original CdMix byte order:
/// left-to-left, left-to-right, right-to-right, right-to-left.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct XaDecoder {
    file: u8,
    channel: u8,
    /// ADPCM predictor history per channel (mono uses the left one).
    history: [[i32; 2]; 2],
    /// The resampler's 37800-Hz input ring per output channel (mono samples
    /// enter both).
    ring: [[i16; 32]; 2],
    cursor: usize,
    phase: u8,
    atv: [u8; 4],
    ended: bool,
}

impl XaDecoder {
    pub fn new(file: u8, channel: u8, atv: [u8; 4]) -> Self {
        Self {
            file,
            channel,
            history: [[0; 2]; 2],
            ring: [[0; 32]; 2],
            cursor: 0,
            phase: 0,
            atv,
            ended: false,
        }
    }

    pub fn set_volume_matrix(&mut self, atv: [u8; 4]) {
        self.atv = atv;
    }

    /// Start another CD read: reset source predictors, preserving the SPU's
    /// interpolation history. This matches the pinned CD Read reset boundary;
    /// it does not flush any PCM already queued by the caller.
    pub fn select_stream(&mut self, file: u8, channel: u8) {
        self.file = file;
        self.channel = channel;
        self.history = [[0; 2]; 2];
        self.ended = false;
    }

    /// Return `None` for an interleaved sector outside this selected audio
    /// channel. A selected sector yields [`Coding::output_frames`] frames
    /// at 44100 Hz (2352 for the stereo 37800-Hz profile). The whole
    /// selected header is validated before decoder state changes. EDC/ECC
    /// repair, emphasis and 8-bit samples are unsupported.
    pub fn decode_sector(&mut self, raw: &[u8]) -> Result<Option<StereoPcm>> {
        if raw.len() != RAW_SECTOR_BYTES {
            return Err(Error::Invalid("XA raw sector length"));
        }
        if raw[..12] != [0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0] || raw[15] != 2 {
            return Err(Error::Invalid("XA Mode2 sector sync"));
        }
        if raw[16..20] != raw[20..24] {
            return Err(Error::Invalid("XA subheader copies differ"));
        }
        if self.ended || raw[16] != self.file || raw[17] != self.channel || raw[18] & 4 == 0 {
            return Ok(None);
        }
        if raw[18] & 0x6e != 0x64 {
            return Err(Error::Invalid("XA submode is not real-time Form 2 audio"));
        }
        let coding = Coding::parse(raw[19])?;
        let groups = &raw[24..24 + 18 * 128];
        for group in groups.chunks_exact(128) {
            if group[4..12].iter().any(|p| p >> 4 > 3 || p & 15 > 12) {
                return Err(Error::Invalid("XA predictor or range"));
            }
        }
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(coding.output_frames())
            .map_err(|_| Error::Allocation)?;
        for group in groups.chunks_exact(128) {
            if coding.stereo {
                // Blocks alternate left / right, 28 samples per block pair.
                for pair in 0..4 {
                    for sample in 0..28 {
                        let previous = [self.history[0][0], self.history[1][0]];
                        let left = self.decode_sample(group, pair * 2, sample, 0);
                        let right = self.decode_sample(group, pair * 2 + 1, sample, 1);
                        if coding.half_rate {
                            let between =
                                [midpoint(previous[0], left), midpoint(previous[1], right)];
                            self.resample(between, &mut frames);
                        }
                        self.resample([left, right], &mut frames);
                    }
                }
            } else {
                // Eight consecutive blocks of 28 samples.
                for block in 0..8 {
                    for sample in 0..28 {
                        let previous = self.history[0][0];
                        let value = self.decode_sample(group, block, sample, 0);
                        if coding.half_rate {
                            let between = midpoint(previous, value);
                            self.resample([between, between], &mut frames);
                        }
                        self.resample([value, value], &mut frames);
                    }
                }
            }
        }
        debug_assert_eq!(frames.len(), coding.output_frames());
        self.ended = raw[18] & 0x81 == 0x81;
        Ok(Some(StereoPcm { frames }))
    }

    /// Sample `sample` of 4-bit block `block` (0..7) of a sound group: the
    /// nibble scaled by the block's range, plus the filter's prediction from
    /// `channel`'s clamped history (the XA-specific rounded sum).
    fn decode_sample(&mut self, group: &[u8], block: usize, sample: usize, channel: usize) -> i16 {
        let parameter = group[4 + block];
        let coefficients = [(0, 0), (60, 0), (115, -52), (98, -55)][usize::from(parameter >> 4)];
        let nibble = (group[16 + sample * 4 + block / 2] >> ((block & 1) * 4)) & 15;
        let signed = i32::from((nibble as i16) << 12);
        let history = &mut self.history[channel];
        let predicted = (history[0] * coefficients.0 + history[1] * coefficients.1 + 32) >> 6;
        let value = ((signed >> (parameter & 15)) + predicted).clamp(-32768, 32767);
        *history = [value, history[0]];
        value as i16
    }

    /// One 37800-Hz frame into the 32-sample rings; every sixth yields seven
    /// 44100-Hz frames through the fixed filters, the CdMix matrix and the
    /// pinned emulator's integer gain and clipping.
    fn resample(&mut self, input: [i16; 2], frames: &mut Vec<[i16; 2]>) {
        for (channel, value) in input.into_iter().enumerate() {
            self.ring[channel][self.cursor] = value;
        }
        self.cursor = (self.cursor + 1) & 31;
        self.phase += 1;
        if self.phase != 6 {
            return;
        }
        self.phase = 0;
        for coefficients in ZIGZAG {
            let mut interpolated = [0_i64; 2];
            for (channel, value) in interpolated.iter_mut().enumerate() {
                let sum = coefficients
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        (i32::from(self.ring[channel][self.cursor.wrapping_sub(i + 1) & 31]) * c)
                            >> 15
                    })
                    .sum::<i32>();
                *value = i64::from(sum.clamp(-32768, 32767));
            }
            let [l, r] = interpolated;
            let left = l * i64::from(self.atv[0]) + r * i64::from(self.atv[3]);
            let right = r * i64::from(self.atv[2]) + l * i64::from(self.atv[1]);
            frames.push(
                [left, right]
                    .map(|v| ((((v * 33617) >> 22).clamp(-32768, 32767) * 31880) >> 15) as i16),
            );
        }
    }
}

/// The 18900-Hz in-between frame: the mean of a channel's previous source
/// sample (its predictor history, zero after a stream start) and the new
/// one, rounded down.
fn midpoint(previous: i32, value: i16) -> i16 {
    ((previous + i32::from(value)) >> 1) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sector() -> [u8; RAW_SECTOR_BYTES] {
        let mut raw = [0; RAW_SECTOR_BYTES];
        raw[1..11].fill(255);
        raw[15] = 2;
        raw[16..24].copy_from_slice(&[1, 16, 0x64, 1, 1, 16, 0x64, 1]);
        raw
    }

    /// `sector()` with coding byte `coding` in both subheader copies.
    fn coded(coding: u8) -> [u8; RAW_SECTOR_BYTES] {
        let mut raw = sector();
        raw[19] = coding;
        raw[23] = coding;
        raw
    }

    #[test]
    fn stereo_nibble_order_fixed_history_and_clamping() {
        let mut raw = sector();
        for group in raw[24..2328].chunks_exact_mut(128) {
            group[16..].fill(0x87);
        }
        let mut decoder = XaDecoder::new(1, 16, [128, 0, 128, 0]);
        let pcm = decoder.decode_sector(&raw).unwrap().unwrap();
        assert_eq!(pcm.frames.len(), 2352);
        assert_eq!(decoder.history, [[28672, 28672], [-32768, -32768]]);
        assert!(decoder.ring[0].iter().all(|v| *v == 28672));
        assert!(decoder.ring[1].iter().all(|v| *v == -32768));
        // Predictor1 continues from the prior sector, then clamps each sample
        // before retaining history. Channels must never share predictor state.
        for group in raw[24..2328].chunks_exact_mut(128) {
            group[4..12].fill(0x10);
        }
        decoder.decode_sector(&raw).unwrap();
        assert_eq!(decoder.history, [[32767, 32767], [-32768, -32768]]);
    }

    #[test]
    fn malformed_selected_sector_is_atomic_and_other_channels_are_ignored() {
        let mut raw = sector();
        let mut decoder = XaDecoder::new(1, 16, [150, 0, 150, 0]);
        let original = decoder.clone();
        raw[24 + 17 * 128 + 11] = 0x4c;
        assert!(decoder.decode_sector(&raw).is_err());
        assert_eq!(decoder, original);
        raw[17] = 20;
        raw[21] = 20;
        assert!(decoder.decode_sector(&raw).unwrap().is_none());
        assert_eq!(decoder, original);
        raw[21] = 16;
        assert!(decoder.decode_sector(&raw).is_err());
        assert!(decoder.decode_sector(&raw[..2351]).is_err());
    }

    #[test]
    fn composite_end_flag_and_read_reset_preserve_fir_history() {
        let mut raw = sector();
        for group in raw[24..2328].chunks_exact_mut(128) {
            group[16..].fill(0x12);
        }
        let mut decoder = XaDecoder::new(1, 16, [150, 0, 150, 0]);
        raw[18] |= 0x80;
        raw[22] = raw[18];
        decoder.decode_sector(&raw).unwrap().unwrap();
        assert!(!decoder.ended); // EOF alone does not stop XA playback.
        raw[18] |= 1;
        raw[22] = raw[18];
        decoder.decode_sector(&raw).unwrap().unwrap();
        assert!(decoder.ended);
        assert!(decoder.decode_sector(&raw).unwrap().is_none());
        let ring = decoder.ring;
        decoder.select_stream(1, 4);
        assert_eq!(decoder.history, [[0; 2]; 2]);
        assert_eq!(decoder.ring, ring);
        assert!(!decoder.ended);
        assert_eq!(decoder.channel, 4);
    }

    #[test]
    fn codings_and_their_frame_counts() {
        let profiles = [(0x01, 2, 37800, 2016, 2352), (0x05, 2, 18900, 2016, 4704)];
        let mono = [(0x00, 1, 37800, 4032, 4704), (0x04, 1, 18900, 4032, 9408)];
        for (byte, channels, rate, source, output) in profiles.into_iter().chain(mono) {
            let coding = Coding::parse(byte).unwrap();
            assert_eq!(coding.byte(), byte);
            assert_eq!(
                (
                    coding.channels(),
                    coding.sample_rate(),
                    coding.source_frames(),
                    coding.output_frames()
                ),
                (channels, rate, source, output)
            );
            let mut decoder = XaDecoder::new(1, 16, [128, 0, 128, 0]);
            let pcm = decoder.decode_sector(&coded(byte)).unwrap().unwrap();
            assert_eq!(pcm.frames.len(), output, "coding {byte:#x}");
        }
        assert_eq!(Coding::parse(1).unwrap(), Coding::STEREO_37800);
        assert_eq!(
            Coding::parse(4).unwrap().output_frames(),
            MAX_OUTPUT_FRAMES_PER_SECTOR
        );
        // 8-bit, emphasis and the reserved channel / rate values.
        for byte in [0x10, 0x11, 0x40, 0x41, 0x02, 0x03, 0x08, 0x09, 0x80] {
            assert!(Coding::parse(byte).is_err(), "coding {byte:#x}");
            let mut decoder = XaDecoder::new(1, 16, [128, 0, 128, 0]);
            let original = decoder.clone();
            assert!(decoder.decode_sector(&coded(byte)).is_err());
            assert_eq!(decoder, original, "refused before any state change");
        }
    }

    #[test]
    fn mono_blocks_run_in_order_through_one_predictor_into_both_rings() {
        // Group 0, block b holds nibble b+1 everywhere (filter 0, range 12),
        // so source samples step through 1..7, -8 in block order; groups
        // 1..17 hold zeros.
        let mut raw = coded(0x00);
        let group = &mut raw[24..24 + 128];
        group[4..12].fill(0x0c);
        for sample in 0..28 {
            for pair in 0..4 {
                let low = (pair * 2 + 1) as u8;
                group[16 + sample * 4 + pair] = low | ((low + 1) << 4);
            }
        }
        let mut decoder = XaDecoder::new(1, 16, [128, 0, 128, 0]);
        let first = &raw[24..24 + 128];
        let mut values = Vec::new();
        for block in 0..8 {
            for sample in 0..28 {
                values.push(decoder.decode_sample(first, block, sample, 0));
            }
        }
        assert_eq!(values[..28], [1; 28]);
        assert_eq!(values[28..56], [2; 28]);
        assert_eq!(values[28 * 7..], [-8; 28]);
        assert_eq!(decoder.history[1], [0, 0], "the right predictor is unused");
        // Both rings get every mono sample; left and right outputs agree.
        let mut decoder = XaDecoder::new(1, 16, [128, 0, 128, 0]);
        let frames = decoder.decode_sector(&raw).unwrap().unwrap().frames;
        assert_eq!(frames.len(), 4704);
        assert_eq!(decoder.ring[0], decoder.ring[1]);
        assert!(frames.iter().all(|[l, r]| l == r));
        assert!(frames.iter().any(|[l, _]| *l != 0));
        assert_eq!(decoder.history[1], [0, 0]);
    }

    #[test]
    fn half_rate_enters_the_midpoint_then_the_frame() {
        // Stereo 18900 Hz: every source frame enters the resampler after
        // the mean with its channel's previous sample.
        let mut raw = coded(0x05);
        for group in raw[24..2328].chunks_exact_mut(128) {
            for (i, byte) in group[16..].iter_mut().enumerate() {
                *byte = (i * 37 % 251) as u8;
            }
            group[4..12].copy_from_slice(&[0x08, 0x19, 0x2a, 0x3b, 0x07, 0x16, 0x25, 0x34]);
        }
        let mut half = XaDecoder::new(1, 16, [150, 0, 150, 0]);
        let pcm = half.decode_sector(&raw).unwrap().unwrap();
        assert_eq!(pcm.frames.len(), 4704);
        // The same source frames, decoded once each; the midpoints by hand,
        // rounded down (-3 and 0 make -2).
        assert_eq!(
            (midpoint(-3, 0), midpoint(3, 0), midpoint(-32768, 32767)),
            (-2, 1, -1)
        );
        let mut reference = XaDecoder::new(1, 16, [150, 0, 150, 0]);
        let mut expected = Vec::new();
        let mut previous = [0_i32; 2];
        for group in raw[24..2328].chunks_exact(128) {
            for pair in 0..4 {
                for sample in 0..28 {
                    let left = reference.decode_sample(group, pair * 2, sample, 0);
                    let right = reference.decode_sample(group, pair * 2 + 1, sample, 1);
                    let between = [
                        ((previous[0] + i32::from(left)).div_euclid(2)) as i16,
                        ((previous[1] + i32::from(right)).div_euclid(2)) as i16,
                    ];
                    reference.resample(between, &mut expected);
                    reference.resample([left, right], &mut expected);
                    previous = [left.into(), right.into()];
                }
            }
        }
        assert_eq!(pcm.frames, expected);
        assert_eq!(half, reference);
        // A second sector continues from the first one's last samples.
        let mut again = half.clone();
        let next = again.decode_sector(&raw).unwrap().unwrap();
        assert_ne!(next.frames, pcm.frames);
        assert_eq!(next.frames.len(), 4704);
        // A 37800-Hz sector of the same data yields half the frames.
        raw[19] = 1;
        raw[23] = 1;
        let mut full = XaDecoder::new(1, 16, [150, 0, 150, 0]);
        assert_eq!(
            full.decode_sector(&raw).unwrap().unwrap().frames.len(),
            2352
        );
        assert_eq!(full.history, half.history);
        assert_ne!(full.ring, [[0; 32]; 2]);
    }

    #[test]
    fn directory_namespaces_and_shifted_extents_are_checked() {
        let mut header = vec![0; 28];
        header[..4].copy_from_slice(b"AC3E");
        header[4..8].copy_from_slice(&1_u32.to_le_bytes());
        header[16..20].copy_from_slice(&1_u32.to_le_bytes());
        header[20..24].copy_from_slice(&0xf3800002_u32.to_le_bytes());
        header[24..28].copy_from_slice(&5_u32.to_le_bytes());
        let directory = StreamDirectory::parse(&header, 21).unwrap();
        assert_eq!(directory.disc(), 1);
        let entry = directory.entry(0x800).unwrap();
        assert_eq!(
            (
                entry.first_sector,
                entry.sector_count,
                entry.channel,
                entry.mode
            ),
            (5, 16, 16, 2)
        );
        assert_eq!(directory.entry(0x1800).unwrap().first_sector, 5);
        assert!(directory.entry(0).is_err());
        assert!(directory.entry(0x801).is_err());
        assert!(directory.entry(0x2800).is_err());
        assert!(StreamDirectory::parse(&header, 20).is_err());
        header[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(StreamDirectory::parse(&header, 21).is_err());
    }

    #[test]
    fn japanese_directories_have_a_disc_number_and_absent_entries() {
        let mut header = vec![0; 36];
        header[..4].copy_from_slice(b"AC3E");
        header[4..8].copy_from_slice(&2_u32.to_le_bytes());
        header[16..20].copy_from_slice(&2_u32.to_le_bytes());
        // Entry 0: on the other disc (bit 31 clear), its extent beyond this
        // disc's SPB; entry 1 present.
        header[20..24].copy_from_slice(&0x7380_0002_u32.to_le_bytes());
        header[24..28].copy_from_slice(&900_u32.to_le_bytes());
        header[28..32].copy_from_slice(&0xf3800002_u32.to_le_bytes());
        header[32..36].copy_from_slice(&5_u32.to_le_bytes());
        let directory = StreamDirectory::parse(&header, 21).unwrap();
        assert_eq!((directory.disc(), directory.len()), (2, 2));
        assert_eq!(
            directory.entry(0x800),
            Err(Error::Invalid("SPH resource is not on this disc"))
        );
        assert_eq!(directory.entry(0x801).unwrap().first_sector, 5);
        for kind in [0_u32, 3, 5] {
            header[4..8].copy_from_slice(&kind.to_le_bytes());
            assert!(StreamDirectory::parse(&header, 21).is_err(), "type {kind}");
        }
    }
}
