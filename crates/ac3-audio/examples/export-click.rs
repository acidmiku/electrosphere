//! Headless export from a local recovered bank; no audio device is opened.
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let id = args
        .next()
        .ok_or("usage: export-click <sound-id-hex> <output.wav>")?;
    let id = u16::from_str_radix(
        id.to_str()
            .ok_or("sound ID must be UTF-8")?
            .trim_start_matches("0x"),
        16,
    )?;
    let output = PathBuf::from(args.next().ok_or("output WAV path required")?);
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let root = PathBuf::from(
        env::var_os("AC3_DATA_DIR").ok_or("set AC3_DATA_DIR to the local game workspace")?,
    );
    let bank_path = if id == 0x58 {
        "extracted/archives/bpb_0069_0004.bin"
    } else if matches!(
        id,
        0x56 | 0x5a | 0x5b | 0x5c | 0x62 | 0x63 | 0x64 | 0x65 | 0x66
    ) {
        "extracted/archives/bpb_0072_0009.bin"
    } else {
        "extracted/archives/bpb_0000_0004.bin"
    };
    let bytes = fs::read(root.join(bank_path))?;
    let pcm = ac3_audio::render_menu_click(ac3_audio::Bank::parse(&bytes)?, id, 208)?;
    fs::write(&output, pcm.to_wav()?)?;
    println!(
        "sound {id:#04x}: {} stereo frames at {} Hz -> {}",
        pcm.frames.len(),
        ac3_audio::OUTPUT_SAMPLE_RATE,
        output.display()
    );
    Ok(())
}
