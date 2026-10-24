//! Recorded register schedules are independent validation inputs only.
use ac3_audio::{BackgroundVoice, Bank, MenuClickVoice, StereoPcm};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub fn replay(bytes: [&[u8]; 3], text: &str, frames: usize) -> Result<StereoPcm> {
    if frames > 44100 * 30 {
        return Err("at most 30 seconds per comparison".into());
    }
    let banks = [
        Bank::parse(bytes[0])?,
        Bank::parse(bytes[1])?,
        Bank::parse(bytes[2])?,
    ];
    let mut events = Vec::new();
    for line in text.lines().skip(1) {
        if events.len() == 100000 {
            return Err("event count limit".into());
        }
        let mut row = [0_i64; 7];
        let mut columns = line.split(',');
        for value in &mut row {
            *value = columns.next().ok_or("expected seven columns")?.parse()?;
        }
        if columns.next().is_some() {
            return Err("expected seven columns".into());
        }
        if events
            .last()
            .is_some_and(|last: &[i64; 7]| last[0] > row[0])
        {
            return Err("events must be chronological".into());
        }
        events.push(row);
    }
    let mut background: [Option<BackgroundVoice>; 5] = std::array::from_fn(|_| None);
    let mut finite: [Option<MenuClickVoice>; 16] = std::array::from_fn(|_| None);
    let mut volumes = [[0_i16; 2]; 16];
    let mut result = Vec::with_capacity(frames);
    let mut cursor = 0;
    let first = events.first().ok_or("empty schedule")?[0].min(0);
    if first < -(44100 * 30) {
        return Err("pre-roll limit".into());
    }
    for sample in first..frames as i64 {
        while cursor < events.len() && events[cursor][0] == sample {
            let [_, kind, voice, channel, value, id, bank] = events[cursor];
            let voice = usize::try_from(voice)?;
            if voice >= 16 {
                return Err("voice index limit".into());
            }
            match kind {
                0 => {
                    let channel = usize::try_from(channel)?;
                    if channel >= 2 {
                        return Err("channel index limit".into());
                    }
                    volumes[voice][channel] = i16::try_from(value)?;
                    if let Some(Some(voice)) = background.get_mut(voice) {
                        voice.set_channel_volume(channel, i16::try_from(value)?)?;
                    }
                }
                1 => {
                    let bank = banks[match bank {
                        0 => 0,
                        69 => 1,
                        72 => 2,
                        _ => return Err("unknown bank".into()),
                    }];
                    let id = u16::try_from(id)?;
                    if (0x5d..=0x61).contains(&id) {
                        let mut bg = BackgroundVoice::from_bank(bank, id, 208)?;
                        for (channel, volume) in volumes[voice].iter().copied().enumerate() {
                            bg.set_channel_volume(channel, volume)?;
                        }
                        *background.get_mut(voice).ok_or("background voice index")? = Some(bg);
                    } else {
                        let sound = bank.sound(id)?;
                        let expected = [sound.volume_left, sound.volume_right]
                            .map(|v| ((i32::from(v) * 208) >> 8) as i16);
                        if volumes[voice] != expected {
                            return Err(format!(
                                "finite {id:x} volume mismatch: {:?} != {expected:?}",
                                volumes[voice]
                            )
                            .into());
                        }
                        finite[voice] = Some(MenuClickVoice::from_bank(bank, id, 208)?);
                    }
                }
                2 => {
                    if background.get(voice).is_some_and(Option::is_some) {
                        return Err("background key-off requires release validation".into());
                    }
                    if finite[voice].as_ref().is_some_and(|voice| voice.len() > 0) {
                        return Err("finite key-off before end requires release validation".into());
                    }
                    finite[voice] = None;
                }
                _ => return Err("unknown event kind".into()),
            }
            cursor += 1;
        }
        let mut mix = [0_i32; 2];
        for voice in background.iter_mut().flatten() {
            let pcm = voice.tick_contribution();
            mix[0] += pcm[0];
            mix[1] += pcm[1];
        }
        for voice in finite.iter_mut().flatten() {
            if let Some(pcm) = voice.next() {
                mix[0] += i32::from(pcm[0]);
                mix[1] += i32::from(pcm[1]);
            }
        }
        if sample >= 0 {
            result.push(mix.map(|v| v.clamp(-32767, 32767) as i16));
        }
    }
    Ok(StereoPcm { frames: result })
}
