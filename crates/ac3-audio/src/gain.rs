use crate::{Error, Result};

/// Original per-voice target/current gain bookkeeping, updated once by the
/// game's sound worker (`8002DB18` and `8002DDCC`), independently of PCM ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceGain {
    current: u8,
    target: u8,
    remaining: u16,
}

impl Default for VoiceGain {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceGain {
    /// Original key-on initializes both byte gains to 128 and the timer to zero.
    pub const fn new() -> Self {
        Self {
            current: 128,
            target: 128,
            remaining: 0,
        }
    }
    pub const fn current(&self) -> u8 {
        self.current
    }
    pub const fn target(&self) -> u8 {
        self.target
    }
    pub const fn remaining(&self) -> u16 {
        self.remaining
    }

    /// Set the target using `(percent * 128) / 100`. Current gain and hardware
    /// volume do not change until the next original worker update. Only the
    /// nonnegative percentage/duration domain used by these mission paths is
    /// accepted; durations above signed 16-bit range are rejected.
    pub fn fade_to(&mut self, percent: u8, updates: u16) -> Result<()> {
        if percent > 100 || updates > i16::MAX as u16 {
            return Err(Error::Invalid(
                "voice fade requires percent 0..100 and updates 0..32767",
            ));
        }
        self.target = (u16::from(percent) * 128 / 100) as u8;
        self.remaining = updates;
        Ok(())
    }

    /// Advance one game sound-worker update. Return the gain to submit to the
    /// voice, or None when the original does not write its volume. An already
    /// dirty volume flag is represented by `force_write`.
    ///
    /// In particular, reaching full gain 128 does not itself mark volume dirty:
    /// a fade may leave the hardware at its previous gain until another write.
    pub fn advance(&mut self, force_write: bool) -> Option<u8> {
        if self.remaining == 0 {
            self.current = self.target;
        } else {
            let delta =
                (i32::from(self.target) - i32::from(self.current)) / i32::from(self.remaining);
            self.current = (i32::from(self.current) + delta) as u8;
            self.remaining -= 1;
        }
        (force_write || self.current != 128).then_some(self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::VoiceGain;

    #[test]
    fn full_gain_boundary_preserves_original_last_write() {
        let mut gain = VoiceGain::new();
        gain.fade_to(0, 0).unwrap();
        assert_eq!(gain.advance(false), Some(0));
        gain.fade_to(100, 60).unwrap();
        let mut applied = 0;
        for _ in 0..60 {
            if let Some(value) = gain.advance(false) {
                applied = value;
            }
        }
        assert_eq!((gain.current(), gain.remaining(), applied), (128, 0, 125));
        assert_eq!(gain.advance(false), None);
        assert_eq!(gain.advance(true), Some(128));
    }

    #[test]
    fn descending_division_truncates_toward_zero_and_invalid_targets_are_atomic() {
        let mut gain = VoiceGain::new();
        gain.fade_to(0, 60).unwrap();
        assert_eq!(gain.advance(false), Some(126)); // -128/60 = -2
        let before = gain;
        assert!(gain.fade_to(101, 0).is_err());
        assert!(gain.fade_to(100, 32768).is_err());
        assert_eq!(gain, before);
        gain.fade_to(50, 0).unwrap();
        assert_eq!(gain.advance(false), Some(64));
        gain.fade_to(100, 0).unwrap();
        assert_eq!(gain.advance(false), None);
        assert_eq!(gain.current(), 128);
    }
}
