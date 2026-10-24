//! Independent oracle renderer. Captured schedules are only validation inputs;
//! the native mission runtime never reads them.
#[path = "../tests/support/scene.rs"]
mod scene;
use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let events_path = args.next().ok_or("events CSV required")?;
    let frames: usize = args.next().ok_or("output frame count required")?.parse()?;
    let output = args.next().ok_or("output WAV required")?;
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let root = PathBuf::from(env::var_os("AC3_DATA_DIR").ok_or("set AC3_DATA_DIR")?);
    let bytes = [
        "bpb_0000_0004.bin",
        "bpb_0069_0004.bin",
        "bpb_0072_0009.bin",
    ]
    .map(|name| fs::read(root.join("extracted/archives").join(name)));
    let [resident, menu, mission] = bytes;
    let (resident, menu, mission) = (resident?, menu?, mission?);
    let text = fs::read_to_string(events_path)?;
    let pcm = scene::replay([&resident, &menu, &mission], &text, frames)?;
    fs::write(output, pcm.to_wav()?)?;
    Ok(())
}
