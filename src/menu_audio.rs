//! Original-bank menu cues and optional host playback. Headless operation never
//! opens an audio device. Unsupported profiles stay visible in the run report.
use crate::mission_audio::{MissionAudio, MissionAudioEvent, NOMINAL_PAL_FRAME_SAMPLES};
use crate::stream_audio::StreamAudio;
use crate::{Result, assets::GameData, sha256};
use ac3_audio::{Bank, OUTPUT_SAMPLE_RATE, StereoPcm, render_menu_click};
use ac3_formats::dot1;
use rodio::{MixerDeviceSink, Player, buffer::SamplesBuffer, nz};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Seek, SeekFrom, Write},
    path::Path,
};

struct Cue {
    pcm: StereoPcm,
    voice_hint: u8,
    digest: String,
}

pub struct MenuAudio<'a> {
    banks: [Bank<'a>; 2],
    cues: BTreeMap<u16, std::result::Result<Cue, String>>,
    output: Option<MixerDeviceSink>,
    players: BTreeMap<u8, Player>,
    background: Option<MissionAudio<'a>>,
    background_player: Option<Player>,
    background_file: Option<fs::File>,
    background_hash: Sha256,
    background_samples: u64,
    stream: StreamAudio,
    stream_player: Option<Player>,
    stream_file: Option<fs::File>,
    stream_hash: Sha256,
    stream_samples: u64,
    pub device_status: Value,
}
impl<'a> MenuAudio<'a> {
    pub fn new(data: &'a GameData, headless: bool) -> Result<Self> {
        let banks = [0, 69].map(|id| -> Result<Bank<'a>> {
            Ok(Bank::parse(dot1::parse(data.resource(id)?)?.child(4)?)?)
        });
        let [first, second] = banks;
        let (output, device_status) = if headless {
            (None, json!({"status":"disabled_headless"}))
        } else {
            match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(mut output) => {
                    output.log_on_drop(false);
                    (Some(output), json!({"status":"open"}))
                }
                Err(error) => (
                    None,
                    json!({"status":"unavailable","reason":error.to_string()}),
                ),
            }
        };
        Ok(Self {
            banks: [first?, second?],
            cues: BTreeMap::new(),
            output,
            players: BTreeMap::new(),
            background: None,
            background_player: None,
            background_file: None,
            background_hash: Sha256::new(),
            background_samples: 0,
            stream: StreamAudio::new(data.disc_directory().to_path_buf()),
            stream_player: None,
            stream_file: None,
            stream_hash: Sha256::new(),
            stream_samples: 0,
            device_status,
        })
    }

    pub fn request(&mut self, id: u16, directory: Option<&Path>) -> Result<Value> {
        let cue = self.cues.entry(id).or_insert_with(|| {
            let bank = self
                .banks
                .iter()
                .find(|b| b.sound(id).is_ok())
                .ok_or_else(|| format!("sound {id:#x} is not in the loaded sound banks"))?;
            let sound = bank.sound(id).map_err(|e| e.to_string())?;
            let pcm = render_menu_click(*bank, id, 208).map_err(|e| e.to_string())?;
            let raw: Vec<u8> = pcm
                .frames
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            Ok(Cue {
                pcm,
                voice_hint: sound.voice_hint,
                digest: sha256(&raw),
            })
        });
        let cue = match cue {
            Ok(cue) => cue,
            Err(reason) => return Ok(json!({"id":id,"status":"unsupported","reason":reason})),
        };
        if let Some(directory) = directory {
            // A mission bank can replace a menu ID with different sample data.
            // Content-addressed exports retain both versions and avoid silently
            // reusing an older WAV when an output directory is used again.
            let path = directory
                .join("sounds")
                .join(format!("{id:02x}-{}.wav", &cue.digest[..16]));
            if !path.exists() {
                fs::create_dir_all(path.parent().unwrap())?;
                fs::write(path, cue.pcm.to_wav()?)?;
            }
        }
        self.players.retain(|_, p| !p.empty());
        let mut status = "rendered";
        if let Some(output) = &self.output {
            let voice = if cue.voice_hint < 24 {
                Some(cue.voice_hint)
            } else {
                (12..24).find(|v| !self.players.contains_key(v))
            };
            if let Some(voice) = voice {
                // Replacing a fixed voice drops and stops its previous source.
                // Original 8002D910 scans allocated voices12..23 and drops
                // requests when all are busy. Host completion is asynchronous.
                self.players.remove(&voice);
                let player = Player::connect_new(output.mixer());
                let samples = cue
                    .pcm
                    .frames
                    .iter()
                    .flatten()
                    .map(|&v| f32::from(v) / 32768.0)
                    .collect::<Vec<_>>();
                player.append(SamplesBuffer::new(nz!(2), nz!(44100), samples));
                self.players.insert(voice, player);
                status = "submitted";
            } else {
                status = "voice_pool_busy";
            }
        }
        Ok(
            json!({"id":id,"status":status,"sample_rate":OUTPUT_SAMPLE_RATE,"frames":cue.pcm.frames.len(),"pcm_sha256":cue.digest}),
        )
    }

    /// Original common-mission load replaces the menu bank's descriptor range.
    /// Cached successes and failures in that range must both be invalidated.
    pub fn load_mission_bank(&mut self, data: &'a GameData) -> Result<()> {
        let bank = Bank::parse(dot1::parse(data.resource(72)?)?.child(9)?)?;
        let first = bank.first_sound_id();
        let last = first + bank.sound_count();
        self.cues.retain(|id, _| !(first..last).contains(id));
        self.banks[1] = bank;
        if self.background.is_none() {
            self.background = Some(MissionAudio::new(bank, 208)?);
        }
        Ok(())
    }

    /// Resident 2D6CC keys off all SPU voices. Host cues are pre-rendered, so
    /// stop their players here; exact envelope release remains in the SPU oracle.
    pub fn stop_resident_voices(&mut self) {
        self.players.clear();
        self.background_player = None;
        self.background = None;
    }

    /// Native 50-Hz scheduling. Original CPU-store timing remains a separate
    /// oracle input; the host intentionally groups writes at block boundaries.
    pub fn background_tick(
        &mut self,
        events: &[MissionAudioEvent],
        directory: Option<&Path>,
    ) -> Result<Option<Value>> {
        let Some(background) = &mut self.background else {
            if !events.is_empty() {
                return Err("background event before mission bank load".into());
            }
            return Ok(None);
        };
        let block = background.frame(events, NOMINAL_PAL_FRAME_SAMPLES)?;
        let raw: Vec<u8> = block
            .pcm
            .frames
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        self.background_samples += block.pcm.frames.len() as u64;
        self.background_hash.update(&raw);
        if let Some(directory) = directory {
            if self.background_file.is_none() {
                let path = directory.join("sounds/mission-background.wav");
                fs::create_dir_all(path.parent().unwrap())?;
                let mut file = fs::File::create(path)?;
                file.write_all(&StereoPcm { frames: Vec::new() }.to_wav()?)?;
                self.background_file = Some(file);
            }
            self.background_file.as_mut().unwrap().write_all(&raw)?;
        }
        if let Some(output) = &self.output {
            let player = self
                .background_player
                .get_or_insert_with(|| Player::connect_new(output.mixer()));
            player.append(SamplesBuffer::new(
                nz!(2),
                nz!(44100),
                block
                    .pcm
                    .frames
                    .iter()
                    .flatten()
                    .map(|&v| f32::from(v) / 32768.0)
                    .collect::<Vec<_>>(),
            ));
        }
        let writes: Vec<_> = block
            .worker
            .writes
            .iter()
            .map(|w| json!({"voice":w.voice,"gain":w.gain,"volume":w.volume}))
            .collect();
        Ok(Some(
            json!({"first_sample":block.first_sample,"frames":block.pcm.frames.len(),"worker":block.worker.update,
            "started":block.worker.started,"volume_writes":writes,"pcm_sha256":sha256(&raw)}),
        ))
    }

    pub fn finish_background(&mut self) -> Result<Value> {
        if let Some(mut file) = self.background_file.take() {
            let bytes = u32::try_from(
                self.background_samples
                    .checked_mul(4)
                    .ok_or("WAV length overflow")?,
            )?;
            let riff = bytes
                .checked_add(36)
                .ok_or("background WAV exceeds RIFF limit")?;
            file.seek(SeekFrom::Start(4))?;
            file.write_all(&riff.to_le_bytes())?;
            file.seek(SeekFrom::Start(40))?;
            file.write_all(&bytes.to_le_bytes())?;
            file.flush()?;
        }
        Ok(
            json!({"sample_rate":44100,"frames":self.background_samples,"pcm_sha256":format!("{:x}",self.background_hash.clone().finalize()),
            "scope":"Five native persistent mission voices at882 samples per50-Hz host update; callback events take effect at the next worker. Original intra-worker SPU-store timing and mixing with finite cues are not reproduced by the host playback layer."}),
        )
    }

    pub fn stream_request(&mut self, request: &Value) -> Result<Value> {
        match request["action"].as_str() {
            Some("start") => {
                let resource = u16::try_from(
                    request["resource"]
                        .as_u64()
                        .ok_or("invalid stream resource")?,
                )?;
                let result = self.stream.start(resource)?;
                self.stream_player = None;
                Ok(result)
            }
            Some("stop") => {
                self.stream_player = None;
                Ok(self.stream.stop())
            }
            _ => Err("invalid stream action".into()),
        }
    }

    pub fn stream_tick(&mut self, directory: Option<&Path>) -> Result<Option<Value>> {
        let Some(pcm) = self.stream.render_frames(NOMINAL_PAL_FRAME_SAMPLES)? else {
            return Ok(None);
        };
        let first_sample = self.stream_samples;
        let raw: Vec<u8> = pcm
            .frames
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        self.stream_samples += pcm.frames.len() as u64;
        self.stream_hash.update(&raw);
        if let Some(directory) = directory {
            if self.stream_file.is_none() {
                let path = directory.join("sounds/mission-streams.wav");
                fs::create_dir_all(path.parent().unwrap())?;
                let mut file = fs::File::create(path)?;
                file.write_all(&StereoPcm { frames: Vec::new() }.to_wav()?)?;
                self.stream_file = Some(file);
            }
            self.stream_file.as_mut().unwrap().write_all(&raw)?;
        }
        if let Some(output) = &self.output {
            let player = self
                .stream_player
                .get_or_insert_with(|| Player::connect_new(output.mixer()));
            player.append(SamplesBuffer::new(
                nz!(2),
                nz!(44100),
                pcm.frames
                    .iter()
                    .flatten()
                    .map(|&v| f32::from(v) / 32768.0)
                    .collect::<Vec<_>>(),
            ));
        }
        Ok(Some(
            json!({"first_sample":first_sample,"frames":pcm.frames.len(),"pcm_sha256":sha256(&raw),"stream":self.stream.status()}),
        ))
    }

    pub fn finish_stream(&mut self) -> Result<Value> {
        if let Some(mut file) = self.stream_file.take() {
            let bytes = u32::try_from(
                self.stream_samples
                    .checked_mul(4)
                    .ok_or("WAV length overflow")?,
            )?;
            let riff = bytes
                .checked_add(36)
                .ok_or("stream WAV exceeds RIFF limit")?;
            file.seek(SeekFrom::Start(4))?;
            file.write_all(&riff.to_le_bytes())?;
            file.seek(SeekFrom::Start(40))?;
            file.write_all(&bytes.to_le_bytes())?;
            file.flush()?;
        }
        Ok(
            json!({"sample_rate":44100,"frames":self.stream_samples,"pcm_sha256":format!("{:x}",self.stream_hash.clone().finalize()),
            "stream":self.stream.status(),"scope":"Native briefing/aircraft XA decoding in882-sample host blocks. Export concatenates active blocks; per-frame traces retain start/stop timing. Immediate start/stop; original CD latency and final SPU mixing with voices are not reproduced."}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires original menu and mission banks"]
    fn mission_bank_replaces_previously_cached_sound() {
        let root = std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").unwrap());
        let data = GameData::load(&root.join("extracted/disc")).unwrap();
        let mut audio = MenuAudio::new(&data, true).unwrap();
        let menu = audio.request(0x56, None).unwrap();
        let confirm = audio.request(0x3c, None).unwrap();
        assert_eq!(menu["frames"], 191914);
        audio.load_mission_bank(&data).unwrap();
        let mission = audio.request(0x56, None).unwrap();
        assert_eq!(mission["status"], "rendered");
        assert_eq!(mission["frames"], 7786);
        assert_ne!(menu["pcm_sha256"], mission["pcm_sha256"]);
        assert_eq!(audio.request(0x3c, None).unwrap(), confirm);
        assert_eq!(audio.device_status["status"], "disabled_headless");
    }
}
