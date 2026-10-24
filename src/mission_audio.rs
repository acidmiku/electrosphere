//! Native mission background voices, original gain targets and an explicit
//! caller-owned PCM clock. No emulator snapshots or host audio device are used.
//!
//! The natural guest sound worker runs before the scene callback. `frame`
//! therefore consumes the preceding callback's queued events, advances the
//! worker, renders a caller-sized block, then queues this callback's events.
//! Its register changes share a block boundary; original intra-worker CPU
//! timing is not implied. Use the lower-level methods for explicit boundaries.
use crate::Result;
use ac3_audio::{BackgroundVoice, Bank, MAX_BACKGROUND_FRAMES, StereoPcm, VoiceGain};

/// A native host running exactly 50 updates/second produces this many samples
/// per update. This is a host cadence, not the pinned emulator's field length.
pub const NOMINAL_PAL_FRAME_SAMPLES: usize = 882;
/// The pinned Redux PAL counter uses 314 integer scanline periods of 2157 cycles.
pub const REDUX_PAL_FIELD_CYCLES: u64 = 677_298;

/// Pinned original SPU writes apply to the first output sample strictly after
/// their CPU cycle. This conversion alone does not schedule native scene code.
pub const fn sample_after_cycle(cycle: u64) -> u64 {
    cycle / 768 + 1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissionAudioEvent {
    /// 800E8008: key-on IDs 5D..61, then set all five target gains to zero.
    InitBackgroundVoices,
    /// 800E7CE8 / 800E7B50(1): target 100 percent over 60 worker updates.
    BeginHubBackgroundFade,
    /// 800E7B50(3): target 0 percent over 60 worker updates.
    BeginLaunchBackgroundFade,
    /// 800E7B50(2): target 100 percent over 10 worker updates.
    ResumeBackgroundFast,
    /// Briefing initialization through 800E7A18(30,10).
    BeginBriefingBackgroundFade,
}

/// One submitted volume change; skipped original writes are absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackgroundVolumeWrite {
    pub voice: u8,
    pub gain: u8,
    pub volume: [i16; 2],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundWorkerUpdate {
    pub update: u64,
    pub started: bool,
    pub writes: Vec<BackgroundVolumeWrite>,
}

#[derive(Debug)]
pub struct MissionAudioFrame {
    pub first_sample: u64,
    pub worker: BackgroundWorkerUpdate,
    pub pcm: StereoPcm,
}

/// Five persistent mission voices only. The already-emitted PlaySound 62/63
/// effects belong to the finite-cue host; fade events here do not duplicate them.
pub struct MissionAudio<'a> {
    bank: Bank<'a>,
    sfx_gain: u16,
    base_volume: [[i32; 2]; 5],
    voices: Option<[BackgroundVoice; 5]>,
    pending: Option<[BackgroundVoice; 5]>,
    gain: [VoiceGain; 5],
    queued: Vec<MissionAudioEvent>,
    sample: u64,
    updates: u64,
}

impl<'a> MissionAudio<'a> {
    /// Construct after the mission bank (resource 72/child 9) has been loaded.
    /// Voices remain absent until the original initialization event arrives.
    pub fn new(bank: Bank<'a>, sfx_gain: u16) -> Result<Self> {
        if sfx_gain > 256 {
            return Err("SFX gain must be 0..=256".into());
        }
        let mut base_volume = [[0; 2]; 5];
        for (index, volume) in base_volume.iter_mut().enumerate() {
            let sound = bank.sound(0x5d + index as u16)?;
            *volume = [sound.volume_left, sound.volume_right]
                .map(|v| (i32::from(v) * i32::from(sfx_gain)) >> 8);
            if volume.iter().any(|v| !(-16384..=16383).contains(v)) {
                return Err("background volume exceeds signed fixed SPU range".into());
            }
        }
        Ok(Self {
            bank,
            sfx_gain,
            base_volume,
            voices: None,
            pending: None,
            gain: [VoiceGain::new(); 5],
            queued: Vec::new(),
            sample: 0,
            updates: 0,
        })
    }

