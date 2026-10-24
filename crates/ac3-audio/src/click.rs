use crate::gaussian::COEFFICIENTS;
use crate::{Bank, Error, MAX_BLOCKS, MAX_SAMPLES, Result};

/// The SPU output clock; source samples advance according to their pitch.
pub const OUTPUT_SAMPLE_RATE: u32 = 44_100;
const MAX_OUTPUT_FRAMES: usize = ((MAX_SAMPLES - 2) * 4096).div_ceil(0x1c8) + 6;

// The accepted profiles all have exponential attack and decreasing or frozen
// sustain. This is intentionally narrower
// than an arbitrary SPU envelope: release, key-off and increasing sustain are
// not reachable through this finite voice API.
#[derive(Debug)]
pub(crate) struct FiniteEnvelope {
    level: i32,
    ticks: u16,
    stage: u8,
    attack: u8,
    decay: u8,
    sustain_level: i32,
    sustain: u8,
    exponential_sustain: bool,
}

impl FiniteEnvelope {
    pub(crate) fn new(adsr1: u16, adsr2: u16) -> Self {
        Self {
            level: 0,
            ticks: 0,
            stage: 0,
            attack: ((adsr1 >> 8) & 127) as u8,
            decay: ((adsr1 >> 4) & 15) as u8 * 4,
            sustain_level: i32::from(adsr1 & 15),
            sustain: ((adsr2 >> 6) & 127) as u8,
            exponential_sustain: adsr2 & 0x8000 != 0,
        }
    }

    pub(crate) fn tick(&mut self) -> i32 {
        match self.stage {
            0 => {
                let rate = self.attack + if self.level >= 0x6000 { 8 } else { 0 };
                if self.step_due(rate) {
                    self.level = (self.level + rate_step(rate, true)).min(32767);
                }
                if self.level == 32767 {
                    self.stage = 1;
                }
            }
            1 => {
                if self.step_due(self.decay) {
                    self.level =
                        (self.level + ((rate_step(self.decay, false) * self.level) >> 15)).max(0);
                }
                // Compare the high four level bits after the exponential step.
                // Transition preserves any unfinished fraction counter.
                if self.level >> 11 <= self.sustain_level {
                    self.stage = 2;
                }
            }
            _ => {
                if self.sustain != 127 && self.step_due(self.sustain) {
                    let delta = rate_step(self.sustain, false);
                    self.level = (self.level
                        + if self.exponential_sustain {
                            (delta * self.level) >> 15
                        } else {
                            delta
                        })
                    .max(0);
                }
            }
        }
        self.level
    }

    fn step_due(&mut self, rate: u8) -> bool {
        let period = 1_u16 << (rate / 4).saturating_sub(11).min(15);
        self.ticks += 1;
        if self.ticks < period {
            false
        } else {
            self.ticks = 0;
            true
        }
    }
}

fn rate_step(rate: u8, increase: bool) -> i32 {
    let step = if increase {
        7 - i32::from(rate & 3)
    } else {
        -8 + i32::from(rate & 3)
    };
    step * (1 << 11_u8.saturating_sub(rate / 4))
}

#[cfg(test)]
mod envelope_tests {
    use super::FiniteEnvelope;

    #[test]
    fn slow_attack_preserves_knee_and_fraction_boundaries() {
        for (adsr1, ticks_to_knee, knee_level, period, delta) in [
            (0xafff, 6144, 24576, 4, 4),
            (0xb1ff, 8192, 24576, 8, 6),
            (0xb5ff, 16384, 24576, 16, 6),
            (0xacff, 3511, 24577, 4, 7),
        ] {
            let mut envelope = FiniteEnvelope::new(adsr1, 0x5fc9);
            for _ in 0..ticks_to_knee {
                envelope.tick();
            }
            assert_eq!(envelope.level, knee_level);
            for _ in 1..period {
                assert_eq!(envelope.tick(), knee_level);
            }
            assert_eq!(envelope.tick(), knee_level + delta);
        }
    }

    #[test]
    fn exponential_sustain_floors_negative_products_and_preserves_decay_tick() {
        let mut envelope = FiniteEnvelope::new(0xb1ff, 0xcc49);
        while envelope.stage == 0 {
            envelope.tick();
        }
        assert_eq!(envelope.tick(), 32767); // one decay tick, counter = 1
        assert_eq!(envelope.tick(), 32760); // -7 * 32767 >> 15 = -7
        assert_eq!(envelope.tick(), 32760);
        assert_eq!(envelope.tick(), 32753);
        envelope.level = 1;
        assert_eq!(envelope.tick(), 1);
        assert_eq!(envelope.tick(), 0); // arithmetic shift prevents a stuck tail
    }

    #[test]
    fn briefing_decay_steps_before_testing_sustain_level() {
        for (adsr1, adsr2, expected_first, threshold) in
            [(0xa764, 0x4c49, 32511, 4), (0x80a6, 0x4dc9, 32751, 6)]
        {
            let mut envelope = FiniteEnvelope::new(adsr1, adsr2);
            while envelope.stage == 0 {
                envelope.tick();
            }
            assert_eq!(envelope.tick(), expected_first);
            let mut previous = envelope.level;
            while envelope.stage == 1 {
                assert!(previous >> 11 > threshold);
                assert!(envelope.tick() < previous);
                previous = envelope.level;
            }
            assert!(envelope.level >> 11 <= threshold);
        }
    }
}

/// A completed stereo click at `OUTPUT_SAMPLE_RATE`, before any host resampling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StereoPcm {
    pub frames: Vec<[i16; 2]>,
}

