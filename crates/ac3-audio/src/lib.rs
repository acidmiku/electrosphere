//! AC3's custom sound bank and raw SPU ADPCM sample decoding.
//!
//! Bank fields follow resident functions 0x8002D544, 0x8002E0F8 and
//! 0x8002DCA8. Decoding stops at the first end flag, retaining loop metadata.
//! Known finite cues and five looping background voices apply verified pitch,
//! interpolation, envelope and volume profiles. Gain updates use a separate
//! explicit game-worker clock. The XA module decodes checked SPH sectors for
//! the verified stereo stream profile; host scheduling, reverb and combined
//! device mixing remain outside this library. Raw-sample WAV rates are caller supplied.

use std::fmt;
mod background;
mod click;
mod gain;
mod gaussian;
pub mod xa;
mod xa_coefficients;
pub use background::{BackgroundVoice, MAX_BACKGROUND_FRAMES};
pub use click::{MenuClickVoice, OUTPUT_SAMPLE_RATE, StereoPcm, render_menu_click};
pub use gain::VoiceGain;

pub const SPU_RAM_BYTES: usize = 512 * 1024;
pub const BLOCK_BYTES: usize = 16;
pub const SAMPLES_PER_BLOCK: usize = 28;
/// At most one complete traversal of the physical SPU memory.
pub const MAX_BLOCKS: usize = SPU_RAM_BYTES / BLOCK_BYTES;
pub const MAX_SAMPLES: usize = MAX_BLOCKS * SAMPLES_PER_BLOCK;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Truncated(&'static str),
    Invalid(&'static str),
    LimitExceeded(&'static str),
    UnknownSound(u16),
    UnsupportedAdpcmHeader { block: usize, header: u8 },
    UnsupportedAdpcmFlags { block: usize, flags: u8 },
    UnsupportedVoice(&'static str),
    MissingEnd,
    Allocation,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated(s) => write!(f, "truncated {s}"),
            Self::Invalid(s) => write!(f, "invalid audio input: {s}"),
            Self::LimitExceeded(s) => write!(f, "audio limit exceeded: {s}"),
            Self::UnknownSound(id) => write!(f, "sound {id:#x} is not in this bank"),
            Self::UnsupportedAdpcmHeader { block, header } => {
                write!(f, "unsupported ADPCM header {header:#04x} at block {block}")
            }
            Self::UnsupportedAdpcmFlags { block, flags } => {
                write!(f, "unsupported ADPCM flags {flags:#04x} at block {block}")
            }
            Self::UnsupportedVoice(reason) => write!(f, "unsupported voice profile: {reason}"),
            Self::MissingEnd => f.write_str("ADPCM stream has no end flag"),
            Self::Allocation => f.write_str("audio allocation failed"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// One 20-byte AC3 descriptor. Notes are SDK note/fraction values, not Hz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sound {
    pub id: u16,
    /// 0..23 is a fixed voice; 0xff asks AC3's voice allocator.
    pub voice_hint: u8,
    /// Preserved bytes +1..+3, whose complete semantics remain unresolved.
    pub attributes: [u8; 3],
    pub volume_left: i16,
    pub volume_right: i16,
    pub note: u16,
    pub sample_note: u16,
    pub adsr1: u16,
    pub adsr2: u16,
    /// Byte address passed to the SDK, not the eight-byte hardware unit.
    pub spu_address: u32,
}

/// Borrowed, validated view of a custom bank (not a standard VAB header).
#[derive(Debug, Clone, Copy)]
pub struct Bank<'a> {
    first_id: u16,
    count: u16,
    spu_address: u32,
    descriptors: &'a [u8],
    samples: &'a [u8],
    consumed: usize,
}

impl<'a> Bank<'a> {
    /// Validate header/table extents, sample addresses and physical RAM bounds.
    /// Trailing container padding is allowed; `bytes_consumed` excludes it.
    pub fn parse(data: &'a [u8]) -> Result<Self> {
        let header = data
            .get(..12)
            .ok_or(Error::Truncated("sound bank header"))?;
        let first_id = u16::from(header[0]);
        let count = u16::from(header[1]);
        if first_id + count > 256 {
            return Err(Error::Invalid("sound ID range exceeds one byte"));
        }
        let words = i16::from_le_bytes([header[2], header[3]]);
        if words < 0 {
            return Err(Error::Invalid("negative sample data offset"));
        }
        let offset = words as usize * 4;
        let table_end = 12 + usize::from(count) * 20;
        if offset < table_end {
            return Err(Error::Invalid("sample data overlaps descriptor table"));
        }
        let spu_address = word(header, 4);
        let byte_count = word(header, 8) as usize;
        if spu_address as usize > SPU_RAM_BYTES || byte_count > SPU_RAM_BYTES - spu_address as usize
        {
            return Err(Error::Invalid("sample transfer exceeds SPU RAM"));
        }
        if !spu_address.is_multiple_of(8) || !byte_count.is_multiple_of(BLOCK_BYTES) {
            return Err(Error::Invalid("sample transfer alignment"));
        }
        // offset <= 131068 and byte_count <= 524288, so this sum cannot overflow.
        let consumed = offset + byte_count;
        let descriptors = data
            .get(12..table_end)
            .ok_or(Error::Truncated("sound descriptors"))?;
        let samples = data
            .get(offset..consumed)
            .ok_or(Error::Truncated("sound sample data"))?;
        let bank = Self {
            first_id,
            count,
            spu_address,
            descriptors,
            samples,
            consumed,
        };
        for index in 0..count {
            let sound = bank.sound(first_id + index)?;
            if sound.voice_hint >= 24 && sound.voice_hint != 0xff {
                return Err(Error::Invalid("voice hint"));
            }
            if sound.spu_address < spu_address
                || !sound.spu_address.is_multiple_of(16)
                || sound.spu_address as usize > SPU_RAM_BYTES - BLOCK_BYTES
                || sound.spu_address as usize + BLOCK_BYTES > spu_address as usize + byte_count
            {
                return Err(Error::Invalid("sound start outside sample transfer"));
            }
        }
        Ok(bank)
    }

    pub fn first_sound_id(&self) -> u16 {
        self.first_id
    }
    pub fn sound_count(&self) -> u16 {
        self.count
    }
    pub fn spu_address(&self) -> u32 {
        self.spu_address
    }
    pub fn sample_bytes(&self) -> &'a [u8] {
        self.samples
    }
    pub fn bytes_consumed(&self) -> usize {
        self.consumed
    }

    pub fn sound(&self, id: u16) -> Result<Sound> {
        if id < self.first_id || id >= self.first_id + self.count {
            return Err(Error::UnknownSound(id));
        }
        let offset = usize::from(id - self.first_id) * 20;
        let d = &self.descriptors[offset..offset + 20];
        Ok(Sound {
            id,
            voice_hint: d[0],
            attributes: [d[1], d[2], d[3]],
            volume_left: half(d, 4) as i16,
            volume_right: half(d, 6) as i16,
            note: half(d, 8),
            sample_note: half(d, 10),
            adsr1: half(d, 12),
            adsr2: half(d, 14),
            spu_address: word(d, 16),
        })
    }

    /// Encoded data up to the next distinct sound address, or the bank end.
    /// Shared sample addresses are valid. No ordering by sound ID is assumed.
    pub fn encoded_sound(&self, id: u16) -> Result<&'a [u8]> {
        let sound = self.sound(id)?;
        let start = (sound.spu_address - self.spu_address) as usize;
        let end = self
            .descriptors
            .chunks_exact(20)
            .map(|d| word(d, 16))
            .filter(|address| *address > sound.spu_address)
            .min()
            .map_or(self.samples.len(), |address| {
                (address - self.spu_address) as usize
            });
        Ok(&self.samples[start..end])
    }

    pub fn decode_sound(&self, id: u16, max_blocks: usize) -> Result<DecodedSound> {
        Ok(DecodedSound {
            sound: self.sound(id)?,
            pcm: decode_adpcm(self.encoded_sound(id)?, max_blocks)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSound {
    pub sound: Sound,
    pub pcm: Adpcm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adpcm {
    /// Raw decoded samples before interpolation, pitch, volume or ADSR.
    pub samples: Vec<i16>,
    /// Every consumed block's flag byte, including the final block.
    pub block_flags: Vec<u8>,
    /// Latest loop-start marker in sample units; None means not set in-stream.
    pub loop_start: Option<usize>,
    pub end_flags: u8,
    pub repeat: bool,
}
impl Adpcm {
    pub fn blocks_consumed(&self) -> usize {
        self.block_flags.len()
    }
}

/// Decode through the first end-marked block, starting with zero history.
/// The end block contributes all 28 samples. Loops are recorded, never expanded.
/// Only filters 0..4, shifts 0..12 and flag bits 0..2 are accepted.
/// Separate arithmetic shifts of predictor terms match the documented SPU
/// recurrence; this intentionally does not apply the CD-XA rounding variant.
pub fn decode_adpcm(data: &[u8], max_blocks: usize) -> Result<Adpcm> {
    if max_blocks == 0 || max_blocks > MAX_BLOCKS {
        return Err(Error::Invalid("ADPCM block budget must be 1..=32768"));
    }
    let mut blocks = 0;
    loop {
        if blocks == max_blocks {
            return Err(Error::LimitExceeded("ADPCM block budget"));
        }
        let offset = blocks * BLOCK_BYTES;
        if offset == data.len() {
            return Err(Error::MissingEnd);
        }
        let block = data
            .get(offset..offset + BLOCK_BYTES)
            .ok_or(Error::Truncated("ADPCM block"))?;
        if block[0] >> 4 > 4 || block[0] & 15 > 12 {
            return Err(Error::UnsupportedAdpcmHeader {
                block: blocks,
                header: block[0],
            });
        }
        if block[1] & !7 != 0 {
            return Err(Error::UnsupportedAdpcmFlags {
                block: blocks,
                flags: block[1],
            });
        }
        blocks += 1;
        if block[1] & 1 != 0 {
            break;
        }
    }
    let mut samples = Vec::new();
    samples
        .try_reserve_exact(blocks * SAMPLES_PER_BLOCK)
        .map_err(|_| Error::Allocation)?;
    let mut block_flags = Vec::new();
    block_flags
        .try_reserve_exact(blocks)
        .map_err(|_| Error::Allocation)?;
    let mut history = [0_i32; 2];
    let mut loop_start = None;
    // Mathematical filter coefficients, not sample or game data.
    const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];
    for block in data[..blocks * BLOCK_BYTES].chunks_exact(BLOCK_BYTES) {
        let (a, b) = FILTERS[usize::from(block[0] >> 4)];
        let shift = block[0] & 15;
        block_flags.push(block[1]);
        if block[1] & 4 != 0 {
            loop_start = Some(samples.len());
        }
        for byte in &block[2..] {
            for nibble in [byte & 15, byte >> 4] {
                let signed = i32::from((nibble << 4) as i8) >> 4;
                let prediction = ((history[0] * a) >> 6) + ((history[1] * b) >> 6);
                let sample = ((signed << 12) >> shift) + prediction;
                let sample = sample.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                samples.push(sample as i16);
                history = [sample, history[0]];
            }
        }
    }
    let end_flags = block_flags[blocks - 1];
    Ok(Adpcm {
        samples,
        block_flags,
        loop_start,
        end_flags,
        repeat: end_flags & 2 != 0,
    })
}

/// Export raw samples as little-endian mono PCM16 RIFF/WAVE, with no processing.
/// The explicit rate is an export choice, not a recovered SPU pitch conversion.
/// Loop information remains in `Adpcm`; this simple WAV writer does not add smpl.
pub fn wav_pcm16_mono(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>> {
    if sample_rate == 0 || sample_rate > 192_000 {
        return Err(Error::Invalid("WAV rate must be 1..=192000 Hz"));
    }
    if samples.len() > MAX_SAMPLES {
        return Err(Error::LimitExceeded("WAV sample count"));
    }
    let byte_count = (samples.len() * 2) as u32;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_count as usize + 44)
        .map_err(|_| Error::Allocation)?;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(byte_count + 36).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&byte_count.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(bytes)
}

fn half(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