    pub const fn sample_position(&self) -> u64 {
        self.sample
    }
    pub fn gain_state(&self) -> [VoiceGain; 5] {
        self.gain
    }
    pub fn initialized(&self) -> bool {
        self.voices.is_some() || self.pending.is_some()
    }

    /// Apply a scene event now; its gain/key-on effects are submitted by the
    /// next `sound_worker_update`. This method itself emits no PCM.
    pub fn apply(&mut self, event: MissionAudioEvent) -> Result<()> {
        if event == MissionAudioEvent::InitBackgroundVoices {
            let make = |id| BackgroundVoice::from_bank(self.bank, id, self.sfx_gain);
            let voices = [
                make(0x5d)?,
                make(0x5e)?,
                make(0x5f)?,
                make(0x60)?,
                make(0x61)?,
            ];
            self.pending = Some(voices);
            self.gain = [VoiceGain::new(); 5];
            for gain in &mut self.gain {
                gain.fade_to(0, 0)?;
            }
            return Ok(());
        }
        if !self.initialized() {
            return Err("background fade before initialization".into());
        }
        let (percent, duration) = match event {
            MissionAudioEvent::BeginHubBackgroundFade => (100, 60),
            MissionAudioEvent::BeginLaunchBackgroundFade => (0, 60),
            MissionAudioEvent::ResumeBackgroundFast => (100, 10),
            MissionAudioEvent::BeginBriefingBackgroundFade => (30, 10),
            MissionAudioEvent::InitBackgroundVoices => unreachable!(),
        };
        for gain in &mut self.gain {
            gain.fade_to(percent, duration)?;
        }
        Ok(())
    }

    /// Advance one original gain-worker update. This groups its five volume
    /// changes and key-on at one PCM boundary; CPU-instruction spacing is not
    /// reconstructed by this per-frame API.
    pub fn sound_worker_update(&mut self) -> Result<BackgroundWorkerUpdate> {
        let update = self.updates;
        self.updates = self
            .updates
            .checked_add(1)
            .ok_or("audio worker counter overflow")?;
        let started = self.pending.is_some();
        if let Some(voices) = self.pending.take() {
            self.voices = Some(voices);
        }
        let mut writes = Vec::with_capacity(5);
        if let Some(voices) = &mut self.voices {
            for (index, voice) in voices.iter_mut().enumerate() {
                if let Some(gain) = self.gain[index].advance(false) {
                    voice.set_gain(gain)?;
                    writes.push(BackgroundVolumeWrite {
                        voice: index as u8,
                        gain,
                        volume: self.base_volume[index]
                            .map(|v| ((v * i32::from(gain)) >> 7) as i16),
                    });
                }
            }
        }
        Ok(BackgroundWorkerUpdate {
            update,
            started,
            writes,
        })
    }

    /// Render the five background contributions, then saturate the combined
    /// stereo sum. Silent initialized voices still advance. The caller owns
    /// timing; this method neither advances gain updates nor opens a device.
    pub fn render_frames(&mut self, count: usize) -> Result<StereoPcm> {
        if count > MAX_BACKGROUND_FRAMES {
            return Err("background frame allocation exceeds 30 seconds".into());
        }
        let next_sample = self
            .sample
            .checked_add(count as u64)
            .ok_or("audio sample counter overflow")?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(count)?;
        for _ in 0..count {
            let mut mix = [0_i32; 2];
            if let Some(voices) = &mut self.voices {
                for voice in voices {
                    let contribution = voice.tick_contribution();
                    mix[0] += contribution[0];
                    mix[1] += contribution[1];
                }
            }
            frames.push(mix.map(|v| v.clamp(-32767, 32767) as i16));
        }
        self.sample = next_sample;
        Ok(StereoPcm { frames })
    }

