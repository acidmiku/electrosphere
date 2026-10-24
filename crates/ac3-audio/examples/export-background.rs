//! Bounded headless segment from one original looping mission voice.
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let id = u16::from_str_radix(
        args.next()
            .ok_or("sound ID required")?
            .trim_start_matches("0x"),
        16,
    )?;
    let frames: usize = args.next().ok_or("frame count required")?.parse()?;
    let output = args.next().ok_or("WAV path required")?;
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let root = PathBuf::from(env::var_os("AC3_DATA_DIR").ok_or("set AC3_DATA_DIR")?);
    let bytes = fs::read(root.join("extracted/archives/bpb_0072_0009.bin"))?;
    let mut voice =
        ac3_audio::BackgroundVoice::from_bank(ac3_audio::Bank::parse(&bytes)?, id, 208)?;
    let pcm = voice.render_frames(frames)?;
    fs::write(output, pcm.to_wav()?)?;
    Ok(())
}
