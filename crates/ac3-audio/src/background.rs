use crate::click::FiniteEnvelope;
use crate::gaussian::COEFFICIENTS;
use crate::{Bank, Error, MAX_BLOCKS, OUTPUT_SAMPLE_RATE, Result, StereoPcm};

/// Maximum allocation for one explicit background rendering request: 30 seconds.
pub const MAX_BACKGROUND_FRAMES: usize = OUTPUT_SAMPLE_RATE as usize * 30;

/// One known mission background voice (IDs 0x5d..0x61), starting at key-on.
///
/// Encoded bytes and one 28-sample block are retained; predictor, interpolation
/// and envelope history continue across loop branches. Each `tick` advances one
/// 44100-Hz sample even at zero gain. This has no device, clock, mixer or release
/// policy. The caller supplies explicit hardware volume changes and scheduling.
#[derive(Debug)]
pub struct BackgroundVoice {
    encoded: Vec<u8>,
    next_block: usize,
    loop_block: usize,
    decoded: [i32; 28],
    next_sample: usize,
    history: [i32; 2],
    window: [i32; 4],
    phase: u32,
    pitch_step: u32,
    startup: u8,
    envelope: FiniteEnvelope,
    base_volume: [i32; 2],
    volume: [i32; 2],
}

impl BackgroundVoice {
    pub fn from_bank(bank: Bank<'_>, id: u16, sfx_gain: u16) -> Result<Self> {
        let (sample_note, adsr1, pitch) = match id {
            0x5d => (0x5401, 0x80ff, 0x400),
            0x5e => (0x4a03, 0x80ff, 0x723),
            0x5f => (0x4002, 0x80ff, 0xcb6),
            0x60 => (0x5c01, 0xc2ff, 0x285),
            0x61 => (0x4c01, 0xc4ff, 0x65a),
            _ => return Err(Error::UnsupportedVoice("only background IDs 0x5d..0x61")),
        };
        if sfx_gain > 256 {
            return Err(Error::Invalid("SFX gain must be 0..=256"));
        }
        let sound = bank.sound(id)?;
        if (
            sound.note,
            sound.sample_note,
            sound.adsr1,
            sound.adsr2,
            sound.voice_hint,
        ) != (0x3c00, sample_note, adsr1, 0x5fc9, (id - 0x5d) as u8)
        {
            return Err(Error::UnsupportedVoice(
                "background descriptor differs from known profile",
            ));
        }
        let base_volume = [sound.volume_left, sound.volume_right]
            .map(|v| (i32::from(v) * i32::from(sfx_gain)) >> 8);
        if base_volume.iter().any(|v| !(-16384..=16383).contains(v)) {
            return Err(Error::UnsupportedVoice(
                "volume exceeds signed fixed SPU range",
            ));
        }
        let validated = bank.decode_sound(id, MAX_BLOCKS)?.pcm;
        if validated.end_flags != 3 {
            return Err(Error::UnsupportedVoice(
                "background must end with repeat flag 3",
            ));
        }
        let loop_block = validated.loop_start.ok_or(Error::UnsupportedVoice(
            "background requires an in-stream loop marker",
        ))? / 28
            * 16;
        let encoded_bytes = validated.blocks_consumed() * 16;
        let mut encoded = Vec::new();
        encoded
            .try_reserve_exact(encoded_bytes)
            .map_err(|_| Error::Allocation)?;
        encoded.extend_from_slice(&bank.encoded_sound(id)?[..encoded_bytes]);
        Ok(Self {
            encoded,
            next_block: 0,
            loop_block,
            decoded: [0; 28],
            next_sample: 28,
            history: [0; 2],
            window: [0; 4],
            phase: 3 * 65536,
            pitch_step: pitch * 16,
            startup: 6,
            envelope: FiniteEnvelope::new(adsr1, 0x5fc9),
            base_volume,
            volume: base_volume,
        })
    }