    /// Convenience for the native frontend: prior callback events -> worker ->
    /// PCM block -> queue current callback events for the next update. At a
    /// nominal 50-Hz host cadence pass 882 samples, explicitly accepting that
    /// this does not reproduce the emulator's fractional field clock.
    pub fn frame(
        &mut self,
        callback_events: &[MissionAudioEvent],
        count: usize,
    ) -> Result<MissionAudioFrame> {
        if callback_events.len() > 64 {
            return Err("too many mission audio events in one frame".into());
        }
        if count > MAX_BACKGROUND_FRAMES {
            return Err("background frame allocation exceeds 30 seconds".into());
        }
        let queued = std::mem::take(&mut self.queued);
        for event in queued {
            self.apply(event)?;
        }
        let worker = self.sound_worker_update()?;
        let first_sample = self.sample;
        let pcm = self.render_frames(count)?;
        self.queued.extend_from_slice(callback_events);
        Ok(MissionAudioFrame {
            first_sample,
            worker,
            pcm,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_after_cycle_boundary_and_pal_fraction_are_explicit() {
        assert_eq!([0, 767, 768, 1535].map(sample_after_cycle), [1, 1, 2, 2]);
        let counts: Vec<_> = (0..128)
            .map(|n| {
                sample_after_cycle((n + 1) * REDUX_PAL_FIELD_CYCLES)
                    - sample_after_cycle(n * REDUX_PAL_FIELD_CYCLES)
            })
            .collect();
        assert!(counts.iter().all(|v| [881, 882].contains(v)));
        assert_eq!(counts.iter().sum::<u64>(), 112883);
        assert_ne!(
            counts.iter().sum::<u64>(),
            128 * NOMINAL_PAL_FRAME_SAMPLES as u64
        );
    }

    #[test]
    #[ignore = "requires AC3_DATA_DIR and original mission bank"]
    fn queued_scene_events_preserve_silent_phase_and_original_gain_boundary() {
        let root = std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
        let bytes = std::fs::read(root.join("extracted/archives/bpb_0072_0009.bin")).unwrap();
        let mut audio = MissionAudio::new(Bank::parse(&bytes).unwrap(), 208).unwrap();
        let first = audio
            .frame(&[MissionAudioEvent::InitBackgroundVoices], 882)
            .unwrap();
        assert!(!first.worker.started);
        let second = audio
            .frame(&[MissionAudioEvent::BeginHubBackgroundFade], 882)
            .unwrap();
        assert!(second.worker.started);
        assert_eq!(second.worker.writes.len(), 5);
        assert!(second.pcm.frames.iter().all(|v| *v == [0, 0]));
        let mut last_written = 0;
        for _ in 0..60 {
            let block = audio.frame(&[], 882).unwrap();
            if let Some(write) = block.worker.writes.first() {
                last_written = write.gain;
            }
        }
        assert_eq!(last_written, 125);
        assert!(
            audio
                .gain_state()
                .iter()
                .all(|v| v.current() == 128 && v.remaining() == 0)
        );
        audio
            .apply(MissionAudioEvent::BeginBriefingBackgroundFade)
            .unwrap();
        for step in 0..10 {
            let update = audio.sound_worker_update().unwrap();
            // (30 * 128) / 100 = 38. The exact nine-point descent from 128
            // crosses no full-gain skipped-write boundary.
            assert_eq!(update.writes.len(), 5);
            assert!(
                update
                    .writes
                    .iter()
                    .all(|write| write.gain == 119 - step * 9)
            );
        }
        assert!(
            audio
                .gain_state()
                .iter()
                .all(|v| v.current() == 38 && v.remaining() == 0)
        );
        audio
            .frame(&[MissionAudioEvent::BeginLaunchBackgroundFade], 0)
            .unwrap();
        for _ in 0..60 {
            audio.frame(&[], 882).unwrap();
        }
        assert!(
            audio
                .frame(&[], 882)
                .unwrap()
                .pcm
                .frames
                .iter()
                .all(|v| *v == [0, 0])
        );
    }
}