impl StereoPcm {
    /// PCM16 little-endian RIFF/WAVE, preserving the 44100-Hz stereo frame clock.
    pub fn to_wav(&self) -> Result<Vec<u8>> {
        if self.frames.len() > MAX_OUTPUT_FRAMES {
            return Err(Error::LimitExceeded("stereo WAV frame count"));
        }
        let byte_count = (self.frames.len() * 4) as u32;
        let mut out = Vec::new();
        out.try_reserve_exact(byte_count as usize + 44)
            .map_err(|_| Error::Allocation)?;
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(byte_count + 36).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&2_u16.to_le_bytes());
        out.extend_from_slice(&OUTPUT_SAMPLE_RATE.to_le_bytes());
        out.extend_from_slice(&(OUTPUT_SAMPLE_RATE * 4).to_le_bytes());
        out.extend_from_slice(&4_u16.to_le_bytes());
        out.extend_from_slice(&16_u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&byte_count.to_le_bytes());
        for frame in &self.frames {
            for sample in frame {
                out.extend_from_slice(&sample.to_le_bytes());
            }
        }
        Ok(out)
    }
}

/// Finite voice for the verified menu and mission cue profiles.
///
/// This owns a decoded source and emits one stereo frame per 44100-Hz tick.
/// Recovered note pairs select a fixed pitch. Envelopes use exponential attack,
/// then sustain, linear decrease or exponential decrease. Fixed L/R
/// volume uses AC3's `(descriptor * sfx_gain) >> 8`; gain 208 is captured.
/// No key-off, loops, sweeps, effects, mixing, or host-device I/O are included.
/// The six silent startup ticks are the pinned Redux oracle's timing setting.
#[derive(Debug)]
pub struct MenuClickVoice {
    source: Vec<i16>,
    source_index: usize,
    window: [i32; 4],
    phase: u32,
    pitch_step: u32,
    startup: u8,
    envelope: FiniteEnvelope,
    volume: [i32; 2],
    remaining: usize,
}

impl MenuClickVoice {
    pub fn from_bank(bank: Bank<'_>, id: u16, sfx_gain: u16) -> Result<Self> {
        let (sample_note, adsr1, adsr2, pitch) = match id {
            0x3b => (0x4800, 0x80ff, 0x4c09, 0x800_u32),
            0x3c | 0x3d | 0x56 => (0x4800, 0x80ff, 0x5fc9, 0x800),
            0x58 => (0x5700, 0x80ff, 0x4e09, 0x35d),
            0x5a => (0x4800, 0xafff, 0x5fc9, 0x800),
            0x5b => (0x4639, 0xb1ff, 0xcc49, 0x937),
            0x5c => (0x4a00, 0x91ff, 0x5fc9, 0x721),
            0x62 => (0x5400, 0xb5ff, 0xdfc9, 0x400),
            0x63 => (0x6000, 0xacff, 0xce89, 0x200),
            0x64 => (0x6200, 0xa764, 0x4c49, 0x1c8),
            0x65 => (0x5e00, 0x80a6, 0x4dc9, 0x23f),
            0x66 => (0x5200, 0x80ff, 0x4e89, 0x47d),
            _ => {
                return Err(Error::UnsupportedVoice(
                    "only verified finite menu/mission cue IDs",
                ));
            }
        };
        if sfx_gain > 256 {
            return Err(Error::Invalid("SFX gain must be 0..=256"));
        }
        let sound = bank.sound(id)?;
        if (sound.note, sound.sample_note) != (0x3c00, sample_note) {
            return Err(Error::UnsupportedVoice(
                "note pair differs from this menu sound's verified profile",
            ));
        }
        if (sound.adsr1, sound.adsr2) != (adsr1, adsr2) {
            return Err(Error::UnsupportedVoice(
                "ADSR differs from this menu sound's verified profile",
            ));
        }
        let volume = [sound.volume_left, sound.volume_right]
            .map(|v| (i32::from(v) * i32::from(sfx_gain)) >> 8);
        if volume.iter().any(|v| !(-16384..=16383).contains(v)) {
            return Err(Error::UnsupportedVoice(
                "volume exceeds signed fixed SPU range",
            ));
        }
        let decoded = bank.decode_sound(id, MAX_BLOCKS)?;
        if decoded.pcm.end_flags != 1 {
            return Err(Error::UnsupportedVoice("click must end without repeat"));
        }
        let frames = ((decoded.pcm.samples.len() - 2) * 4096).div_ceil(pitch as usize) + 6;
        Ok(Self {
            source: decoded.pcm.samples,
            source_index: 0,
            window: [0; 4],
            phase: 3 * 65536,
            pitch_step: pitch * 16,
            startup: 6,
            envelope: FiniteEnvelope::new(adsr1, adsr2),
            volume,
            remaining: frames,
        })
    }
}

impl Iterator for MenuClickVoice {
    type Item = [i16; 2];

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        if self.startup != 0 {
            self.startup -= 1;
            return Some([0, 0]);
        }
        while self.phase >= 65536 {
            self.window.rotate_left(1);
            self.window[3] = i32::from(self.source[self.source_index]);
            self.source_index += 1;
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
        Some(self.volume.map(|volume| {
            // Signed division intentionally truncates toward zero.
            ((enveloped * volume) / 16384).clamp(-32767, 32767) as i16
        }))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for MenuClickVoice {}
impl std::iter::FusedIterator for MenuClickVoice {}

/// Render one verified-profile menu click; gain 208 matches original captures.
pub fn render_menu_click(bank: Bank<'_>, id: u16, sfx_gain: u16) -> Result<StereoPcm> {
    let voice = MenuClickVoice::from_bank(bank, id, sfx_gain)?;
    let mut frames = Vec::new();
    frames
        .try_reserve_exact(voice.len())
        .map_err(|_| Error::Allocation)?;
    frames.extend(voice);
    Ok(StereoPcm { frames })
}