    /// Apply the original byte gain (128 = full) to the already SFX-scaled
    /// descriptor volume. This models a submitted register update, not the
    /// game's target/fade bookkeeping or its decision whether to submit one.
    pub fn set_gain(&mut self, gain: u8) -> Result<()> {
        if gain > 128 {
            return Err(Error::Invalid("background gain must be 0..=128"));
        }
        self.volume = self.base_volume.map(|v| (v * i32::from(gain)) >> 7);
        Ok(())
    }

    /// Advance exactly one output frame without allocating.
    pub fn tick(&mut self) -> [i16; 2] {
        self.tick_contribution()
            .map(|sample| sample.clamp(-32767, 32767) as i16)
    }

    /// Advance one frame and return the integer contribution before final mix
    /// saturation. Sum all voices first, then clamp the complete stereo mix.
    pub fn tick_contribution(&mut self) -> [i32; 2] {
        if self.startup != 0 {
            self.startup -= 1;
            return [0, 0];
        }
        while self.phase >= 65536 {
            self.window.rotate_left(1);
            self.window[3] = self.source_sample();
            self.phase -= 65536;
        }
        let phase = (self.phase >> 8) as usize;
        let coefficients = [
            COEFFICIENTS[255 - phase],
            COEFFICIENTS[511 - phase],
            COEFFICIENTS[256 + phase],
            COEFFICIENTS[phase],
        ];
        let interpolated = coefficients
            .into_iter()
            .zip(self.window)
            .map(|(coefficient, sample)| (coefficient * sample) >> 14)
            .sum::<i32>()
            >> 1;
        let enveloped = (interpolated * self.envelope.tick()) >> 15;
        self.phase += self.pitch_step;
        self.volume.map(|volume| (enveloped * volume) / 16384)
    }

    /// Apply one signed fixed-volume register at an explicit sample boundary.
    /// Sweep modes are not accepted. Useful for an independently timed original
    /// register trace; ordinary gain updates should use `set_gain`.
    pub fn set_channel_volume(&mut self, channel: usize, volume: i16) -> Result<()> {
        if !(-16384..=16383).contains(&volume) {
            return Err(Error::UnsupportedVoice(
                "volume exceeds signed fixed SPU range",
            ));
        }
        *self
            .volume
            .get_mut(channel)
            .ok_or(Error::Invalid("stereo channel must be 0 or 1"))? = i32::from(volume);
        Ok(())
    }

    /// Render a bounded segment from the current state; subsequent calls continue
    /// it. Use `tick` and explicit register changes for a changing gain timeline.
    pub fn render_frames(&mut self, count: usize) -> Result<StereoPcm> {
        if count > MAX_BACKGROUND_FRAMES {
            return Err(Error::LimitExceeded("background render frames"));
        }
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(count)
            .map_err(|_| Error::Allocation)?;
        frames.extend((0..count).map(|_| self.tick()));
        Ok(StereoPcm { frames })
    }

    fn source_sample(&mut self) -> i32 {
        if self.next_sample == 28 {
            let block = &self.encoded[self.next_block..self.next_block + 16];
            const FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];
            let (a, b) = FILTERS[usize::from(block[0] >> 4)];
            let shift = block[0] & 15;
            for index in 0..28 {
                let nibble = (block[2 + index / 2] >> (4 * (index & 1))) & 15;
                let signed = i32::from((nibble << 4) as i8) >> 4;
                let sample = (((signed << 12) >> shift)
                    + ((self.history[0] * a) >> 6)
                    + ((self.history[1] * b) >> 6))
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX));
                self.decoded[index] = sample;
                self.history = [sample, self.history[0]];
            }
            self.next_block = if block[1] & 1 != 0 {
                self.loop_block
            } else {
                self.next_block + 16
            };
            self.next_sample = 0;
        }
        let sample = self.decoded[self.next_sample];
        self.next_sample += 1;
        sample
    }
}
